//! Supervises the embedded Cloudflare tunnel.
//!
//! The naive version of this — spawn `cloudflared`, restart it when the process
//! exits — misses the failure mode that actually takes this machine off the
//! internet. cloudflared does not exit when its connections to the Cloudflare
//! edge collapse; it sits there retrying, and from the outside the tunnel host
//! returns 502 while `sc query` still reports the service Running. A supervisor
//! watching only for process exit sees a perfectly healthy tunnel throughout.
//!
//! So this module watches the connection count instead of the process, via
//! cloudflared's own metrics endpoint, and restarts on "0 connections for a
//! minute" rather than on exit.
//!
//! It also pins down three things the default invocation left loose:
//!
//! * **Logs.** As a service there is no console, so cloudflared's stdout went
//!   nowhere at all. Every diagnosis of a past outage started from no evidence.
//!   Now `--logfile` puts it on disk next to the binary.
//! * **Auto-update.** cloudflared self-updates by rewriting its own exe and
//!   exiting, expecting a service manager to bring it back. Ours is a child
//!   process in a Job Object, and the exe it rewrites is the one we extracted,
//!   so the update raced our own restart logic. `--no-autoupdate` — updates
//!   arrive with the app instead.
//! * **Transport.** QUIC (UDP/7844) is cloudflared's default and is the usual
//!   cause of a tunnel that flaps without ever exiting: consumer NAT and
//!   mobile-adjacent links drop long-lived UDP flows, and each drop is a
//!   reconnect. When QUIC proves unstable here we fall back to HTTP/2 over TCP,
//!   which survives those links.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::{error, info, warn};

const CLOUDFLARED_EXE: &[u8] = include_bytes!("../bin/cloudflared.exe");

// ── Supervisor tuning ────────────────────────────────────────────────────────

/// Exponential backoff bounds between respawns after a short-lived run.
const MIN_BACKOFF_SECS: u64 = 5;
const MAX_BACKOFF_SECS: u64 = 300;
/// A run shorter than this counts as a failure rather than a real session.
const HEALTHY_RUN_SECS: u64 = 60;
/// Restarting a tunnel we killed for being unhealthy should not back off to
/// five minutes — the edge may well be reachable again on the next attempt.
const HEALTH_MAX_BACKOFF_SECS: u64 = 60;

/// How long a freshly spawned cloudflared gets to establish connections before
/// its health counts against it. Four HA connections over a slow link take a
/// while; killing it early would turn a slow start into a restart loop.
const HEALTH_GRACE_SECS: u64 = 45;
/// Interval between `/ready` polls once the grace period is over.
const HEALTH_POLL_SECS: u64 = 15;
/// Consecutive bad polls before we call the tunnel down. At 15 s that is a
/// minute of zero connections — long enough to ride out a normal reconnect.
const UNHEALTHY_STRIKES: u32 = 4;
/// If the metrics endpoint never answers at all, health monitoring disables
/// itself after this many polls rather than killing a tunnel it cannot see.
const METRICS_UNREACHABLE_GIVEUP: u32 = 8;

/// Window over which QUIC reconnects are counted.
const CHURN_WINDOW_SECS: u64 = 300;
/// Closed QUIC connections within one window that mark the transport as
/// unusable. Steady state is zero; a handful during a network change is
/// normal. Dozens means the UDP path is not holding.
const CHURN_LIMIT: u64 = 40;

/// Unhealthy QUIC events tolerated before falling back to HTTP/2 for the rest
/// of this service run. Two, so a single network blip does not cost the better
/// transport, but a genuinely broken UDP path is abandoned quickly.
const QUIC_STRIKES_BEFORE_FALLBACK: u32 = 2;

/// Cap on the cloudflared log before it is rolled to `.1`.
const LOG_ROLL_BYTES: u64 = 8 * 1024 * 1024;

/// Cloudflare's edge-discovery hostname, dialled over TCP to tell a broken
/// transport apart from a broken uplink. cloudflared already resolves and
/// connects to this host, so probing it introduces no new third party and
/// nothing leaves the machine that was not already leaving it.
const EDGE_PROBE_ADDR: &str = "region1.v2.argotunnel.com:7844";
const EDGE_PROBE_TIMEOUT_SECS: u64 = 8;

