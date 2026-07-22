//! Digest (Extract from File) — Task 2.4. Reads a CSV, spreadsheet
//! (XLSX/XLS/XLSB/ODS via calamine's format sniffing), JSON, XML, or plain
//! text file into workflow data, so Myelin-stored files and downloaded
//! attachments become data the list toolkit (Filter/Aggregate/Loop) can chew
//! on. `csv`, `calamine`, and `quick-xml` (shared with the `xml` node) are all
//! pure Rust — no new TLS/HTTP stacks (dependency policy).
//!
//! Three `source`s for the bytes:
//!   - `file`   — a path on disk. Left blank, the node auto-detects the
//!                standard binary descriptor on the incoming item
//!                (`binary.local_path` — what Myelin retrieve, Telegram
//!                download, and Synapse file responses all emit).
//!   - `text`   — raw text content (any format except `xlsx`, which is
//!                binary). This is how a text HTTP fetch arrives: Synapse
//!                returns text bodies as plain strings, not staged files.
//!   - `base64` — base64-encoded bytes (e.g. Synapse's binary `body.body`).
//!
//! Five `operation`s:
//!   - `csv`/`xlsx` — a bare array of row items (objects when `headerRow`,
//!     else arrays) — the list-node convention, so it composes with
//!     Filter/Aggregate/Split Out/Sort-Limit and Loop directly.
//!   - `json` — parsed as-is (an array is already the item list; an object is
//!     one item); the exact inverse of Convert to File's `json` operation.
//!   - `xml` — `{ <rootTag>: value }` via the same parser as the `xml` node
//!     (`nodes::xml::parse_document`, shared rather than duplicated).
//!   - `text` — the whole file as a string, or one array item per line when
//!     `splitLines` is on.

use crate::tools::workflow::{cfg_str, cfg_usize, val_to_string};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Map, Value};

/// Find the standard binary descriptor's `local_path` on the incoming item:
/// direct `local_path`/`localPath` keys, a nested `binary` descriptor, or the
/// first element of a list input.
fn find_local_path(v: &Value) -> Option<String> {
    match v {
        Value::Object(m) => {
            for k in ["local_path", "localPath"] {
                if let Some(s) = m.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()) {
                    return Some(s.to_string());
                }
            }
            m.get("binary").and_then(find_local_path)
        }
        Value::Array(a) => a.first().and_then(find_local_path),
        _ => None,
    }
}

/// Resolve the file bytes per the configured `source`.
fn source_bytes(config: &Value, input: &Value, operation: &str) -> Result<Vec<u8>, String> {
    match config
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("file")
    {
        "text" => {
            if operation == "xlsx" {
                return Err(
                    "Extract from File: a spreadsheet is binary — use the File on Disk \
                            or Base64 source for XLSX/XLS/ODS"
                        .to_string(),
                );
            }
            if let Some(t) = config
                .get("text")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
            {
                return Ok(t.as_bytes().to_vec());
            }
            match input {
                Value::String(s) if !s.trim().is_empty() => Ok(s.clone().into_bytes()),
                _ => Err(
                    "Extract from File: Raw Text source needs content — fill the Text \
                          field (e.g. {{ $node[\"Synapse\"].body }}) or feed a string input"
                        .to_string(),
                ),
            }
        }
        "base64" => {
            let data = cfg_str(config, "data").ok_or(
                "Extract from File: Base64 source needs the Data field \
                 (e.g. {{ $node[\"Synapse\"].body.body }})",
            )?;
            // Tolerate line-wrapped base64 (MIME style) by stripping whitespace.
            let compact: String = data.chars().filter(|c| !c.is_whitespace()).collect();
            STANDARD
                .decode(compact.as_bytes())
                .map_err(|e| format!("Extract from File: invalid base64 data: {e}"))
        }
        _ => {
            let path = cfg_str(config, "filePath")
                .map(str::to_string)
                .or_else(|| find_local_path(input))
                .ok_or(
                    "Extract from File: no file to read — set File Path or feed a node that \
                     outputs a binary descriptor (Myelin retrieve, Telegram download, Synapse \
                     file response)",
                )?;
            std::fs::read(&path)
                .map_err(|e| format!("Extract from File: cannot read '{path}': {e}"))
        }
    }
}

