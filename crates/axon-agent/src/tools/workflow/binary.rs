//! B2: binary / large-payload offloading.
//!
//! Node outputs occasionally carry large payloads — base64 images, file bytes,
//! big HTTP response bodies — as long JSON string leaves. Persisting them inline
//! in `workflow_runs.node_results` bloats SQLite, slows the incremental UPDATE,
//! and balloons run history.
//!
//! This module offloads large string leaves to a content-addressed blob store on
//! disk (`<data_files>/wf_blobs/<sha256>`), replacing each with a small
//! descriptor `{ "_axon_binary": { id, size } }`. The transform runs ONLY when
//! serializing results to the database; the engine's in-memory results keep the
//! full data, so same-run downstream nodes are unaffected. Every path that reads
//! results back from the DB rehydrates the descriptors first, so no consumer ever
//! sees one — the win is smaller at-rest storage + natural dedup (identical
//! payloads share one blob, and re-offloading the same value is idempotent).
//!
//! Blobs are reclaimed by a sweep in [`crate::maintenance`] that deletes any file
//! not referenced by a surviving run.

use super::NodeResult;
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;

/// Single-key marker object that stands in for an offloaded value.
pub const BINARY_KEY: &str = "_axon_binary";

fn blob_dir() -> PathBuf {
    // `AXON_WF_BLOB_DIR` overrides the default (used to isolate the destructive
    // GC sweep in tests from a dev instance's real blobs).
    let dir = match std::env::var("AXON_WF_BLOB_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => axon_core::data_files_dir().join("wf_blobs"),
    };
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
    }
    dir
}

fn blob_path(id: &str) -> PathBuf {
    blob_dir().join(id)
}

/// Store bytes under their sha256 (content-addressed: identical payloads dedupe
/// and repeated offloads of the same value reuse one file — no leaks). Returns
/// the blob id, or `None` if the write failed.
fn store_blob(bytes: &[u8]) -> Option<String> {
    use sha2::{Digest, Sha256};
    let id = format!("{:x}", Sha256::digest(bytes));
    let path = blob_path(&id);
    if !path.exists() {
        if let Err(e) = std::fs::write(&path, bytes) {
            tracing::warn!("wf_blob write failed ({}): {}", id, e);
            return None;
        }
    }
    Some(id)
}

fn load_blob(id: &str) -> Option<Vec<u8>> {
    std::fs::read(blob_path(id)).ok()
}

/// True when `map` is exactly a descriptor we wrote: a single `_axon_binary`
/// key whose value is `{ id: <64-hex sha256>, size: <number> }`. The strict
/// id/size shape keeps genuine node output that merely *contains* an
/// `_axon_binary` key from being mistaken for a descriptor — which would
/// otherwise corrupt it on read (rehydrate would try to load a nonexistent blob
/// and replace the value with a "[missing binary blob]" marker).
fn is_descriptor_map(map: &serde_json::Map<String, Value>) -> bool {
    if map.len() != 1 {
        return false;
    }
    let Some(d) = map.get(BINARY_KEY).and_then(|v| v.as_object()) else {
        return false;
    };
    let id_ok = d
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or(false);
    let size_ok = d.get("size").map(|v| v.is_number()).unwrap_or(false);
    id_ok && size_ok
}

/// Replace large string leaves with descriptors, in place. Existing descriptors
/// are left alone. `threshold` is the inline byte cap; `0` disables offloading.
pub fn offload_value(v: &mut Value, threshold: usize) {
    if threshold == 0 {
        return;
    }
    match v {
        Value::String(s) if s.len() > threshold => {
            if let Some(id) = store_blob(s.as_bytes()) {
                let size = s.len();
                *v = serde_json::json!({ BINARY_KEY: { "id": id, "size": size } });
            }
        }
        Value::Object(map) => {
            if is_descriptor_map(map) {
                return; // never descend into / re-offload a descriptor
            }
            for child in map.values_mut() {
                offload_value(child, threshold);
            }
        }
        Value::Array(arr) => {
            for child in arr.iter_mut() {
                offload_value(child, threshold);
            }
        }
        _ => {}
    }
}

/// Replace descriptors with their original bytes (as the original UTF-8 string),
/// in place. A blob missing on disk (GC race / manual delete) becomes a marker
/// string rather than failing the read.
pub fn rehydrate_value(v: &mut Value) {
    let blob_id = match v.as_object() {
        Some(map) if is_descriptor_map(map) => map
            .get(BINARY_KEY)
            .and_then(|d| d.get("id"))
            .and_then(|id| id.as_str())
            .map(str::to_string),
        _ => None,
    };
    if let Some(id) = blob_id {
        *v = match load_blob(&id) {
            Some(bytes) => Value::String(String::from_utf8_lossy(&bytes).into_owned()),
            None => Value::String(format!("[missing binary blob {}]", id)),
        };
        return;
    }
    match v {
        Value::Object(map) => {
            for child in map.values_mut() {
                rehydrate_value(child);
            }
        }
        Value::Array(arr) => {
            for child in arr.iter_mut() {
                rehydrate_value(child);
            }
        }
        _ => {}
    }
}