/// Consecutive restarts caused by an unreachable uplink before we write the
/// "this is your internet, not the tunnel" note to the error log.
const LINK_DOWN_REPORT_AFTER: u32 = 3;

// ── Transport selection ──────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Quic,
    Http2,
}

impl Protocol {
    fn as_str(self) -> &'static str {
        match self {
            Protocol::Quic => "quic",
            Protocol::Http2 => "http2",
        }
    }
}

/// What `tunnel_protocol` in config.toml asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProtocolPref {
    /// Start on QUIC, fall back to HTTP/2 if it proves unstable. The default.
    Auto,
    /// Use exactly this, never switch.
    Pinned(Protocol),
}

impl ProtocolPref {
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "quic" => ProtocolPref::Pinned(Protocol::Quic),
            "http2" | "h2" => ProtocolPref::Pinned(Protocol::Http2),
            _ => ProtocolPref::Auto,
        }
    }

    fn initial(self) -> Protocol {
        match self {
            ProtocolPref::Auto => Protocol::Quic,
            ProtocolPref::Pinned(p) => p,
        }
    }
}

// ── Published health, read by GET /status ────────────────────────────────────

/// A point-in-time view of the tunnel, surfaced on `/status` so "is the tunnel
/// up?" is answerable from the API itself rather than from the process list.
#[derive(Clone, serde::Serialize)]
pub struct TunnelSnapshot {
    /// A cloudflared process is currently running.
    pub running: bool,
    /// It has at least one live connection to the Cloudflare edge. This is the
    /// field that answers "can the outside world reach this machine".
    pub healthy: bool,
    pub protocol: &'static str,
    pub ready_connections: u32,
    /// Respawns since the service started. Steadily climbing means the link,
    /// the token, or the transport is bad.
    pub restarts: u32,
    /// QUIC connections torn down since cloudflared last started. High and
    /// rising is the signature of an unstable UDP path.
    pub quic_closed_connections: u64,
    /// Set once an unstable QUIC path has forced the HTTP/2 fallback.
    pub fell_back_to_http2: bool,
    /// Whether the metrics endpoint is answering. When false, `healthy` is a
    /// guess based on the process being alive.
    pub metrics_reachable: bool,
    pub metrics_port: u16,
    /// Whether the Cloudflare edge was TCP-reachable the last time the tunnel
    /// went down. `false` means the outage was the uplink, not the tunnel —
    /// the single most useful thing to know before debugging anything here.
    /// `None` until the first outage.
    pub network_reachable: Option<bool>,
    pub last_event: String,
}

impl Default for TunnelSnapshot {
    fn default() -> Self {
        Self {
            running: false,
            healthy: false,
            protocol: "quic",
            ready_connections: 0,
            restarts: 0,
            quic_closed_connections: 0,
            fell_back_to_http2: false,
            metrics_reachable: false,
            metrics_port: 0,
            network_reachable: None,
            last_event: "not started".into(),
        }
    }
}

static STATUS: OnceLock<Mutex<TunnelSnapshot>> = OnceLock::new();

fn status_cell() -> &'static Mutex<TunnelSnapshot> {
    STATUS.get_or_init(|| Mutex::new(TunnelSnapshot::default()))
}

/// Current tunnel health. Never blocks on the tunnel itself.
pub fn snapshot() -> TunnelSnapshot {
    status_cell()
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
}

fn update(f: impl FnOnce(&mut TunnelSnapshot)) {
    if let Ok(mut s) = status_cell().lock() {
        f(&mut s);
    }
}

// ── Supervisor ───────────────────────────────────────────────────────────────

/// Why a health watcher asked for a restart.
enum Unhealthy {
    /// cloudflared is running but has no connection to the edge.
    NoConnections,
    /// The transport is reconnecting so often it cannot carry traffic.
    Churn(u64),
}

