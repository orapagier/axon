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
//! * **Address family.** cloudflared dials the edge over IPv4 only unless told
//!   otherwise. On a link where the IPv4 path to 198.41.192.0/24 is being
//!   dropped but IPv6 works, that default turns a reachable edge into an
//!   unreachable one. `--edge-ip-version auto` lets it use whichever family is
//!   actually carrying packets.
//!
//! ## Restarting is the expensive move
//!
//! The hard-won lesson in this file is that a restart is *not* a cheap way to
//! nudge a struggling tunnel. Measured on a consumer link, a cold start costs
//! about 45 seconds of total unreachability — DNS, the feature fetch, the
//! protocol lookup and the metrics bind all happen before the first edge
//! connection registers. cloudflared's own retry ladder, meanwhile, reconnects a
//! dropped connection in seconds without dropping the others.
//!
//! An earlier version of this supervisor restarted after one minute of zero
//! connections. On a lossy link a single edge dial can burn 20-30 s in a TCP
//! timeout, so one minute is not even a full round of cloudflared's retries: the
//! supervisor was killing tunnels that were seconds from reconnecting, then
//! paying 45 s of downtime for the privilege. Restarts ran at one every four
//! minutes and the tunnel was down roughly a fifth of the time — all of it self
//! inflicted, none of it visible as anything but "the tunnel keeps flapping".
//!
//! So the rules here are deliberately conservative:
//!
//! * Judge the tunnel only after it has had a fair chance to come up.
//! * Never restart while the machine cannot reach the Cloudflare edge at all —
//!   a restart cannot fix an uplink, and it guarantees a cold start once the
//!   link returns. Wait instead.
//! * Give cloudflared minutes, not seconds, to recover on its own.
//! * Cap how often restarts may happen, because a restart storm is strictly
//!   worse than leaving a struggling tunnel alone.

use std::collections::VecDeque;
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
/// its health counts against it.
///
/// Measured cold start on the link this was debugged against: spawn to first
/// registered connection was 45 s, of which 22 s went on DNS and the protocol
/// lookup before the transport was even chosen. The old budget was also 45 s,
/// so it expired at the exact moment the tunnel came up — every slow start was
/// scored as a failure. Doubling it costs nothing (a healthy tunnel is still
/// observed throughout the grace period, it is just not judged) and removes a
/// whole class of self-inflicted restart.
const HEALTH_GRACE_SECS: u64 = 120;
/// Interval between `/ready` polls once the grace period is over.
const HEALTH_POLL_SECS: u64 = 15;
/// Consecutive zero-connection polls, *with the Cloudflare edge TCP-reachable
/// throughout*, before we call the tunnel wedged. At 15 s that is five minutes.
///
/// The reachability qualifier is what makes a number this large safe: we are no
/// longer counting "the internet is down" toward a restart, so every strike is
/// evidence that cloudflared specifically is failing to use a working network.
/// Five minutes is far past cloudflared's own retry ladder, which tops out
/// around 16-32 s between attempts.
const UNHEALTHY_STRIKES: u32 = 20;
/// The same threshold once the restart budget is spent. Fifteen minutes.
///
/// Not infinity: a genuinely wedged cloudflared must still have a way out. But
/// once restarts have demonstrably failed to help, the next one waits a long
/// time, because the evidence says restarting is not the remedy.
const UNHEALTHY_STRIKES_THROTTLED: u32 = 60;
/// If the metrics endpoint never answers at all, health monitoring disables
/// itself after this many polls rather than killing a tunnel it cannot see.
const METRICS_UNREACHABLE_GIVEUP: u32 = 8;

/// Rolling window over which health-driven restarts are counted.
const RESTART_BUDGET_WINDOW_SECS: u64 = 3600;
/// Health-driven restarts permitted per window before the threshold escalates
/// to `UNHEALTHY_STRIKES_THROTTLED`.
///
/// Three per hour is generous for a remedy that works: a restart that fixes
/// anything fixes it the first time. A fourth in the same hour means restarting
/// is not fixing it, and continuing to restart just adds cold-start downtime to
/// whatever is already wrong.
const RESTART_BUDGET: usize = 3;

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

