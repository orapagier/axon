//! Wake-word enrollment endpoints for the web dashboard.
//!
//! The browser's rustpotter-worklet (WASM) can only run detection — reading
//! its exported symbols shows no training/building capability at all. So
//! browser-side enrollment records clips and ships them here:
//!
//! * `POST /api/wakeword/build` — a `name` field plus one or more `sample`
//!   file fields (WAV clips, mirroring axon-voice's on-device JNI builder)
//!   builds a personal `.rpw`, persists it in `wake_model`, and confirms.
//! * `GET /api/wakeword/model` — serves the persisted model back as raw
//!   bytes, or 404 when nothing's enrolled yet — the dashboard has no bundled
//!   fallback model, so a 404 sends the user into the enrollment flow instead.
//!
//! The actual build runs in the standalone `wake-model-builder` binary
//! (repo root, deliberately outside the cargo workspace — same reasoning as
//! axon-voice/rustpotter-jni) rather than linking `rustpotter` straight into
//! this crate: rustpotter unconditionally pulls in candle-core 0.2.2 (its
//! neural-training backend, needed even though we only use the simple
//! few-shot "reference" builder), and that stale pin's `rand`/`rand_distr`
//! requirement conflicts with the rest of axon-agent's dependency tree.
//! Shelling out keeps that conflict fully contained in the other crate's own
//! (separately-resolved) build.

use super::*;
use axum::body::Body;
use axum::extract::Multipart;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use std::path::PathBuf;

/// Mirrors `rustpotter-cli build`'s documented recommendation (3-8 samples).
const MIN_SAMPLES: usize = 3;

/// `wake-model-builder(.exe)`, built via `cargo build --release` in
/// `wake-model-builder/` and copied to `bin/` next to wherever axon-agent
/// runs from (same CWD-relative convention as the `static/` dashboard
/// assets) — or `AXON_WAKE_BUILDER_BIN` to point at it explicitly.
fn builder_bin_path() -> PathBuf {
    if let Ok(p) = std::env::var("AXON_WAKE_BUILDER_BIN") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let exe = if cfg!(windows) {
        "wake-model-builder.exe"
    } else {
        "wake-model-builder"
    };
    PathBuf::from("bin").join(exe)
}

pub async fn build_wakeword(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Json<Value> {
    // The bin.is_file() check below intentionally runs AFTER the multipart
    // loop, not before: axum's Multipart extractor streams the request body
    // rather than buffering it up front, so returning early — before the
    // client has finished uploading its WAV samples — makes hyper close the
    // connection mid-upload. The client's still-in-flight writes then fail
    // with a broken pipe, which a fronting proxy (e.g. cloudflared) surfaces
    // as a bare 502 instead of this handler's intended JSON error body.
    let mut name = String::new();
    let mut samples: Vec<Vec<u8>> = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        match field.name().unwrap_or("") {
            "name" => {
                if let Ok(text) = field.text().await {
                    name = text.trim().to_string();
                }
            }
            "sample" => {
                if let Ok(bytes) = field.bytes().await {
                    if !bytes.is_empty() {
                        samples.push(bytes.to_vec());
                    }
                }
            }
            _ => {}
        }
    }
    if name.is_empty() {
        name = "hey axon".to_string();
    }
    if samples.len() < MIN_SAMPLES {
        return Json(json!({
            "error": format!("need at least {MIN_SAMPLES} samples, got {}", samples.len())
        }));
    }

    let bin = builder_bin_path();
    if !bin.is_file() {
        return Json(json!({
            "error": format!(
                "wake-model-builder not found at {} — build it from wake-model-builder/ \
                 (cargo build --release) and copy the binary there, or set AXON_WAKE_BUILDER_BIN",
                bin.display()
            )
        }));
    }

    let work_dir = std::env::temp_dir().join(format!("axon-wake-{}", uuid::Uuid::new_v4()));
    if let Err(e) = std::fs::create_dir_all(&work_dir) {
        return Json(json!({"error": format!("couldn't create a scratch dir: {e}")}));
    }
    let mut sample_paths = Vec::with_capacity(samples.len());
    for (i, wav) in samples.iter().enumerate() {
        let path = work_dir.join(format!("take{i}.wav"));
        if let Err(e) = std::fs::write(&path, wav) {
            let _ = std::fs::remove_dir_all(&work_dir);
            return Json(json!({"error": format!("couldn't write sample {i}: {e}")}));
        }
        sample_paths.push(path);
    }
    let out_path = work_dir.join("model.rpw");

    // Same tuning the bundled "Hey Axon" model shipped with — no per-user
    // calibration yet.
    let result = tokio::process::Command::new(&bin)
        .arg(&name)
        .arg("0.47")
        .arg("0.0")
        .arg("16")
        .arg(&out_path)
        .args(&sample_paths)
        .output()
        .await;

    let model = match result {
        Ok(out) if out.status.success() => std::fs::read(&out_path).ok(),
        Ok(out) => {
            let _ = std::fs::remove_dir_all(&work_dir);
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Json(json!({"error": format!("couldn't build a model: {}", stderr.trim())}));
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&work_dir);
            return Json(json!({"error": format!("couldn't run wake-model-builder: {e}")}));
        }
    };
    let _ = std::fs::remove_dir_all(&work_dir);
    let Some(model) = model else {
        return Json(json!({"error": "wake-model-builder exited ok but wrote no model"}));
    };

    let Ok(conn) = state.db.get() else {
        return Json(json!({"error": "database unavailable"}));
    };
    if let Err(e) = conn.execute(
        "INSERT INTO wake_model (id, phrase, model, updated_at) VALUES (1, ?1, ?2, datetime('now'))
         ON CONFLICT(id) DO UPDATE SET phrase=excluded.phrase, model=excluded.model, updated_at=excluded.updated_at",
        rusqlite::params![name, model],
    ) {
        tracing::warn!("wake_model persist failed: {e}");
        return Json(json!({"error": format!("couldn't save the model: {e}")}));
    }

    Json(json!({"ok": true, "phrase": name, "bytes": model.len()}))
}

pub async fn get_wakeword_model(State(state): State<AppState>) -> Response {
    let Ok(conn) = state.db.get() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let row: rusqlite::Result<Vec<u8>> =
        conn.query_row("SELECT model FROM wake_model WHERE id = 1", [], |r| {
            r.get(0)
        });
    match row {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            Body::from(bytes),
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
