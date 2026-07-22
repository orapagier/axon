//! Convert to File — Task 2.5. The inverse of Digest (Extract from File):
//! turns workflow JSON into a **staged file** plus the standard binary
//! descriptor (`binary.local_path`, both key conventions), so the result can
//! feed anything that ships files — Telegram send, Gmail attachment, SSH /
//! Drive / OneDrive upload, Myelin store. Zero new crates: `csv` is already in
//! tree (2.4) and JSON/text/base64 are std + serde (dependency policy).
//!
//! Four `operation`s:
//!   - `csv` — a list of items → one CSV row each. Object items are keyed by
//!     a header union (first-seen order); scalar items land in a `value`
//!     column; a list of arrays writes positional rows verbatim.
//!   - `json` — serialize the data as-is (pretty by default).
//!   - `text` — a string as-is; a list joins one item per line.
//!   - `fromBase64` — decode base64 into raw bytes (n8n's "move base64 string
//!     to file"), e.g. a Synapse binary `body.body`.
//!
//! XLSX *output* is deliberately deferred: writing it needs a new crate
//! (`rust_xlsxwriter` + zip) and CSV already opens in Excel — build it when a
//! workflow actually needs styled sheets (pairs with 2.6 Compression's `zip`).
//!
//! The `data` config expression is the source; blank falls back to the primary
//! input (the list-node convention). Staging overwrites a same-named file, so
//! re-runs keep only the newest copy — same rule as every other producer.

use crate::tools::telegram::binary_descriptor;
use crate::tools::workflow::{cfg_str, to_items, val_to_string};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

/// The value to convert: the `data` expression when it resolved to something,
/// else the primary input. Expression results keep their JSON type through
/// `interpolate_config`, so an object/array rides through as-is.
fn source_value<'a>(config: &'a Value, input: &'a Value) -> &'a Value {
    match config.get("data") {
        None | Some(Value::Null) => input,
        Some(Value::String(s)) if s.trim().is_empty() => input,
        Some(v) => v,
    }
}

/// One cell: strings verbatim, numbers/bools via the shared stringifier
/// (integral floats print as integers), null/missing empty, nested
/// objects/arrays as compact JSON. The csv writer handles quoting.
fn cell(v: Option<&Value>) -> String {
    v.map(val_to_string).unwrap_or_default()
}

fn build_csv(items: &[Value], config: &Value) -> Result<Vec<u8>, String> {
    let delim_cfg = cfg_str(config, "delimiter").unwrap_or(",");
    let delimiter: u8 = if delim_cfg.eq_ignore_ascii_case("tab") || delim_cfg == "\\t" {
        b'\t'
    } else {
        delim_cfg.as_bytes()[0]
    };
    let header_row = config
        .get("headerRow")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let mut wtr = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .from_writer(Vec::new());

    // A list of arrays is already rows — write them positionally (a header
    // would be meaningless; array items carry no key names).
    if !items.is_empty() && items.iter().all(|i| i.is_array()) {
        for item in items {
            let cells: Vec<String> = item
                .as_array()
                .expect("checked all items are arrays")
                .iter()
                .map(|c| cell(Some(c)))
                .collect();
            if !cells.is_empty() {
                wtr.write_record(&cells)
                    .map_err(|e| format!("Convert to File: CSV write error: {e}"))?;
            }
        }
        return wtr
            .into_inner()
            .map_err(|e| format!("Convert to File: CSV write error: {e}"));
    }

    // Header union across items in first-seen order (serde_json's map keeps a
    // deterministic key order per item); non-object items get a `value` column.
    let mut headers: Vec<String> = Vec::new();
    for item in items {
        match item {
            Value::Object(m) => {
                for k in m.keys() {
                    if !headers.iter().any(|h| h == k) {
                        headers.push(k.clone());
                    }
                }
            }
            _ => {
                if !headers.iter().any(|h| h == "value") {
                    headers.push("value".to_string());
                }
            }
        }
    }
    if headers.is_empty() {
        // No items (or only empty objects): an empty file, not an error — a
        // pipeline legitimately produces zero rows some days.
        return Ok(Vec::new());
    }

    if header_row {
        wtr.write_record(&headers)
            .map_err(|e| format!("Convert to File: CSV write error: {e}"))?;
    }
    for item in items {
        let cells: Vec<String> = match item {
            Value::Object(m) => headers.iter().map(|h| cell(m.get(h))).collect(),
            other => headers
                .iter()
                .map(|h| {
                    if h == "value" {
                        cell(Some(other))
                    } else {
                        String::new()
                    }
                })
                .collect(),
        };
        wtr.write_record(&cells)
            .map_err(|e| format!("Convert to File: CSV write error: {e}"))?;
    }
    wtr.into_inner()
        .map_err(|e| format!("Convert to File: CSV write error: {e}"))
}