/// Continuous seconds of an unreachable uplink before we write the "this is
/// your internet, not the tunnel" note to the error log. Five minutes, so a
/// router reboot does not earn a scary log entry.
const LINK_DOWN_REPORT_AFTER_SECS: u64 = 300;
/// How often to repeat the "still waiting for the uplink" line while an outage
/// drags on. Every poll would bury everything else in the log.
const LINK_DOWN_LOG_EVERY: u32 = 20;

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

/// Maps `tunnel_edge_ip_version` onto the three values cloudflared accepts.
///
/// Defaults to `auto` rather than cloudflared's own `4`. A machine with working
/// IPv6 and a lossy IPv4 path to 198.41.192.0/24 — which is what a lot of
/// consumer links look like — can reach the edge perfectly well over v6, and
/// the IPv4-only default is the only thing stopping it. On an IPv4-only network
/// `auto` behaves exactly like `4`, so there is nothing to lose by it.
fn normalise_edge_ip_version(raw: Option<&str>) -> &'static str {
    match raw.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
        "4" | "ipv4" => "4",
        "6" | "ipv6" => "6",
        _ => "auto",
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
    /// The tunnel is down and so is the machine's path to the internet, so the
    /// supervisor is deliberately holding rather than restarting. Distinguishes
    /// "we have given up" from "there is nothing a restart could achieve".
    pub waiting_for_uplink: bool,
    /// Health-driven restarts in the last hour have hit `RESTART_BUDGET`, so the
    /// supervisor has backed off to a much longer threshold. Seeing this set is
    /// the signal to look at the network rather than the app.
    pub restart_budget_spent: bool,
    /// Continuous seconds with no edge connection. Zero whenever the tunnel is
    /// healthy, and the number to quote when reporting an outage.
    pub secs_without_edge: u64,
    /// Address family cloudflared was told to dial the edge with.
    pub edge_ip_version: String,
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
            waiting_for_uplink: false,
            restart_budget_spent: false,
            secs_without_edge: 0,
            edge_ip_version: "auto".into(),
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
///
/// Both variants imply the Cloudflare edge was TCP-reachable when the decision
/// was made — `watch_health` will not return at all while the uplink is down,
/// because a restart cannot mend a network. So every value here is evidence
/// against cloudflared or its transport, never against the ISP.
enum Unhealthy {
    /// cloudflared is running, the edge is reachable, and it still has no
    /// connection to it.
    NoConnections { secs: u64 },
    /// The transport is reconnecting so often it cannot carry traffic.
    Churn(u64),
}

impl Unhealthy {
    fn describe(&self) -> String {
        match self {
            Unhealthy::NoConnections { secs } => format!(
                "no edge connections for {}s while the Cloudflare edge was reachable over TCP",
                secs
            ),
            Unhealthy::Churn(n) => format!(
                "{} QUIC reconnects in {}s — the UDP path is not holding",
                n, CHURN_WINDOW_SECS
            ),
        }
    }
}

/// Rolling count of health-driven restarts, used to stop a restart storm.
///
/// Restarting cloudflared costs ~45 s of hard downtime. If three of them inside
/// an hour have not fixed things, a fourth will not either, and the cure has
/// become worse than the disease — so the threshold escalates instead.
struct RestartBudget {
    events: VecDeque<Instant>,
}

impl RestartBudget {
    fn new() -> Self {
        Self {
            events: VecDeque::new(),
        }
    }

    fn record(&mut self) {
        self.events.push_back(Instant::now());
    }

    /// Restarts inside the window, after dropping anything older.
    fn recent(&mut self) -> usize {
        let window = Duration::from_secs(RESTART_BUDGET_WINDOW_SECS);
        while self
            .events
            .front()
            .is_some_and(|t| t.elapsed() > window)
        {
            self.events.pop_front();
        }
        self.events.len()
    }

