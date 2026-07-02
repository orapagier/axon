//! C3: Prometheus metrics surface.
//!
//! A single in-process recorder (installed once at startup) backs the
//! `metrics::{counter,gauge,histogram}!` macros the engine emits to. `GET
//! /metrics` (behind the dashboard bearer auth) renders the registry; a compact
//! `GET /api/health` exposes the same live gauges as JSON for the dashboard's own
//! status widget. No external collector is required to read either.

use crate::state::AppState;
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use once_cell::sync::OnceCell;
use serde_json::json;
use std::sync::atomic::Ordering;

static HANDLE: OnceCell<PrometheusHandle> = OnceCell::new();

/// Histogram bucket boundaries (seconds) shared by the duration histograms —
/// from a few ms up to 5 minutes, covering quick HTTP nodes through long runs.
const DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];

/// Install the Prometheus recorder once. Safe to call repeatedly (no-op after the
/// first success). Metric emission elsewhere is a silent no-op until this runs,
/// so a failure here degrades to "no metrics", never a crash.
pub fn init() {
    if HANDLE.get().is_some() {
        return;
    }
    let builder = match PrometheusBuilder::new().set_buckets_for_metric(
        Matcher::Suffix("_duration_seconds".to_string()),
        DURATION_BUCKETS,
    ) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("metrics: bucket config failed ({e}); using defaults");
            PrometheusBuilder::new()
        }
    };
    match builder.install_recorder() {
        Ok(handle) => {
            describe();
            let _ = HANDLE.set(handle);
            tracing::info!("Prometheus metrics recorder installed (GET /metrics)");
        }
        Err(e) => tracing::warn!("metrics: recorder install failed ({e}); /metrics disabled"),
    }
}

fn describe() {
    use metrics::{describe_counter, describe_gauge, describe_histogram, Unit};
    describe_counter!(
        "axon_workflow_runs_total",
        "Workflow runs by terminal status"
    );
    describe_histogram!(
        "axon_workflow_run_duration_seconds",
        Unit::Seconds,
        "End-to-end workflow run wall time"
    );
    describe_histogram!(
        "axon_node_exec_duration_seconds",
        Unit::Seconds,
        "Per-node execution wall time by node type"
    );
    describe_counter!("axon_node_retries_total", "Node execution retries (A1)");
    describe_gauge!("axon_active_runs", "Workflow runs currently executing");
    describe_gauge!(
        "axon_run_queue_depth",
        "Runs queued waiting for a concurrency slot"
    );
}

/// Render the Prometheus text exposition, refreshing the live gauges from the B3
/// atomics at scrape time. `None` when the recorder failed to install.
pub fn render(state: &AppState) -> Option<String> {
    let handle = HANDLE.get()?;
    metrics::gauge!("axon_active_runs").set(state.active_runs.load(Ordering::SeqCst) as f64);
    metrics::gauge!("axon_run_queue_depth")
        .set(state.run_queue_depth.load(Ordering::SeqCst) as f64);
    Some(handle.render())
}

// ── Emission helpers (no-ops until the recorder is installed) ───────────────

/// Record a terminal workflow run: per-status counter + duration histogram.
/// Suspended ('waiting') runs are not terminal and must not be counted here.
pub fn record_run_complete(status: &str, duration_secs: f64) {
    metrics::counter!("axon_workflow_runs_total", "status" => status.to_string()).increment(1);
    metrics::histogram!("axon_workflow_run_duration_seconds").record(duration_secs);
}

/// Record one node execution's wall time, labeled by node type.
pub fn record_node_exec(node_type: &str, duration_secs: f64) {
    metrics::histogram!("axon_node_exec_duration_seconds", "node_type" => node_type.to_string())
        .record(duration_secs);
}

/// Record a node retry attempt (A1), labeled by node type.
pub fn record_node_retry(node_type: &str) {
    metrics::counter!("axon_node_retries_total", "node_type" => node_type.to_string()).increment(1);
}

// ── HTTP handlers ───────────────────────────────────────────────────────────

/// `GET /metrics` — Prometheus text exposition (gated by the dashboard bearer).
pub async fn metrics_endpoint(State(state): State<AppState>) -> Response {
    match render(&state) {
        Some(body) => ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body).into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "metrics recorder not installed",
        )
            .into_response(),
    }
}

/// `GET /api/health` — compact live status JSON for the dashboard status widget.
pub async fn health_json(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "active_runs": state.active_runs.load(Ordering::SeqCst),
        "run_queue_depth": state.run_queue_depth.load(Ordering::SeqCst),
        "max_concurrent_runs": state.settings.workflow_max_concurrent_runs(),
        "max_queue_depth": state.settings.workflow_max_queue_depth(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_installs_and_renders_emitted_metrics() {
        init(); // idempotent; installs the single global recorder
        record_run_complete("success", 1.5);
        record_node_exec("synapse", 0.2);
        record_node_retry("synapse");
        let out = HANDLE.get().expect("recorder should be installed").render();
        assert!(
            out.contains("axon_workflow_runs_total"),
            "missing runs counter:\n{out}"
        );
        assert!(
            out.contains("axon_node_exec_duration_seconds"),
            "missing node histogram:\n{out}"
        );
        assert!(
            out.contains("axon_node_retries_total"),
            "missing retries counter:\n{out}"
        );
        // The success label is rendered on the counter.
        assert!(
            out.contains("status=\"success\""),
            "missing status label:\n{out}"
        );
    }
}