impl Unhealthy {
    fn describe(&self) -> String {
        match self {
            Unhealthy::NoConnections => format!(
                "no edge connections for {}s",
                HEALTH_POLL_SECS * UNHEALTHY_STRIKES as u64
            ),
            Unhealthy::Churn(n) => format!(
                "{} QUIC reconnects in {}s — the UDP path is not holding",
                n, CHURN_WINDOW_SECS
            ),
        }
    }
}

/// How a supervised run ended.
enum Outcome {
    /// The process exited on its own.
    Exited,
    /// We killed it because the tunnel was down.
    HealthKill(Unhealthy),
    /// The service is stopping.
    Shutdown,
}

pub async fn start(
    token: String,
    protocol_pref: Option<String>,
    metrics_port: u16,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let exe_path = match extract_exe() {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to extract cloudflared.exe: {}", e);
            update(|s| s.last_event = format!("could not extract cloudflared.exe: {e}"));
            return;
        }
    };
    let log_path = exe_path.with_file_name("cloudflared.log");

    // Create Job Object once — kept alive for entire process lifetime.
    // Any cloudflared process assigned to it is auto-killed when we exit.
    #[cfg(windows)]
    let job_handle = create_job_object();

    let pref = ProtocolPref::parse(protocol_pref.as_deref().unwrap_or("auto"));
    let mut protocol = pref.initial();
    let mut quic_strikes: u32 = 0;
    let mut link_down_restarts: u32 = 0;

    info!(
        "Starting Cloudflare tunnel (protocol: {}, metrics: 127.0.0.1:{}, log: {})",
        protocol.as_str(),
        metrics_port,
        log_path.display()
    );
    update(|s| {
        s.protocol = protocol.as_str();
        s.metrics_port = metrics_port;
        s.last_event = "starting".into();
    });

    let mut backoff_secs = MIN_BACKOFF_SECS;
    // Separate, gentler ramp for restarts we triggered ourselves.
    let mut health_backoff_secs = MIN_BACKOFF_SECS;
    let mut consecutive_failures: u32 = 0;

    loop {
        roll_log_if_large(&log_path);
        let started = Instant::now();

        // Global flags have to precede the `run` subcommand — cloudflared
        // rejects `tunnel run --no-autoupdate` outright. Verified against the
        // bundled build; keep this ordering if you add flags.
        let args: Vec<String> = vec![
            "tunnel".into(),
            "--no-autoupdate".into(),
            "--protocol".into(),
            protocol.as_str().into(),
            "--metrics".into(),
            format!("127.0.0.1:{metrics_port}"),
            "--loglevel".into(),
            "info".into(),
            "--logfile".into(),
            log_path.to_string_lossy().into_owned(),
            "run".into(),
            "--token".into(),
            token.clone(),
        ];

        let outcome = match tokio::process::Command::new(&exe_path)
            .args(&args)
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .spawn()
        {
            Ok(mut child) => {
                let pid = child.id().unwrap_or(0);
                info!("Tunnel started (pid: {}, protocol: {})", pid, protocol.as_str());
                update(|s| {
                    s.running = true;
                    s.protocol = protocol.as_str();
                    s.quic_closed_connections = 0;
                    s.last_event = format!("started on {}", protocol.as_str());
                });

                // Assign the cloudflared child process to our Job Object.
                // When our process exits for ANY reason, Windows kills it.
                #[cfg(windows)]
                if job_handle != 0 && pid != 0 {
                    assign_process_to_job(pid, job_handle);
                }

                let outcome = tokio::select! {
                    status = child.wait() => {
                        match status {
                            Ok(s) => warn!("Tunnel exited: {}. Restarting...", s),
                            Err(e) => error!("Tunnel error: {}. Restarting...", e),
                        }
                        Outcome::Exited
                    }
                    reason = watch_health(metrics_port, protocol) => {
                        warn!("Tunnel unhealthy: {}. Restarting cloudflared...", reason.describe());
                        let _ = child.kill().await;
                        Outcome::HealthKill(reason)
                    }
                    _ = shutdown_rx.changed() => {
                        info!("Shutdown — killing tunnel...");
                        let _ = child.kill().await;
                        Outcome::Shutdown
                    }
                };

                update(|s| {
                    s.running = false;
                    s.healthy = false;
                    s.ready_connections = 0;
                });
                outcome
            }
            Err(e) => {
                error!("Failed to spawn cloudflared: {}", e);
                update(|s| {
                    s.running = false;
                    s.healthy = false;
                    s.last_event = format!("spawn failed: {e}");
                });
                Outcome::Exited
            }
        };

        if matches!(outcome, Outcome::Shutdown) {
            update(|s| s.last_event = "stopped".into());
            return;
        }

        update(|s| s.restarts += 1);

        // A tunnel we killed for being unhealthy is a different situation from
        // one that exited instantly: the process was fine, something about the
        // network was not. Retry it sooner, and work out what to blame.
        let health_kill = matches!(outcome, Outcome::HealthKill(_));
        if let Outcome::HealthKill(reason) = &outcome {
            update(|s| s.last_event = format!("restarted — {}", reason.describe()));
        }

        // Before blaming the transport, find out whether there was a network to
        // transport over. A flapping uplink takes down QUIC and HTTP/2 alike,
        // and without this check every ISP outage would be scored against QUIC
        // until the tunnel fell back to a transport that was never the problem
        // — and stayed there.
        if health_kill {
            let link_up = link_reachable().await;
            update(|s| s.network_reachable = Some(link_up));

            if !link_up {
                link_down_restarts += 1;
                info!(
                    "Cloudflare edge is not TCP-reachable either — treating this as an uplink \
                     outage rather than a tunnel fault (not counted against {})",
                    protocol.as_str()
                );
                update(|s| {
                    s.last_event =
                        "restarted — the network path to Cloudflare is down; this is the \
                         internet connection, not the tunnel"
                            .into()
                });

                // Say it once, plainly, where someone will find it. Every
                // symptom above this line looks like a broken tunnel.
                if link_down_restarts == LINK_DOWN_REPORT_AFTER {
                    let msg = format!(
                        "The Cloudflare tunnel has restarted {} times because this machine could \
                         not reach the internet — the Cloudflare edge was unreachable over plain \
                         TCP at each failure, so the tunnel and its token are fine. Check the \
                         router/ISP uplink: if the local gateway pings cleanly while anything \
                         beyond it times out, the fault is upstream of this machine and nothing \
                         in this app can work around it.",
                        link_down_restarts
                    );
                    warn!("{}", msg);
                    crate::log_error_to_file(&msg);
                }
            } else {
                link_down_restarts = 0;
            }

            // Only a failure with a working path to Cloudflare is evidence
            // against QUIC.
            if link_up && protocol == Protocol::Quic {
                quic_strikes += 1;

                if pref == ProtocolPref::Auto && quic_strikes >= QUIC_STRIKES_BEFORE_FALLBACK {
                    protocol = Protocol::Http2;
                    let msg = format!(
                        "Cloudflare tunnel failed {} times on QUIC while the Cloudflare edge was \
                         reachable over TCP, so the uplink is fine and the transport is not. \
                         Falling back to HTTP/2 over TCP for the rest of this run — QUIC uses \
                         UDP/7844, which many consumer routers and ISPs drop for long-lived \
                         flows. Set tunnel_protocol in config.toml to pin this permanently \
                         (\"http2\" to keep it, \"quic\" to force QUIC back).",
                        quic_strikes
                    );
                    warn!("{}", msg);
                    crate::log_error_to_file(&msg);
                    update(|s| {
                        s.fell_back_to_http2 = true;
                        s.protocol = Protocol::Http2.as_str();
                    });
                }
            }
        }

        let wait_secs = if health_kill {
            // The process itself was fine; the network path was not. Ramp, but
            // gently and to a low ceiling — the edge may already be reachable
            // again, and a five-minute wait would strand the machine offline
            // long after the link came back.
            consecutive_failures = 0;
            backoff_secs = MIN_BACKOFF_SECS;
            health_backoff_secs = (health_backoff_secs * 2).min(HEALTH_MAX_BACKOFF_SECS);
            health_backoff_secs
        } else if started.elapsed().as_secs() >= HEALTHY_RUN_SECS {
            // The tunnel ran long enough to have been working — treat this as a
            // transient drop and retry promptly.
            consecutive_failures = 0;
            backoff_secs = MIN_BACKOFF_SECS;
            health_backoff_secs = MIN_BACKOFF_SECS;
            backoff_secs
        } else {
            consecutive_failures += 1;
            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);

            // Surface the likely cause once, rather than silently looping.
            if consecutive_failures == 3 {
                let msg = format!(
                    "Cloudflare tunnel has failed {} times in a row, each within {}s of starting. \
                     The tunnel_token in config.toml is most likely invalid, revoked, or for a \
                     deleted tunnel. Backing off to {}s between attempts. cloudflared's own \
                     reason is in {}.",
                    consecutive_failures,
                    HEALTHY_RUN_SECS,
                    backoff_secs,
                    log_path.display()
                );
                error!("{}", msg);
                crate::log_error_to_file(&msg);
            }
            backoff_secs
        };

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(wait_secs)) => {
                info!("Reconnecting tunnel (attempt after {}s backoff)...", wait_secs);
            }
            _ = shutdown_rx.changed() => {
                info!("Shutdown during retry wait");
                update(|s| s.last_event = "stopped".into());
                return;
            }
        }
    }
}