/// Build the file bytes + default MIME type per operation. Pure — staging is
/// the only side effect and lives in `execute`.
fn build_bytes(
    config: &Value,
    input: &Value,
    operation: &str,
) -> Result<(Vec<u8>, &'static str), String> {
    let data = source_value(config, input);
    match operation {
        "csv" => {
            if data.is_null() {
                return Err("Convert to File: nothing to convert — fill the Data field \
                     (e.g. {{ $node[\"Filter\"] }}) or feed a node output"
                    .to_string());
            }
            let items = to_items(data, config.get("arrayPath").and_then(|v| v.as_str()));
            let mut bytes = Vec::new();
            // Excel needs a UTF-8 BOM to open non-ASCII text correctly.
            if config.get("bom").and_then(|v| v.as_bool()).unwrap_or(false) {
                bytes.extend_from_slice(b"\xef\xbb\xbf");
            }
            bytes.extend(build_csv(&items, config)?);
            Ok((bytes, "text/csv"))
        }
        "json" => {
            if data.is_null() {
                return Err("Convert to File: nothing to convert — fill the Data field \
                     or feed a node output"
                    .to_string());
            }
            let pretty = config
                .get("pretty")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let s = if pretty {
                serde_json::to_string_pretty(data)
            } else {
                serde_json::to_string(data)
            }
            .map_err(|e| format!("Convert to File: JSON serialize error: {e}"))?;
            Ok((s.into_bytes(), "application/json"))
        }
        "text" => {
            let s = match data {
                Value::Null => {
                    return Err("Convert to File: nothing to convert — fill the Data field \
                         or feed a node output"
                        .to_string())
                }
                // A list writes one item per line — the natural text shape for
                // list-node output.
                Value::Array(a) => a.iter().map(val_to_string).collect::<Vec<_>>().join("\n"),
                other => val_to_string(other),
            };
            Ok((s.into_bytes(), "text/plain"))
        }
        "fromBase64" => {
            let b64 = match data {
                Value::String(s) if !s.trim().is_empty() => s.as_str(),
                _ => {
                    return Err("Convert to File: From Base64 needs a base64 string — fill \
                         the Data field (e.g. {{ $node[\"Synapse\"].body.body }})"
                        .to_string())
                }
            };
            // Tolerate line-wrapped base64 (MIME style) by stripping whitespace.
            let compact: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
            let bytes = STANDARD
                .decode(compact.as_bytes())
                .map_err(|e| format!("Convert to File: invalid base64 data: {e}"))?;
            Ok((bytes, "application/octet-stream"))
        }
        other => Err(format!("Unknown Convert to File format: {other}")),
    }
}