/// Build unique, non-empty column names from a header row. Blank headers become
/// `column_N` (1-based); duplicates get `_2`, `_3`, … suffixes.
fn finalize_headers(raw: Vec<String>) -> Vec<String> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    raw.into_iter()
        .enumerate()
        .map(|(i, h)| {
            let base = if h.trim().is_empty() {
                format!("column_{}", i + 1)
            } else {
                h.trim().to_string()
            };
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                base
            } else {
                format!("{}_{}", base, *n)
            }
        })
        .collect()
}

/// Key for cell `i` when the row is longer than the header row.
fn column_key(headers: &[String], i: usize) -> String {
    headers
        .get(i)
        .cloned()
        .unwrap_or_else(|| format!("column_{}", i + 1))
}

/// Optional CSV type inference: booleans and numbers, with leading-zero
/// strings ("0123" — IDs, phone numbers) deliberately kept as text.
fn infer_type(s: &str) -> Value {
    let t = s.trim();
    if t.eq_ignore_ascii_case("true") {
        return json!(true);
    }
    if t.eq_ignore_ascii_case("false") {
        return json!(false);
    }
    let unsigned = t.strip_prefix('-').unwrap_or(t);
    if unsigned.len() > 1 && unsigned.starts_with('0') && !unsigned.starts_with("0.") {
        return Value::String(s.to_string());
    }
    if unsigned.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        if let Ok(i) = t.parse::<i64>() {
            return json!(i);
        }
        if let Ok(f) = t.parse::<f64>() {
            if f.is_finite() {
                if let Some(n) = serde_json::Number::from_f64(f) {
                    return Value::Number(n);
                }
            }
        }
    }
    Value::String(s.to_string())
}

/// One CSV record → a row item: an object keyed by headers, or a plain array
/// when there is no header row.
fn row_to_value(cells: Vec<String>, headers: Option<&[String]>, infer: bool) -> Value {
    let conv = |s: String| {
        if infer {
            infer_type(&s)
        } else {
            Value::String(s)
        }
    };
    match headers {
        Some(h) => {
            let mut m = Map::new();
            for (i, c) in cells.into_iter().enumerate() {
                m.insert(column_key(h, i), conv(c));
            }
            Value::Object(m)
        }
        None => Value::Array(cells.into_iter().map(conv).collect()),
    }
}

fn parse_csv(bytes: &[u8], config: &Value) -> Result<Value, String> {
    // Excel exports open with a UTF-8 BOM that would pollute the first header.
    let bytes = bytes
        .strip_prefix(b"\xef\xbb\xbf".as_slice())
        .unwrap_or(bytes);

    let delim_cfg = config
        .get("delimiter")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(",");
    let delimiter: u8 = if delim_cfg.eq_ignore_ascii_case("tab") || delim_cfg == "\\t" {
        b'\t'
    } else {
        delim_cfg.as_bytes()[0]
    };
    let header_row = config
        .get("headerRow")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let infer = config
        .get("inferTypes")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_rows = cfg_usize(config, "maxRows").unwrap_or(0);

    // has_headers(false) + manual header handling gives control over blank /
    // duplicate header names; flexible tolerates ragged real-world rows; byte
    // records + lossy UTF-8 keep a stray Latin-1 export from failing the run.
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(bytes);

    let mut headers: Option<Vec<String>> = None;
    let mut rows: Vec<Value> = Vec::new();
    for rec in rdr.byte_records() {
        let rec = rec.map_err(|e| format!("Extract from File: CSV parse error: {e}"))?;
        let cells: Vec<String> = rec
            .iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        if header_row && headers.is_none() {
            headers = Some(finalize_headers(cells));
            continue;
        }
        if cells.iter().all(|c| c.is_empty()) {
            continue; // skip blank lines
        }
        if max_rows > 0 && rows.len() >= max_rows {
            break;
        }
        rows.push(row_to_value(cells, headers.as_deref(), infer));
    }
    Ok(Value::Array(rows))
}

/// One spreadsheet cell → JSON. Integral floats become integers (Excel stores
/// every number as a float — "3" should not surface as 3.0); date cells become
/// naive ISO strings (Excel has no timezone); error cells surface their Excel
/// error text ("#DIV/0!") instead of a silent null.
fn cell_to_value(c: &calamine::Data) -> Value {
    use calamine::Data;
    match c {
        Data::Empty => Value::Null,
        Data::String(s) => json!(s),
        Data::Int(i) => json!(i),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 9.0e15 {
                json!(*f as i64)
            } else {
                json!(f)
            }
        }
        Data::Bool(b) => json!(b),
        Data::DateTime(dt) => match dt.as_datetime() {
            Some(ndt) => json!(ndt.format("%Y-%m-%dT%H:%M:%S").to_string()),
            None => json!(dt.as_f64()),
        },
        Data::DateTimeIso(s) | Data::DurationIso(s) => json!(s),
        Data::Error(e) => json!(e.to_string()),
    }
}