/// Polls cloudflared's metrics endpoint until the tunnel looks broken, then
/// returns the reason. Returns only when a restart is warranted — a healthy
/// tunnel keeps this future pending forever, which is what lets the caller
/// select on it against the child process.
async fn watch_health(metrics_port: u16, protocol: Protocol) -> Unhealthy {
    // Polling starts immediately but judgement waits: sleeping through the
    // grace period instead would leave /status reporting `healthy: false` for
    // the first minute of every restart, which is indistinguishable from a
    // tunnel that is genuinely down and would make any external monitor built
    // on this endpoint cry wolf on every restart.
    let began = Instant::now();
    let grace = Duration::from_secs(HEALTH_GRACE_SECS);

    let ready_url = format!("http://127.0.0.1:{metrics_port}/ready");
    let metrics_url = format!("http://127.0.0.1:{metrics_port}/metrics");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("Could not build health-check client: {} — health monitoring off", e);
            return std::future::pending().await;
        }
    };

    let mut strikes: u32 = 0;
    let mut ever_reachable = false;
    let mut unreachable_polls: u32 = 0;

    // QUIC reconnect counting, sampled once per window.
    let mut window_start = Instant::now();
    let mut window_baseline: Option<u64> = None;

    loop {
        tokio::time::sleep(Duration::from_secs(HEALTH_POLL_SECS)).await;
        // Four HA connections over a slow link take time to come up. Observe
        // that, but do not hold it against the tunnel yet.
        let in_grace = began.elapsed() < grace;

        match fetch_ready(&client, &ready_url).await {
            Some(ready) => {
                ever_reachable = true;
                unreachable_polls = 0;
                update(|s| {
                    s.metrics_reachable = true;
                    s.ready_connections = ready;
                    s.healthy = ready > 0;
                });

                if in_grace {
                    continue;
                }

                if ready == 0 {
                    strikes += 1;
                    warn!("Tunnel has 0 edge connections ({}/{})", strikes, UNHEALTHY_STRIKES);
                    if strikes >= UNHEALTHY_STRIKES {
                        return Unhealthy::NoConnections;
                    }
                } else {
                    if strikes > 0 {
                        info!("Tunnel recovered — {} edge connections", ready);
                    }
                    strikes = 0;
                }
            }
            None => {
                update(|s| s.metrics_reachable = false);

                // cloudflared binds its metrics listener a moment after start;
                // not answering yet is not evidence of anything.
                if in_grace {
                    continue;
                }

                if !ever_reachable {
                    // The metrics endpoint never came up — most likely the port
                    // is taken. Killing a tunnel we simply cannot observe would
                    // be worse than not watching it, so stand down and let the
                    // caller fall back to watching for process exit.
                    unreachable_polls += 1;
                    if unreachable_polls >= METRICS_UNREACHABLE_GIVEUP {
                        warn!(
                            "cloudflared metrics endpoint on 127.0.0.1:{} never answered — \
                             health monitoring disabled for this run. Set tunnel_metrics_port \
                             in config.toml to a free port to re-enable it.",
                            metrics_port
                        );
                        return std::future::pending().await;
                    }
                    continue;
                }

                // It answered before and has stopped. cloudflared serves
                // metrics from the same process that serves the tunnel, so this
                // counts against it like a zero-connection poll.
                strikes += 1;
                warn!(
                    "cloudflared metrics endpoint stopped responding ({}/{})",
                    strikes, UNHEALTHY_STRIKES
                );
                if strikes >= UNHEALTHY_STRIKES {
                    update(|s| s.healthy = false);
                    return Unhealthy::NoConnections;
                }
            }
        }

        // A tunnel can report connections and still be useless if it is
        // rebuilding them constantly. That churn is invisible to /ready, so it
        // gets its own signal.
        if protocol == Protocol::Quic {
            if let Some(closed) = fetch_quic_closed(&client, &metrics_url).await {
                update(|s| s.quic_closed_connections = closed);
                let baseline = *window_baseline.get_or_insert(closed);

                if window_start.elapsed() >= Duration::from_secs(CHURN_WINDOW_SECS) {
                    let churn = closed.saturating_sub(baseline);
                    if churn >= CHURN_LIMIT {
                        return Unhealthy::Churn(churn);
                    }
                    window_start = Instant::now();
                    window_baseline = Some(closed);
                }
            }
        }
    }
}