/// Serialize results for DB storage with large values offloaded. The in-memory
/// `results` are untouched — offloading runs on a serialized clone.
pub fn results_to_db_json(results: &[NodeResult], threshold: usize) -> String {
    let mut v = match serde_json::to_value(results) {
        Ok(v) => v,
        Err(_) => return "[]".to_string(),
    };
    offload_value(&mut v, threshold);
    v.to_string()
}

/// Rehydrate a parsed results vector in place (engine read seams).
pub fn rehydrate_results(results: &mut [NodeResult]) {
    for r in results.iter_mut() {
        rehydrate_value(&mut r.output);
    }
}

/// Collect blob ids referenced by a `node_results` JSON string into `set`.
pub fn collect_referenced_ids(node_results_json: &str, set: &mut HashSet<String>) {
    if let Ok(v) = serde_json::from_str::<Value>(node_results_json) {
        collect_ids_value(&v, set);
    }
}

fn collect_ids_value(v: &Value, set: &mut HashSet<String>) {
    match v {
        Value::Object(map) => {
            if is_descriptor_map(map) {
                if let Some(id) = map
                    .get(BINARY_KEY)
                    .and_then(|d| d.get("id"))
                    .and_then(|i| i.as_str())
                {
                    set.insert(id.to_string());
                }
                return;
            }
            for c in map.values() {
                collect_ids_value(c, set);
            }
        }
        Value::Array(arr) => {
            for c in arr {
                collect_ids_value(c, set);
            }
        }
        _ => {}
    }
}

/// Delete blob files whose id is not in `referenced`. Returns the count removed.
/// Called by retention after pruning runs, with the id set gathered from every
/// surviving run — so a blob shared by multiple runs is kept until the last one
/// referencing it is pruned.
pub fn gc_unreferenced(referenced: &HashSet<String>) -> usize {
    let dir = blob_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    // Grace window: never delete a blob written in the last hour — an in-flight
    // run may have written its bytes but not yet persisted the referencing row.
    let now = std::time::SystemTime::now();
    let grace = std::time::Duration::from_secs(3600);
    let mut removed = 0;
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if referenced.contains(&name) {
            continue;
        }
        let recent = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|m| now.duration_since(m).map(|a| a < grace).unwrap_or(true))
            .unwrap_or(true);
        if recent {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Test-only serialization guard. `blob_dir()` reads the process-global
/// `AXON_WF_BLOB_DIR`; every test that sets it (here and in `maintenance`) holds
/// this lock so one test's set_var can't redirect another's reads mid-run.
#[cfg(test)]
pub(crate) static BLOB_DIR_TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// Lock the shared guard and point the blob store at a unique temp dir, so
    /// these unit tests never write into a dev instance's real `wf_blobs`.
    fn test_blob_guard() -> std::sync::MutexGuard<'static, ()> {
        let g = BLOB_DIR_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(
            "AXON_WF_BLOB_DIR",
            std::env::temp_dir().join(format!(
                "axon_blob_unit_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )),
        );
        g
    }

    #[test]
    fn offload_then_rehydrate_round_trips_large_strings() {
        let _g = test_blob_guard();
        let big = "x".repeat(2000);
        let mut v = serde_json::json!({
            "small": "keep me",
            "nested": { "huge": big.clone() },
            "list": [ big.clone(), "tiny" ],
        });
        offload_value(&mut v, 1024);

        // Large leaves became descriptors; the small one stayed inline.
        assert_eq!(v["small"], serde_json::json!("keep me"));
        assert!(v["nested"]["huge"].get(BINARY_KEY).is_some());
        assert!(v["list"][0].get(BINARY_KEY).is_some());
        assert_eq!(v["list"][1], serde_json::json!("tiny"));

        // Descriptors carry the original byte size.
        assert_eq!(v["nested"]["huge"][BINARY_KEY]["size"], serde_json::json!(2000));

        rehydrate_value(&mut v);
        assert_eq!(v["nested"]["huge"], serde_json::json!(big));
        assert_eq!(v["list"][0], serde_json::json!(big));
        assert_eq!(v["small"], serde_json::json!("keep me"));
    }

    #[test]
    fn offload_is_idempotent_and_threshold_zero_disables() {
        let _g = test_blob_guard();
        let big = "y".repeat(5000);
        let mut a = serde_json::json!({ "b": big.clone() });
        let mut b = serde_json::json!({ "b": big.clone() });
        offload_value(&mut a, 1024);
        offload_value(&mut b, 1024);
        // Same content → same blob id (content-addressed dedup).
        assert_eq!(a["b"][BINARY_KEY]["id"], b["b"][BINARY_KEY]["id"]);

        // Re-offloading an already-offloaded value is a no-op (no double-wrap).
        let before = a.clone();
        offload_value(&mut a, 1024);
        assert_eq!(a, before);

        // Threshold 0 disables offloading entirely.
        let mut c = serde_json::json!({ "b": big.clone() });
        offload_value(&mut c, 0);
        assert_eq!(c["b"], serde_json::json!(big));

        // referenced-id collection finds the offloaded blob.
        let mut ids = HashSet::new();
        collect_referenced_ids(&a.to_string(), &mut ids);
        assert!(ids.contains(a["b"][BINARY_KEY]["id"].as_str().unwrap()));
    }
}
