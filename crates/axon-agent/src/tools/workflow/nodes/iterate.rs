use crate::tools::workflow::{cfg_usize, extract_items_for_loop};
use serde_json::{json, Value};

pub(crate) fn execute(config: &Value) -> Result<Value, String> {
    let raw_items = config.get("items").cloned().unwrap_or(Value::Null);
    let array_path = config.get("array_path").and_then(|v| v.as_str());
    let items = extract_items_for_loop(&raw_items, array_path)?;

    // Concurrency + batching knobs the engine reads when fanning the
    // downstream body out over these items. parallelism>1 runs iterations
    // concurrently (a real win over n8n's single-threaded JS executor);
    // batch_size>1 hands each iteration a slice of items at once.
    let parallelism = cfg_usize(config, "parallelism").unwrap_or(1).max(1);
    let batch_size = cfg_usize(config, "batch_size").unwrap_or(1).max(1);
    // Safety cap so a malformed expression can't fan out millions of runs.
    // Default 100k; 0 (or unset) keeps the default, any positive overrides.
    let max_iterations = cfg_usize(config, "max_iterations")
        .filter(|n| *n > 0)
        .unwrap_or(100_000);

    if items.len() > max_iterations {
        return Err(format!(
            "Loop node: {} items exceeds max_iterations ({}). Raise 'Max Iterations' if this is intentional.",
            items.len(),
            max_iterations
        ));
    }

    Ok(json!({
        "_axon_loop": {
            "enabled": true,
            "count": items.len(),
            "parallelism": parallelism,
            "batch_size": batch_size
        },
        "items": items,
        "count": items.len(),
        "total": items.len(),
        "index": -1,
        "current": Value::Null
    }))
}