/// Can this machine reach the Cloudflare edge at all, over plain TCP?
///
/// This is the question that separates "the tunnel is broken" from "the
/// internet is gone", and they are indistinguishable from inside cloudflared:
/// a dropped uplink produces the same dial timeouts and the same zero
/// connection count as a genuinely bad transport. Resolving and connecting
/// exercises DNS and TCP together, so a flapping uplink fails it for the same
/// reason the tunnel just did.
async fn link_reachable() -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(EDGE_PROBE_TIMEOUT_SECS),
            tokio::net::TcpStream::connect(EDGE_PROBE_ADDR),
        )
        .await,
        Ok(Ok(_))
    )
}

/// Reads `readyConnections` from cloudflared's `/ready`. `None` means the
/// endpoint could not be reached or did not parse.
async fn fetch_ready(client: &reqwest::Client, url: &str) -> Option<u32> {
    let resp = client.get(url).send().await.ok()?;
    // /ready answers 503 with a body when it has no connections, so a non-2xx
    // status is still useful — parse the body either way.
    let body: serde_json::Value = resp.json().await.ok()?;
    body.get("readyConnections")?.as_u64().map(|n| n as u32)
}

async fn fetch_quic_closed(client: &reqwest::Client, url: &str) -> Option<u64> {
    let body = client.get(url).send().await.ok()?.text().await.ok()?;
    parse_metric(&body, "quic_client_closed_connections")
}