fn parse_spreadsheet(bytes: Vec<u8>, config: &Value) -> Result<Value, String> {
    use calamine::{Data, Reader};

    let cursor = std::io::Cursor::new(bytes);
    let mut wb = calamine::open_workbook_auto_from_rs(cursor)
        .map_err(|e| format!("Extract from File: cannot open spreadsheet: {e}"))?;

    let sheet = match cfg_str(config, "sheetName") {
        Some(s) => s.to_string(),
        None => wb
            .sheet_names()
            .first()
            .cloned()
            .ok_or("Extract from File: the workbook has no sheets")?,
    };
    let range = wb.worksheet_range(&sheet).map_err(|e| {
        format!(
            "Extract from File: cannot read sheet '{sheet}': {e} (sheets: {})",
            wb.sheet_names().join(", ")
        )
    })?;

    let header_row = config
        .get("headerRow")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let max_rows = cfg_usize(config, "maxRows").unwrap_or(0);

    let mut headers: Option<Vec<String>> = None;
    let mut rows: Vec<Value> = Vec::new();
    for row in range.rows() {
        if row.iter().all(|c| matches!(c, Data::Empty)) {
            continue; // used ranges can include fully-blank rows
        }
        if header_row && headers.is_none() {
            headers = Some(finalize_headers(
                row.iter()
                    .map(|c| val_to_string(&cell_to_value(c)))
                    .collect(),
            ));
            continue;
        }
        if max_rows > 0 && rows.len() >= max_rows {
            break;
        }
        match &headers {
            Some(h) => {
                let mut m = Map::new();
                for (i, c) in row.iter().enumerate() {
                    m.insert(column_key(h, i), cell_to_value(c));
                }
                rows.push(Value::Object(m));
            }
            None => rows.push(Value::Array(row.iter().map(cell_to_value).collect())),
        }
    }
    Ok(Value::Array(rows))
}

/// Raw text extraction (lossy UTF-8 — a stray Latin-1 export doesn't fail the
/// run, same tolerance as CSV). `splitLines` breaks it into one array item
/// per line — the list-node shape, so it composes with Filter/Loop — capped
/// by `maxRows` when set; off (the default) returns the whole file as a
/// single string, the natural shape for a config file or message body.
fn parse_text(bytes: &[u8], config: &Value) -> Value {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let split = config
        .get("splitLines")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !split {
        return Value::String(text);
    }
    let mut lines: Vec<Value> = text.lines().map(|l| json!(l)).collect();
    let max_rows = cfg_usize(config, "maxRows").unwrap_or(0);
    if max_rows > 0 && lines.len() > max_rows {
        lines.truncate(max_rows);
    }
    Value::Array(lines)
}