    /// Zero-connection polls required before the next restart is allowed.
    fn strikes_required(&mut self) -> u32 {
        if self.recent() >= RESTART_BUDGET {
            UNHEALTHY_STRIKES_THROTTLED
        } else {
            UNHEALTHY_STRIKES
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
    edge_ip_version: Option<String>,
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
    let edge_ip_version = normalise_edge_ip_version(edge_ip_version.as_deref());

    info!(
        "Starting Cloudflare tunnel (protocol: {}, edge IP: {}, metrics: 127.0.0.1:{}, log: {})",
        protocol.as_str(),
        edge_ip_version,
        metrics_port,
        log_path.display()
    );
    update(|s| {
        s.protocol = protocol.as_str();
        s.metrics_port = metrics_port;
        s.edge_ip_version = edge_ip_version.to_string();
        s.last_event = "starting".into();
    });

    let mut backoff_secs = MIN_BACKOFF_SECS;
    // Separate, gentler ramp for restarts we triggered ourselves.
    let mut health_backoff_secs = MIN_BACKOFF_SECS;
    let mut consecutive_failures: u32 = 0;
    let mut budget = RestartBudget::new();

    loop {
        roll_log_if_large(&log_path);
        let started = Instant::now();
        let strikes_required = budget.strikes_required();
        update(|s| s.restart_budget_spent = strikes_required != UNHEALTHY_STRIKES);

        // Global flags have to precede the `run` subcommand — cloudflared
        // rejects `tunnel run --no-autoupdate` outright. Verified against the
        // bundled build; keep this ordering if you add flags.
        let args: Vec<String> = vec![
            "tunnel".into(),
            "--no-autoupdate".into(),
            "--protocol".into(),
            protocol.as_str().into(),
            // Without this cloudflared dials the edge over IPv4 only. When the
            // IPv4 path is being dropped and IPv6 is not, that default is the
            // difference between a tunnel and no tunnel; "auto" simply uses
            // whichever family answers, and is a no-op on IPv4-only networks.
            "--edge-ip-version".into(),
            edge_ip_version.into(),
            // Default is 5. Each retry on a lossy link may burn a 20-30 s TCP
            // timeout, so five attempts is a couple of minutes of trying — and
            // giving up early on one of four HA connections leaves the tunnel
            // permanently degraded until something forces a reconnect.
            "--retries".into(),
            "10".into(),
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
                    reason = watch_health(metrics_port, protocol, strikes_required) => {
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
        // network was not. Retry it sooner.
        let health_kill = matches!(outcome, Outcome::HealthKill(_));
        if let Outcome::HealthKill(reason) = &outcome {
            update(|s| s.last_event = format!("restarted — {}", reason.describe()));

            // Spend a unit of budget. Three of these in an hour and the next
            // restart has to clear a fifteen-minute bar instead of a
            // five-minute one, because restarting has stopped being a remedy.
            budget.record();
            let spent = budget.recent();
            if spent == RESTART_BUDGET {
                let msg = format!(
                    "The Cloudflare tunnel has been restarted {} times in the last hour with a \
                     reachable edge each time, and restarting is evidently not fixing it. \
                     Backing off: the tunnel now has to sit at zero connections for {} minutes \
                     before another restart, so cloudflared's own reconnect logic gets a proper \
                     chance instead of being interrupted. Look at the link quality rather than \
                     the app — see {} for what cloudflared makes of it.",
                    spent,
                    HEALTH_POLL_SECS * UNHEALTHY_STRIKES_THROTTLED as u64 / 60,
                    log_path.display()
                );
                warn!("{}", msg);
                crate::log_error_to_file(&msg);
            }

            // Reaching here at all means the edge was TCP-reachable when the
            // decision was made — watch_health holds rather than returning
            // while the uplink is down — so this is evidence against the
            // transport, never against the ISP.
            if protocol == Protocol::Quic {
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

/// Polls cloudflared's metrics endpoint until the tunnel looks broken *and a
/// restart could plausibly help*, then returns the reason. Returns only when a
/// restart is warranted — a healthy tunnel, or a tunnel that is down only
/// because the machine has no internet, keeps this future pending forever,
/// which is what lets the caller select on it against the child process.
///
/// `strikes_required` is the number of consecutive zero-connection polls needed
/// before a restart, which the caller raises once the restart budget is spent.
async fn watch_health(metrics_port: u16, protocol: Protocol, strikes_required: u32) -> Unhealthy {
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
    // Consecutive polls spent waiting out an uplink outage, and when it started.
    let mut link_down_polls: u32 = 0;
    let mut link_down_since: Option<Instant> = None;
    let mut link_down_reported = false;

    // QUIC reconnect counting, sampled once per window.
    let mut window_start = Instant::now();
    let mut window_baseline: Option<u64> = None;

    loop {
        tokio::time::sleep(Duration::from_secs(HEALTH_POLL_SECS)).await;
        // Four HA connections over a slow link take time to come up. Observe
        // that, but do not hold it against the tunnel yet.
        let in_grace = began.elapsed() < grace;

        // `None` means the poll told us nothing either way; `Some(true)` that
        // the tunnel holds at least one edge connection; `Some(false)` that it
        // holds none. Collapsing both failure paths into one verdict keeps the
        // restart decision below in a single place.
        let verdict: Option<bool> = match fetch_ready(&client, &ready_url).await {
            Some(ready) => {
                ever_reachable = true;
                unreachable_polls = 0;
                update(|s| {
                    s.metrics_reachable = true;
                    s.ready_connections = ready;
                    s.healthy = ready > 0;
                });

                if in_grace {
                    None
                } else {
                    Some(ready > 0)
                }
            }
            None => {
                update(|s| s.metrics_reachable = false);

                // cloudflared binds its metrics listener a moment after start;
                // not answering yet is not evidence of anything.
                if in_grace {
                    None
                } else if !ever_reachable {
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
                    None
                } else {
                    // It answered before and has stopped. cloudflared serves
                    // metrics from the same process that serves the tunnel, so
                    // this counts against it like a zero-connection poll.
                    update(|s| s.healthy = false);
                    Some(false)
                }
            }
        };

        match verdict {
            None => {}
            Some(true) => {
                if strikes > 0 {
                    info!(
                        "Tunnel recovered after {}s without an edge connection",
                        strikes as u64 * HEALTH_POLL_SECS
                    );
                }
                strikes = 0;
                link_down_polls = 0;
                link_down_since = None;
                link_down_reported = false;
                update(|s| {
                    s.secs_without_edge = 0;
                    s.waiting_for_uplink = false;
                });
            }
            Some(false) => {
                strikes += 1;
                let secs = strikes as u64 * HEALTH_POLL_SECS;
                update(|s| s.secs_without_edge = secs);

                if strikes < strikes_required {
                    warn!(
                        "Tunnel has no edge connection ({}/{} polls, {}s so far)",
                        strikes, strikes_required, secs
                    );
                } else if link_reachable().await {
                    // There is a network, and cloudflared is not using it.
                    // Now — and only now — is a restart worth its cold start.
                    update(|s| {
                        s.network_reachable = Some(true);
                        s.waiting_for_uplink = false;
                    });
                    return Unhealthy::NoConnections { secs };
                } else {
                    // The decisive question, and the one this supervisor used
                    // to ask only *after* killing the process: is there a
                    // network to reach the edge over at all? Restarting cannot
                    // mend an uplink, and it guarantees ~45 s of extra downtime
                    // once the link does come back. So hold, and let
                    // cloudflared keep retrying into the void — that costs
                    // nothing and reconnects the instant the link returns.
                    if link_down_since.is_none() {
                        link_down_since = Some(Instant::now());
                        info!(
                            "Tunnel is down and so is this machine's path to Cloudflare — \
                             holding instead of restarting, since a restart cannot mend an uplink"
                        );
                    }
                    update(|s| {
                        s.network_reachable = Some(false);
                        s.waiting_for_uplink = true;
                        s.last_event = "waiting for the uplink — the network path to Cloudflare \
                                        is down; this is the internet connection, not the tunnel"
                            .into();
                    });

                    // Say it once, plainly, where someone will find it. Every
                    // symptom above this line looks like a broken tunnel.
                    let down_for = link_down_since.map_or(0, |t| t.elapsed().as_secs());
                    if !link_down_reported && down_for >= LINK_DOWN_REPORT_AFTER_SECS {
                        link_down_reported = true;
                        let msg = format!(
                            "The Cloudflare tunnel has had no edge connection for {}s because \
                             this machine cannot reach the internet — the Cloudflare edge has \
                             been unreachable over plain TCP for {}s, so the tunnel and its \
                             token are fine and nothing is being restarted. Check the router/ISP \
                             uplink: if the local gateway pings cleanly while anything beyond it \
                             times out, the fault is upstream of this machine and nothing in \
                             this app can work around it. The tunnel will reconnect on its own \
                             when the link returns.",
                            secs, down_for
                        );
                        warn!("{}", msg);
                        crate::log_error_to_file(&msg);
                    }

                    link_down_polls += 1;
                    if link_down_polls % LINK_DOWN_LOG_EVERY == 0 {
                        info!(
                            "Still waiting for the uplink ({}s without an edge connection)",
                            secs
                        );
                    }
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

    #[test]
    fn edge_ip_version_defaults_to_auto() {
        assert_eq!(normalise_edge_ip_version(None), "auto");
        assert_eq!(normalise_edge_ip_version(Some("")), "auto");
        assert_eq!(normalise_edge_ip_version(Some("nonsense")), "auto");
    }

    #[test]
    fn edge_ip_version_maps_onto_cloudflared_values() {
        assert_eq!(normalise_edge_ip_version(Some(" IPv4 ")), "4");
        assert_eq!(normalise_edge_ip_version(Some("6")), "6");
        assert_eq!(normalise_edge_ip_version(Some("ipv6")), "6");
    }

    #[test]
    fn restart_budget_escalates_after_its_allowance() {
        let mut b = RestartBudget::new();
        for _ in 0..RESTART_BUDGET - 1 {
            b.record();
            assert_eq!(b.strikes_required(), UNHEALTHY_STRIKES);
        }
        // The one that spends the budget is also the one that raises the bar.
        b.record();
        assert_eq!(b.strikes_required(), UNHEALTHY_STRIKES_THROTTLED);
        b.record();
        assert_eq!(b.strikes_required(), UNHEALTHY_STRIKES_THROTTLED);
    }

    #[test]
    fn restart_budget_forgets_events_outside_the_window() {
        let mut b = RestartBudget::new();
        // Backdate everything past the window; it should all be pruned.
        let stale = Instant::now()
            .checked_sub(Duration::from_secs(RESTART_BUDGET_WINDOW_SECS + 60))
            .expect("clock far enough from the epoch to backdate");
        for _ in 0..RESTART_BUDGET + 2 {
            b.events.push_back(stale);
        }
        assert_eq!(b.recent(), 0);
        assert_eq!(b.strikes_required(), UNHEALTHY_STRIKES);
    }

    /// The supervisor must be more patient than cloudflared's own retry ladder,
    /// which tops out around 16-32 s between attempts. Restarting inside that
    /// window is what produced the original flapping: a ~45 s cold start bought
    /// to interrupt a reconnect that was already in progress.
    #[test]
    fn patience_exceeds_cloudflareds_own_retry_ladder() {
        let patience = HEALTH_POLL_SECS * UNHEALTHY_STRIKES as u64;
        assert!(
            patience >= 300,
            "zero-connection patience is {patience}s; anything under five minutes \
             interrupts cloudflared mid-reconnect"
        );
        assert!(HEALTH_GRACE_SECS >= 90, "cold start alone measures ~45s");
    }
}