/// Pulls a single unlabelled counter out of a Prometheus exposition body.
fn parse_metric(body: &str, key: &str) -> Option<u64> {
    body.lines()
        .filter_map(|l| l.strip_prefix(key))
        .filter_map(|rest| rest.strip_prefix(' '))
        .next()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map(|v| v as u64)
}

/// Keeps the cloudflared log from growing without bound. One generation back is
/// enough to cover the outage you are currently investigating.
fn roll_log_if_large(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > LOG_ROLL_BYTES {
            let _ = std::fs::rename(path, path.with_extension("log.1"));
        }
    }
}

/// Creates a Job Object with KILL_ON_JOB_CLOSE.
/// Returns the raw handle value (kept alive by the caller's stack frame).
/// 0 means creation failed — assignment will be skipped gracefully.
#[cfg(windows)]
fn create_job_object() -> isize {
    use windows::Win32::System::JobObjects::{
        CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    unsafe {
        let job = match CreateJobObjectW(None, None) {
            Ok(h) => h,
            Err(e) => {
                warn!("Failed to create Job Object: {}", e);
                return 0;
            }
        };

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        if let Err(e) = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &raw const info as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            warn!("SetInformationJobObject failed: {}", e);
            return 0;
        }

        info!("Job Object created — cloudflared will be auto-killed on any exit");
        job.0 as isize
    }
}