/// Parse the bytes as a JSON document, as-is — an array is the (optionally
/// `maxRows`-capped) list of items, an object is a single item, matching
/// Convert to File's `json` operation (its exact inverse).
fn parse_json(bytes: &[u8], config: &Value) -> Result<Value, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| format!("Extract from File: JSON file is not valid UTF-8: {e}"))?;
    let mut value: Value = serde_json::from_str(text)
        .map_err(|e| format!("Extract from File: JSON parse error: {e}"))?;
    let max_rows = cfg_usize(config, "maxRows").unwrap_or(0);
    if max_rows > 0 {
        if let Value::Array(arr) = &mut value {
            arr.truncate(max_rows);
        }
    }
    Ok(value)
}

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("csv");
    let bytes = source_bytes(config, input, operation)?;
    match operation {
        "csv" => parse_csv(&bytes, config),
        "xlsx" => parse_spreadsheet(bytes, config),
        "json" => parse_json(&bytes, config),
        "text" => Ok(parse_text(&bytes, config)),
        "xml" => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            super::xml::parse_document(&text).map_err(|e| format!("Extract from File: {e}"))
        }
        other => Err(format!("Unknown Extract from File format: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv_cfg(text: &str, extra: Value) -> Value {
        let mut c = json!({ "operation": "csv", "source": "text", "text": text });
        if let (Some(obj), Some(ex)) = (c.as_object_mut(), extra.as_object()) {
            for (k, v) in ex {
                obj.insert(k.clone(), v.clone());
            }
        }
        c
    }

    // Header row keys each record; values stay strings by default.
    #[test]
    fn csv_with_headers_keys_rows() {
        let out = execute(
            &csv_cfg("name,age\nAda,36\nGrace,45\n", json!({})),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(
            out,
            json!([{ "name": "Ada", "age": "36" }, { "name": "Grace", "age": "45" }])
        );
    }

    // headerRow=false emits plain value arrays.
    #[test]
    fn csv_without_headers_emits_arrays() {
        let out = execute(
            &csv_cfg("a,b\nc,d\n", json!({ "headerRow": false })),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(out, json!([["a", "b"], ["c", "d"]]));
    }

    // inferTypes converts numerics/booleans but keeps leading-zero IDs as text.
    #[test]
    fn csv_infer_types() {
        let out = execute(
            &csv_cfg(
                "id,qty,price,ok,phone\nx1,3,9.75,true,0917\n",
                json!({ "inferTypes": true }),
            ),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(
            out,
            json!([{ "id": "x1", "qty": 3, "price": 9.75, "ok": true, "phone": "0917" }])
        );
    }

    // Custom delimiter (semicolon) and the "tab" alias both work.
    #[test]
    fn csv_custom_delimiters() {
        let semi = execute(
            &csv_cfg("a;b\n1;2\n", json!({ "delimiter": ";" })),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(semi, json!([{ "a": "1", "b": "2" }]));
        let tab = execute(
            &csv_cfg("a\tb\n1\t2\n", json!({ "delimiter": "tab" })),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(tab, json!([{ "a": "1", "b": "2" }]));
    }

    // Blank/duplicate headers get stable synthesized names; ragged rows and
    // blank lines don't error.
    #[test]
    fn csv_messy_headers_and_ragged_rows() {
        let out = execute(
            &csv_cfg("name,,name\nAda,x,y,extra\n\nGrace\n", json!({})),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(
            out,
            json!([
                { "name": "Ada", "column_2": "x", "name_2": "y", "column_4": "extra" },
                { "name": "Grace" },
            ])
        );
    }

    // A UTF-8 BOM never pollutes the first header name.
    #[test]
    fn csv_strips_bom() {
        let out = execute(&csv_cfg("\u{feff}name\nAda\n", json!({})), &Value::Null).unwrap();
        assert_eq!(out, json!([{ "name": "Ada" }]));
    }

    // maxRows caps the output.
    #[test]
    fn csv_max_rows_caps() {
        let out = execute(
            &csv_cfg("n\n1\n2\n3\n4\n", json!({ "maxRows": 2 })),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(out, json!([{ "n": "1" }, { "n": "2" }]));
    }

    // Quoted fields keep embedded delimiters and newlines.
    #[test]
    fn csv_quoted_fields() {
        let out = execute(
            &csv_cfg("note,who\n\"a, b\nc\",Ada\n", json!({})),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(out, json!([{ "note": "a, b\nc", "who": "Ada" }]));
    }

    // Blank text source with a string input falls back to the input.
    #[test]
    fn csv_text_source_falls_back_to_string_input() {
        let cfg = json!({ "operation": "csv", "source": "text" });
        let out = execute(&cfg, &json!("k\nv\n")).unwrap();
        assert_eq!(out, json!([{ "k": "v" }]));
    }

    // Text source for a spreadsheet is a teaching error.
    #[test]
    fn spreadsheet_text_source_errors() {
        let cfg = json!({ "operation": "xlsx", "source": "text", "text": "a,b" });
        let err = execute(&cfg, &Value::Null).unwrap_err();
        assert!(err.contains("binary"), "got: {err}");
    }

    // File source: read a real file from disk via filePath.
    #[test]
    fn csv_file_source_reads_from_disk() {
        let path = std::env::temp_dir().join(format!(
            "axon_extract_test_{}_{}.csv",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "h\nrow\n").unwrap();
        let cfg =
            json!({ "operation": "csv", "source": "file", "filePath": path.to_string_lossy() });
        let out = execute(&cfg, &Value::Null).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(out, json!([{ "h": "row" }]));
    }

    // File source auto-detects the standard binary descriptor on the input.
    #[test]
    fn file_source_autodetects_binary_descriptor() {
        let path = std::env::temp_dir().join(format!(
            "axon_extract_auto_{}_{}.csv",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "h\nauto\n").unwrap();
        let input =
            json!({ "binary": { "local_path": path.to_string_lossy(), "file_name": "x.csv" } });
        let cfg = json!({ "operation": "csv", "source": "file" });
        let out = execute(&cfg, &input).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(out, json!([{ "h": "auto" }]));
    }

    // A missing file errors with the path named.
    #[test]
    fn missing_file_errors_with_path() {
        let cfg =
            json!({ "operation": "csv", "source": "file", "filePath": "Z:/nope/missing.csv" });
        let err = execute(&cfg, &Value::Null).unwrap_err();
        assert!(err.contains("missing.csv"), "got: {err}");
    }

    // No path and no descriptor is a teaching error.
    #[test]
    fn no_file_source_errors() {
        let cfg = json!({ "operation": "csv", "source": "file" });
        let err = execute(&cfg, &Value::Null).unwrap_err();
        assert!(err.contains("binary descriptor"), "got: {err}");
    }

    // Base64 source decodes (line wraps tolerated) and parses.
    #[test]
    fn csv_base64_source() {
        let b64 = STANDARD.encode("n,v\na,1\n");
        // Insert a line wrap to mimic MIME-style base64.
        let wrapped = format!("{}\n{}", &b64[..4], &b64[4..]);
        let cfg = json!({ "operation": "csv", "source": "base64", "data": wrapped });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!([{ "n": "a", "v": "1" }]));
    }

    // ---- Spreadsheet (XLSX) ----
    // A minimal hand-crafted XLSX (inline strings, one "Data" sheet):
    //   name  | qty | price
    //   widget|  3  | 9.5
    //   gadget|  4  | 2.25
    // Base64 generated once from the zipped OOXML parts; decoded per-test.
    // Regenerate with the parts in this repo's history if columns must change.
    const XLSX_B64: &str = "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";

    fn xlsx_cfg(extra: Value) -> Value {
        let mut c = json!({ "operation": "xlsx", "source": "base64", "data": XLSX_B64 });
        if let (Some(obj), Some(ex)) = (c.as_object_mut(), extra.as_object()) {
            for (k, v) in ex {
                obj.insert(k.clone(), v.clone());
            }
        }
        c
    }

    // XLSX rows become objects; integral floats surface as integers.
    #[test]
    fn xlsx_with_headers() {
        let out = execute(&xlsx_cfg(json!({})), &Value::Null).unwrap();
        assert_eq!(
            out,
            json!([
                { "name": "widget", "qty": 3, "price": 9.5 },
                { "name": "gadget", "qty": 4, "price": 2.25 },
            ])
        );
    }

    // headerRow=false emits raw cell arrays including the header line.
    #[test]
    fn xlsx_without_headers() {
        let out = execute(&xlsx_cfg(json!({ "headerRow": false })), &Value::Null).unwrap();
        assert_eq!(
            out,
            json!([
                ["name", "qty", "price"],
                ["widget", 3, 9.5],
                ["gadget", 4, 2.25],
            ])
        );
    }

    // Naming a sheet that doesn't exist errors and lists the real sheets.
    #[test]
    fn xlsx_unknown_sheet_errors_listing_sheets() {
        let err = execute(&xlsx_cfg(json!({ "sheetName": "Nope" })), &Value::Null).unwrap_err();
        assert!(err.contains("Data"), "got: {err}");
    }

    // maxRows applies to spreadsheets too.
    #[test]
    fn xlsx_max_rows_caps() {
        let out = execute(&xlsx_cfg(json!({ "maxRows": 1 })), &Value::Null).unwrap();
        assert_eq!(out, json!([{ "name": "widget", "qty": 3, "price": 9.5 }]));
    }

    // Garbage bytes are a clean error, not a panic.
    #[test]
    fn xlsx_garbage_bytes_error() {
        let cfg = json!({
            "operation": "xlsx",
            "source": "base64",
            "data": STANDARD.encode("this is not a zip"),
        });
        let err = execute(&cfg, &Value::Null).unwrap_err();
        assert!(err.contains("cannot open spreadsheet"), "got: {err}");
    }

    // ---- JSON ----

    // An array is already the item list.
    #[test]
    fn json_array_source_text() {
        let cfg = json!({ "operation": "json", "source": "text", "text": "[{\"a\":1},{\"a\":2}]" });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!([{ "a": 1 }, { "a": 2 }]));
    }

    // A bare object stays a single item, not wrapped in an array.
    #[test]
    fn json_object_stays_single_item() {
        let cfg = json!({ "operation": "json", "source": "text", "text": "{\"a\":1}" });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "a": 1 }));
    }

    // maxRows truncates a top-level array.
    #[test]
    fn json_max_rows_caps_array() {
        let cfg = json!({
            "operation": "json",
            "source": "text",
            "text": "[1,2,3,4]",
            "maxRows": 2,
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!([1, 2]));
    }

    // Base64 source works for JSON too.
    #[test]
    fn json_base64_source() {
        let cfg = json!({
            "operation": "json",
            "source": "base64",
            "data": STANDARD.encode("{\"ok\":true}"),
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "ok": true }));
    }

    // Malformed JSON is a clean error, not a panic.
    #[test]
    fn json_malformed_errors() {
        let cfg = json!({ "operation": "json", "source": "text", "text": "{not json" });
        let err = execute(&cfg, &Value::Null).unwrap_err();
        assert!(err.contains("JSON parse error"), "got: {err}");
    }

    // ---- Text ----

    // Default: the whole file as one string.
    #[test]
    fn text_whole_file_as_string() {
        let cfg = json!({ "operation": "text", "source": "text", "text": "line one\nline two" });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!("line one\nline two"));
    }

    // splitLines breaks it into one item per line.
    #[test]
    fn text_split_lines() {
        let cfg = json!({
            "operation": "text",
            "source": "text",
            "text": "a\nb\nc",
            "splitLines": true,
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!(["a", "b", "c"]));
    }

    // maxRows caps split lines.
    #[test]
    fn text_split_lines_max_rows_caps() {
        let cfg = json!({
            "operation": "text",
            "source": "text",
            "text": "a\nb\nc",
            "splitLines": true,
            "maxRows": 2,
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!(["a", "b"]));
    }

    // File source works for text too (previously CSV-only).
    #[test]
    fn text_file_source_reads_from_disk() {
        let path = std::env::temp_dir().join(format!(
            "axon_extract_text_{}_{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "hello file").unwrap();
        let cfg =
            json!({ "operation": "text", "source": "file", "filePath": path.to_string_lossy() });
        let out = execute(&cfg, &Value::Null).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(out, json!("hello file"));
    }

    // ---- XML ----

    // Delegates to the shared xml::parse_document parser.
    #[test]
    fn xml_parses_via_shared_parser() {
        let cfg = json!({
            "operation": "xml",
            "source": "text",
            "text": "<note><to>Ada</to></note>",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "note": { "to": "Ada" } }));
    }

    // Base64 source works for XML too.
    #[test]
    fn xml_base64_source() {
        let cfg = json!({
            "operation": "xml",
            "source": "base64",
            "data": STANDARD.encode("<a>hi</a>"),
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "a": "hi" }));
    }

    // Malformed XML surfaces a clean, prefixed error.
    #[test]
    fn xml_malformed_errors() {
        let cfg = json!({ "operation": "xml", "source": "text", "text": "<a><b></a>" });
        let err = execute(&cfg, &Value::Null).unwrap_err();
        assert!(err.starts_with("Extract from File:"), "got: {err}");
    }

    // A raw-text source now works for any non-binary operation.
    #[test]
    fn text_source_allowed_for_json_and_xml() {
        let json_cfg = json!({ "operation": "json", "source": "text", "text": "[1]" });
        assert_eq!(execute(&json_cfg, &Value::Null).unwrap(), json!([1]));
        let xml_cfg = json!({ "operation": "xml", "source": "text", "text": "<a/>" });
        assert_eq!(execute(&xml_cfg, &Value::Null).unwrap(), json!({ "a": "" }));
    }

    // A raw-text source is still rejected for the binary xlsx operation.
    #[test]
    fn text_source_still_rejected_for_xlsx() {
        let cfg = json!({ "operation": "xlsx", "source": "text", "text": "a,b" });
        let err = execute(&cfg, &Value::Null).unwrap_err();
        assert!(err.contains("binary"), "got: {err}");
    }
}