/// The staged file's name: `fileName` config (sanitized), else a per-operation
/// default. CSV/JSON/text auto-append their extension when missing; From
/// Base64 leaves the name alone (the user may deliberately want none).
fn resolve_file_name(config: &Value, operation: &str) -> String {
    let default = match operation {
        "json" => "data.json",
        "text" => "data.txt",
        "fromBase64" => "file.bin",
        _ => "data.csv",
    };
    let name = crate::files::sanitize_filename(cfg_str(config, "fileName").unwrap_or(default));
    let has_ext = std::path::Path::new(&name)
        .extension()
        .map(|e| !e.is_empty())
        .unwrap_or(false);
    if has_ext || operation == "fromBase64" {
        return name;
    }
    let ext = match operation {
        "json" => "json",
        "text" => "txt",
        _ => "csv",
    };
    format!("{name}.{ext}")
}

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("csv");
    let (bytes, default_mime) = build_bytes(config, input, operation)?;
    let file_name = resolve_file_name(config, operation);
    let mime = cfg_str(config, "mimeType").unwrap_or(default_mime);

    let path = crate::files::stage_bytes(&bytes, &file_name)
        .map_err(|e| format!("Convert to File: failed to stage '{file_name}': {e}"))?;
    let local_path = path.to_string_lossy().into_owned();

    // Same output shape as Myelin store/retrieve: file facts at the top level
    // plus the standardized descriptor every downstream consumer resolves.
    Ok(json!({
        "filename": file_name,
        "mime_type": mime,
        "size": bytes.len(),
        "binary": binary_descriptor(&local_path, &file_name, mime, bytes.len()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv_bytes(config: Value, input: Value) -> String {
        let (bytes, mime) = build_bytes(&config, &input, "csv").unwrap();
        assert_eq!(mime, "text/csv");
        String::from_utf8(bytes).unwrap()
    }

    // Object items: header union in first-seen order; a field missing from one
    // item is an empty cell, not an error.
    #[test]
    fn csv_objects_union_headers() {
        let out = csv_bytes(
            json!({}),
            json!([
                { "age": 36, "name": "Ada" },
                { "name": "Grace", "role": "admiral" },
            ]),
        );
        assert_eq!(out, "age,name,role\n36,Ada,\n,Grace,admiral\n");
    }

    // headerRow=false writes data rows only.
    #[test]
    fn csv_no_header_row() {
        let out = csv_bytes(
            json!({ "headerRow": false }),
            json!([{ "a": 1 }, { "a": 2 }]),
        );
        assert_eq!(out, "1\n2\n");
    }

    // Custom delimiter (semicolon) and the "tab" alias both work.
    #[test]
    fn csv_custom_delimiters() {
        let semi = csv_bytes(json!({ "delimiter": ";" }), json!([{ "a": 1, "b": 2 }]));
        assert_eq!(semi, "a;b\n1;2\n");
        let tab = csv_bytes(json!({ "delimiter": "tab" }), json!([{ "a": 1, "b": 2 }]));
        assert_eq!(tab, "a\tb\n1\t2\n");
    }

    // A list of arrays is written positionally, no header.
    #[test]
    fn csv_array_rows_verbatim() {
        let out = csv_bytes(json!({}), json!([["h1", "h2"], [1, 2], [3, 4]]));
        assert_eq!(out, "h1,h2\n1,2\n3,4\n");
    }

    // Scalar items land in a `value` column; a bare object is one row.
    #[test]
    fn csv_scalars_and_bare_object() {
        let scalars = csv_bytes(json!({}), json!(["x", 7, true]));
        assert_eq!(scalars, "value\nx\n7\ntrue\n");
        let bare = csv_bytes(json!({}), json!({ "n": "solo" }));
        assert_eq!(bare, "n\nsolo\n");
    }

    // Nested values serialize as JSON in the cell; embedded delimiters/newlines
    // get quoted by the csv writer; null is an empty cell. (serde_json maps
    // iterate keys alphabetically, hence the gone/note/tags column order.)
    #[test]
    fn csv_nested_quoted_and_null() {
        let out = csv_bytes(
            json!({}),
            json!([{ "note": "a, b\nc", "tags": ["x"], "gone": null }]),
        );
        assert_eq!(out, "gone,note,tags\n,\"a, b\nc\",\"[\"\"x\"\"]\"\n");
    }

    // The BOM option prefixes the bytes for Excel.
    #[test]
    fn csv_bom_prefix() {
        let (bytes, _) = build_bytes(&json!({ "bom": true }), &json!([{ "a": 1 }]), "csv").unwrap();
        assert!(bytes.starts_with(b"\xef\xbb\xbf"));
        assert_eq!(&bytes[3..], b"a\n1\n");
    }

    // arrayPath unwraps a wrapper object.
    #[test]
    fn csv_array_path_unwraps() {
        let out = csv_bytes(
            json!({ "arrayPath": "results" }),
            json!({ "results": [{ "a": 1 }] }),
        );
        assert_eq!(out, "a\n1\n");
    }

    // An empty list is an empty file (a legit zero-row day), but a Null input
    // with no data expression is a teaching error.
    #[test]
    fn csv_empty_vs_missing() {
        let (bytes, _) = build_bytes(&json!({}), &json!([]), "csv").unwrap();
        assert!(bytes.is_empty());
        let err = build_bytes(&json!({}), &Value::Null, "csv").unwrap_err();
        assert!(err.contains("Data"), "got: {err}");
    }

    // The data expression overrides the input.
    #[test]
    fn data_expression_overrides_input() {
        let out = csv_bytes(
            json!({ "data": [{ "picked": 1 }] }),
            json!([{ "ignored": 2 }]),
        );
        assert_eq!(out, "picked\n1\n");
    }

    // JSON: pretty by default, compact on pretty=false.
    #[test]
    fn json_pretty_and_compact() {
        let input = json!({ "b": 1 });
        let (pretty, mime) = build_bytes(&json!({}), &input, "json").unwrap();
        assert_eq!(mime, "application/json");
        assert_eq!(String::from_utf8(pretty).unwrap(), "{\n  \"b\": 1\n}");
        let (compact, _) = build_bytes(&json!({ "pretty": false }), &input, "json").unwrap();
        assert_eq!(String::from_utf8(compact).unwrap(), "{\"b\":1}");
    }

    // Text: a string as-is; a list joins one item per line.
    #[test]
    fn text_string_and_list() {
        let (s, mime) = build_bytes(&json!({}), &json!("hello\nworld"), "text").unwrap();
        assert_eq!(mime, "text/plain");
        assert_eq!(String::from_utf8(s).unwrap(), "hello\nworld");
        let (lines, _) = build_bytes(&json!({}), &json!(["a", 2, true]), "text").unwrap();
        assert_eq!(String::from_utf8(lines).unwrap(), "a\n2\ntrue");
    }

    // fromBase64 decodes (line wraps tolerated); garbage is a clean error.
    #[test]
    fn from_base64_decodes() {
        let b64 = STANDARD.encode("raw bytes");
        let wrapped = format!("{}\n{}", &b64[..4], &b64[4..]);
        let (bytes, mime) =
            build_bytes(&json!({ "data": wrapped }), &Value::Null, "fromBase64").unwrap();
        assert_eq!(mime, "application/octet-stream");
        assert_eq!(bytes, b"raw bytes");
        let err = build_bytes(&json!({ "data": "!!!" }), &Value::Null, "fromBase64").unwrap_err();
        assert!(err.contains("base64"), "got: {err}");
    }

    // File names: default per op, extension auto-append (not for fromBase64),
    // and path characters sanitized.
    #[test]
    fn file_name_resolution() {
        assert_eq!(resolve_file_name(&json!({}), "csv"), "data.csv");
        assert_eq!(resolve_file_name(&json!({}), "json"), "data.json");
        assert_eq!(resolve_file_name(&json!({}), "fromBase64"), "file.bin");
        assert_eq!(
            resolve_file_name(&json!({ "fileName": "report" }), "csv"),
            "report.csv"
        );
        assert_eq!(
            resolve_file_name(&json!({ "fileName": "raw" }), "fromBase64"),
            "raw"
        );
        assert_eq!(
            resolve_file_name(&json!({ "fileName": "../evil.csv" }), "csv"),
            ".._evil.csv"
        );
    }

    // End-to-end: execute stages the file and emits the standard descriptor
    // (both key conventions) with the bytes really on disk.
    #[test]
    fn execute_stages_and_describes() {
        let name = format!(
            "axon_convert_test_{}_{}.csv",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let cfg = json!({ "operation": "csv", "fileName": name });
        let out = execute(&cfg, &json!([{ "a": 1 }])).unwrap();
        assert_eq!(out["filename"], json!(name));
        assert_eq!(out["mime_type"], json!("text/csv"));
        assert_eq!(out["size"], json!(4));
        let binary = &out["binary"];
        let path = binary["local_path"].as_str().unwrap().to_string();
        assert_eq!(binary["localPath"], binary["local_path"]);
        assert_eq!(binary["file_name"], json!(name));
        assert_eq!(binary["mimeType"], json!("text/csv"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\n1\n");
        std::fs::remove_file(&path).ok();
    }
}