/// Opens the cloudflared process by PID and assigns it to the Job Object.
/// After this, if our process exits for any reason, Windows kills cloudflared too.
#[cfg(windows)]
fn assign_process_to_job(pid: u32, job_handle: isize) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};

    unsafe {
        let job = windows::Win32::Foundation::HANDLE(job_handle as *mut _);

        match OpenProcess(PROCESS_ALL_ACCESS, false, pid) {
            Ok(proc) => {
                match AssignProcessToJobObject(job, proc) {
                    Ok(_) => info!("Cloudflared (pid {}) assigned to Job Object", pid),
                    Err(e) => warn!("AssignProcessToJobObject failed: {}", e),
                }
                let _ = CloseHandle(proc);
            }
            Err(e) => warn!("OpenProcess failed for pid {}: {}", pid, e),
        }
    }
}

fn extract_exe() -> anyhow::Result<PathBuf> {
    // Next to our own binary, NOT %TEMP%. As a LocalSystem service we execute
    // whatever is at this path, and when the service runs, %TEMP% resolves to
    // C:\Windows\Temp — a directory standard users can create files in. The
    // install directory is ACL'd to SYSTEM and Administrators by
    // service::lock_down_install_dir, so nothing unprivileged can swap the
    // binary we are about to launch.
    let path = std::env::current_exe()
        .map(|exe| exe.with_file_name("cloudflared.exe"))
        .unwrap_or_else(|_| std::env::temp_dir().join("win_automation_cloudflared.exe"));

    let needs_write = if path.exists() {
        match std::fs::metadata(&path) {
            Ok(meta) => meta.len() != CLOUDFLARED_EXE.len() as u64,
            Err(_) => true,
        }
    } else {
        true
    };

    if needs_write {
        std::fs::write(&path, CLOUDFLARED_EXE)?;
        info!("Extracted cloudflared.exe to {:?}", path);
    }

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prometheus_counter() {
        let body = "# HELP x\nquic_client_closed_connections 244\ncloudflared_tunnel_ha_connections 4\n";
        assert_eq!(parse_metric(body, "quic_client_closed_connections"), Some(244));
        assert_eq!(parse_metric(body, "cloudflared_tunnel_ha_connections"), Some(4));
    }

    #[test]
    fn ignores_labelled_series_with_the_same_prefix() {
        // cloudflared_tunnel_server_locations{...} shares no exact-key match,
        // and a labelled line must never be mistaken for the bare counter.
        let body = "cloudflared_tunnel_server_locations{edge_location=\"ceb01\"} 1\n";
        assert_eq!(parse_metric(body, "cloudflared_tunnel_server_locations"), None);
    }

    #[test]
    fn missing_metric_is_none() {
        assert_eq!(parse_metric("other_metric 1\n", "quic_client_closed_connections"), None);
    }

    #[test]
    fn protocol_pref_defaults_to_auto_on_junk() {
        assert!(matches!(ProtocolPref::parse("nonsense"), ProtocolPref::Auto));
        assert!(matches!(ProtocolPref::parse(""), ProtocolPref::Auto));
        assert!(matches!(ProtocolPref::parse("auto"), ProtocolPref::Auto));
    }

    #[test]
    fn protocol_pref_pins_explicit_choices() {
        assert!(matches!(
            ProtocolPref::parse("HTTP2"),
            ProtocolPref::Pinned(Protocol::Http2)
        ));
        assert!(matches!(
            ProtocolPref::parse(" quic "),
            ProtocolPref::Pinned(Protocol::Quic)
        ));
    }

    #[test]
    fn auto_starts_on_quic() {
        assert!(ProtocolPref::Auto.initial() == Protocol::Quic);
    }
}
