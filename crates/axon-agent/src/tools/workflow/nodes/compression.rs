//! Compression — Task 2.6. zip/unzip/gzip archive utilities. Both crates are
//! promotions of dependencies already resolved transitively (`zip` via
//! calamine's XLSX reader in 2.4, `flate2` via `zip`'s deflate backend and
//! reqwest's response decompression) — same `zlib-rs` pure-Rust backend both
//! ways, so `cargo tree` shows no new compile weight (dependency policy).
//!
//! Four `operation`s:
//!   - `zip`    — bundle one or more input items into a single .zip archive.
//!                Each item resolves via its standard binary descriptor
//!                (`binary.local_path` — Convert to File / Myelin / Telegram
//!                download shape) when present, else a string is written as
//!                text and anything else as compact JSON.
//!   - `unzip`  — explode a .zip archive into its entries, one staged file
//!                each. **Output is a bare array** (the list-node convention)
//!                so it composes with Loop/Filter/Split Out downstream.
//!   - `gzip`   — gzip-compress a single value (a staged file, a string, or
//!                JSON) into one .gz file; embeds the original filename in
//!                the gzip header like the standard `gzip` CLI does.
//!   - `gunzip` — gzip-decompress a single .gz file/bytes back to its
//!                original content, recovering the embedded filename when
//!                present.
//!
//! `unzip`/`gunzip` share Extract from File's `file`/`base64` source
//! convention (auto-detecting the incoming binary descriptor, or an explicit
//! `filePath`/`data` config) — no `text` source, since archive bytes are
//! never meaningfully "raw text".

use crate::tools::telegram::binary_descriptor;
use crate::tools::workflow::cfg_usize;
use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::{Compression, GzBuilder};
use serde_json::{json, Value};
use std::io::{Read, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Non-blank trimmed string config value.
fn cfg_str<'a>(config: &'a Value, key: &str) -> Option<&'a str> {
    config
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Find the standard binary descriptor's `local_path` + a display file name
/// on a value: direct `local_path`/`localPath` + `file_name`/`fileName`
/// keys, or a nested `binary` descriptor (same probing order as Extract from
/// File / Convert to File). The name falls back to the path's basename when
/// no name field is present.
fn find_file_descriptor(v: &Value) -> Option<(String, String)> {
    match v {
        Value::Object(m) => {
            let path = ["local_path", "localPath"]
                .iter()
                .find_map(|k| m.get(*k).and_then(|x| x.as_str()))
                .filter(|s| !s.is_empty());
            if let Some(path) = path {
                let name = ["file_name", "fileName", "original_name", "filename"]
                    .iter()
                    .find_map(|k| m.get(*k).and_then(|x| x.as_str()))
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        std::path::Path::new(path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "file".to_string())
                    });
                return Some((path.to_string(), name));
            }
            m.get("binary").and_then(find_file_descriptor)
        }
        _ => None,
    }
}

/// Resolve archive/gzip bytes per the configured `source`: `file` (default —
/// an explicit `filePath`, else auto-detect the incoming binary descriptor)
/// or `base64` (an explicit `data` string).
fn source_bytes(config: &Value, input: &Value, label: &str) -> Result<Vec<u8>, String> {
    match cfg_str(config, "source").unwrap_or("file") {
        "base64" => {
            let data = cfg_str(config, "data").ok_or_else(|| {
                format!(
                    "Compression: {label} needs Base64 Data (e.g. {{{{ $node[\"Synapse\"].body.body }}}})"
                )
            })?;
            let compact: String = data.chars().filter(|c| !c.is_whitespace()).collect();
            STANDARD
                .decode(compact.as_bytes())
                .map_err(|e| format!("Compression: invalid base64 data: {e}"))
        }
        _ => {
            let path = cfg_str(config, "filePath")
                .map(str::to_string)
                .or_else(|| find_file_descriptor(input).map(|(p, _)| p))
                .ok_or_else(|| {
                    format!(
                        "Compression: no file to {label} — set File Path or feed a node that \
                         outputs a binary descriptor (Myelin retrieve, Telegram download, \
                         Convert to File, Synapse file response)"
                    )
                })?;
            std::fs::read(&path).map_err(|e| format!("Compression: cannot read '{path}': {e}"))
        }
    }
}

/// One item → (bytes, entry name). A binary descriptor reads the staged
/// file; a string writes as text; anything else serializes as compact JSON.
fn entry_bytes(item: &Value, index: usize) -> Result<(Vec<u8>, String), String> {
    if let Some((path, name)) = find_file_descriptor(item) {
        let bytes =
            std::fs::read(&path).map_err(|e| format!("Compression: cannot read '{path}': {e}"))?;
        return Ok((bytes, name));
    }
    match item {
        Value::String(s) => Ok((s.clone().into_bytes(), format!("item_{}.txt", index + 1))),
        other => {
            let bytes = serde_json::to_vec(other)
                .map_err(|e| format!("Compression: JSON serialize error: {e}"))?;
            Ok((bytes, format!("item_{}.json", index + 1)))
        }
    }
}

/// De-duplicate a zip entry name against names already used, inserting
/// `_2`, `_3`, … before the extension (mirrors Extract from File's header
/// de-duplication).
fn dedupe_name(used: &mut std::collections::HashSet<String>, name: String) -> String {
    if used.insert(name.clone()) {
        return name;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.clone(), String::new()),
    };
    let mut n = 2;
    loop {
        let candidate = format!("{stem}_{n}{ext}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

fn build_zip(items: &[Value], level: Option<i64>) -> Result<Vec<u8>, String> {
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let mut options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    if let Some(l) = level {
        options = options.compression_level(Some(l));
    }
    let mut used = std::collections::HashSet::new();
    for (i, item) in items.iter().enumerate() {
        let (bytes, name) = entry_bytes(item, i)?;
        let name = dedupe_name(&mut used, name);
        writer
            .start_file(&name, options)
            .map_err(|e| format!("Compression: zip write error on '{name}': {e}"))?;
        writer
            .write_all(&bytes)
            .map_err(|e| format!("Compression: zip write error on '{name}': {e}"))?;
    }
    let cursor = writer
        .finish()
        .map_err(|e| format!("Compression: zip finalize error: {e}"))?;
    Ok(cursor.into_inner())
}

fn do_unzip(bytes: Vec<u8>) -> Result<Value, String> {
    let mut archive = ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("Compression: cannot open zip archive: {e}"))?;

    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Compression: cannot read zip entry {i}: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|e| format!("Compression: cannot decompress '{name}': {e}"))?;
        let size = data.len();
        let mime = mime_guess::from_path(&name)
            .first_or_octet_stream()
            .to_string();
        let staged = crate::files::stage_bytes(&data, &name)
            .map_err(|e| format!("Compression: failed to stage '{name}': {e}"))?;
        let local_path = staged.to_string_lossy().into_owned();
        out.push(json!({
            "filename": name,
            "mime_type": mime,
            "size": size,
            "binary": binary_descriptor(&local_path, &name, &mime, size),
        }));
    }
    Ok(Value::Array(out))
}

/// The value to gzip: the `gzipData` expression when it resolved to
/// something, else the primary input (the Convert to File convention).
/// Returns the compressed bytes plus the original file name (embedded in the
/// gzip header and used to derive the default output name).
fn build_gzip(config: &Value, input: &Value) -> Result<(Vec<u8>, String), String> {
    let data = match config.get("gzipData") {
        None | Some(Value::Null) => input.clone(),
        Some(Value::String(s)) if s.trim().is_empty() => input.clone(),
        Some(v) => v.clone(),
    };

    let (raw, source_name) = match &data {
        Value::Null => {
            return Err("Compression: nothing to gzip — fill the Data field \
                 (e.g. {{ $node[\"Convert to File\"] }}) or feed a node output"
                .to_string())
        }
        Value::String(s) => (s.clone().into_bytes(), "data".to_string()),
        other => match find_file_descriptor(other) {
            Some((path, name)) => (
                std::fs::read(&path)
                    .map_err(|e| format!("Compression: cannot read '{path}': {e}"))?,
                name,
            ),
            None => (
                serde_json::to_vec(other)
                    .map_err(|e| format!("Compression: JSON serialize error: {e}"))?,
                "data.json".to_string(),
            ),
        },
    };

    let level = cfg_usize(config, "compressionLevel")
        .map(|n| Compression::new((n as u32).min(9)))
        .unwrap_or_default();
    let mut encoder = GzBuilder::new()
        .filename(source_name.clone())
        .write(Vec::new(), level);
    encoder
        .write_all(&raw)
        .map_err(|e| format!("Compression: gzip error: {e}"))?;
    let compressed = encoder
        .finish()
        .map_err(|e| format!("Compression: gzip error: {e}"))?;
    Ok((compressed, source_name))
}

fn do_gunzip(config: &Value, bytes: Vec<u8>, input: &Value) -> Result<Value, String> {
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let embedded_name = decoder
        .header()
        .and_then(|h| h.filename())
        .map(|b| String::from_utf8_lossy(b).into_owned());
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| format!("Compression: gunzip error (not a valid gzip stream?): {e}"))?;

    let default_name = embedded_name
        .or_else(|| {
            find_file_descriptor(input)
                .map(|(_, n)| n.strip_suffix(".gz").unwrap_or(&n).to_string())
        })
        .unwrap_or_else(|| "data".to_string());
    let file_name =
        crate::files::sanitize_filename(cfg_str(config, "fileName").unwrap_or(&default_name));
    let mime = mime_guess::from_path(&file_name)
        .first_or_octet_stream()
        .to_string();
    let size = out.len();
    let staged = crate::files::stage_bytes(&out, &file_name)
        .map_err(|e| format!("Compression: failed to stage '{file_name}': {e}"))?;
    let local_path = staged.to_string_lossy().into_owned();
    Ok(json!({
        "filename": file_name,
        "mime_type": mime,
        "size": size,
        "binary": binary_descriptor(&local_path, &file_name, &mime, size),
    }))
}

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("zip");
    match operation {
        "zip" => {
            let items = match input {
                Value::Array(a) => a.clone(),
                Value::Null => {
                    return Err("Compression: nothing to zip — feed a node output \
                         (a file descriptor, string, or JSON value)"
                        .to_string())
                }
                other => vec![other.clone()],
            };
            // 0 means "use the library default" (per the UI hint) — the zip
            // crate's Deflated compression_level rejects a literal 0, so only
            // pass an explicit level through for 1-9.
            let level = cfg_usize(config, "compressionLevel")
                .filter(|&n| n > 0)
                .map(|n| n.min(9) as i64);
            let bytes = build_zip(&items, level)?;
            let size = bytes.len();
            let file_name = crate::files::sanitize_filename(
                cfg_str(config, "fileName").unwrap_or("archive.zip"),
            );
            let staged = crate::files::stage_bytes(&bytes, &file_name)
                .map_err(|e| format!("Compression: failed to stage '{file_name}': {e}"))?;
            let local_path = staged.to_string_lossy().into_owned();
            Ok(json!({
                "filename": file_name,
                "mime_type": "application/zip",
                "size": size,
                "binary": binary_descriptor(&local_path, &file_name, "application/zip", size),
            }))
        }
        "unzip" => {
            let bytes = source_bytes(config, input, "unzip")?;
            do_unzip(bytes)
        }
        "gzip" => {
            let (bytes, source_name) = build_gzip(config, input)?;
            let size = bytes.len();
            let file_name = crate::files::sanitize_filename(
                cfg_str(config, "fileName")
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{source_name}.gz"))
                    .as_str(),
            );
            let staged = crate::files::stage_bytes(&bytes, &file_name)
                .map_err(|e| format!("Compression: failed to stage '{file_name}': {e}"))?;
            let local_path = staged.to_string_lossy().into_owned();
            Ok(json!({
                "filename": file_name,
                "mime_type": "application/gzip",
                "size": size,
                "binary": binary_descriptor(&local_path, &file_name, "application/gzip", size),
            }))
        }
        "gunzip" => {
            let bytes = source_bytes(config, input, "gunzip")?;
            do_gunzip(config, bytes, input)
        }
        other => Err(format!("Unknown Compression operation: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(path: &std::path::Path, name: &str) -> Value {
        json!({ "binary": { "local_path": path.to_string_lossy(), "file_name": name } })
    }

    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(unique_name(tag, "src"))
    }

    // A name unique enough that parallel test threads never stage to the
    // same path in the shared staging dir (files.rs overwrites same-named
    // files, so two tests both defaulting to e.g. "archive.zip" would race).
    fn unique_name(tag: &str, ext: &str) -> String {
        format!(
            "axon_compression_test_{tag}_{}_{}.{ext}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    // Zipping a single staged file round-trips through unzip with the same
    // name and bytes.
    #[test]
    fn zip_single_file_round_trips() {
        let path = temp_path("a.txt");
        std::fs::write(&path, b"hello zip").unwrap();
        let input = descriptor(&path, "a.txt");
        let cfg = json!({ "operation": "zip", "fileName": unique_name("single", "zip") });
        let out = execute(&cfg, &input).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(out["mime_type"], json!("application/zip"));
        let zip_path = out["binary"]["local_path"].as_str().unwrap().to_string();
        let zip_bytes = std::fs::read(&zip_path).unwrap();
        std::fs::remove_file(&zip_path).ok();

        let entries = do_unzip(zip_bytes).unwrap();
        let arr = entries.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["filename"], json!("a.txt"));
        let entry_path = arr[0]["binary"]["local_path"].as_str().unwrap().to_string();
        assert_eq!(std::fs::read_to_string(&entry_path).unwrap(), "hello zip");
        std::fs::remove_file(&entry_path).ok();
    }

    // An array of items zips each as its own entry: descriptors read their
    // staged file, plain values serialize as text/JSON.
    #[test]
    fn zip_multiple_items_mixed_kinds() {
        let path = temp_path("report.csv");
        std::fs::write(&path, b"a,b\n1,2\n").unwrap();
        let input = json!([descriptor(&path, "report.csv"), "plain text", { "n": 1 }]);
        let cfg = json!({ "operation": "zip", "fileName": unique_name("multi", "zip") });
        let out = execute(&cfg, &input).unwrap();
        std::fs::remove_file(&path).ok();

        let zip_path = out["binary"]["local_path"].as_str().unwrap().to_string();
        let zip_bytes = std::fs::read(&zip_path).unwrap();
        std::fs::remove_file(&zip_path).ok();

        let entries = do_unzip(zip_bytes).unwrap();
        let arr = entries.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        let names: Vec<&str> = arr
            .iter()
            .map(|e| e["filename"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["report.csv", "item_2.txt", "item_3.json"]);
        for entry in arr {
            let p = entry["binary"]["local_path"].as_str().unwrap();
            std::fs::remove_file(p).ok();
        }
    }

    // Duplicate entry names (two items resolving to the same file name) get
    // a numbered suffix so the archive never silently drops one.
    #[test]
    fn zip_dedupes_duplicate_names() {
        // Two different files that both resolve to entry name "same.txt"
        // must not collide silently in the archive.
        let a = temp_path("same.txt");
        let b = temp_path("same2.txt");
        std::fs::write(&a, b"A").unwrap();
        std::fs::write(&b, b"B").unwrap();
        let input = json!([descriptor(&a, "same.txt"), descriptor(&b, "same.txt")]);
        let cfg = json!({ "operation": "zip", "fileName": unique_name("dedupe", "zip") });
        let out = execute(&cfg, &input).unwrap();
        std::fs::remove_file(&a).ok();
        std::fs::remove_file(&b).ok();

        let zip_path = out["binary"]["local_path"].as_str().unwrap().to_string();
        let zip_bytes = std::fs::read(&zip_path).unwrap();
        std::fs::remove_file(&zip_path).ok();
        let entries = do_unzip(zip_bytes).unwrap();
        let arr = entries.as_array().unwrap();
        let names: Vec<&str> = arr
            .iter()
            .map(|e| e["filename"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["same.txt", "same_2.txt"]);
        for entry in arr {
            let p = entry["binary"]["local_path"].as_str().unwrap();
            std::fs::remove_file(p).ok();
        }
    }

    // A missing/empty zip has zero entries — not an error (a legit empty
    // input, mirrors Extract from File's "no rows is not an error").
    #[test]
    fn unzip_empty_archive_is_empty_array() {
        let bytes = build_zip(&[], None).unwrap();
        let out = do_unzip(bytes).unwrap();
        assert_eq!(out, json!([]));
    }

    // Garbage bytes are a clean error, not a panic.
    #[test]
    fn unzip_garbage_bytes_errors() {
        let err = do_unzip(b"not a zip".to_vec()).unwrap_err();
        assert!(err.contains("cannot open zip archive"), "got: {err}");
    }

    // gzip a plain string, gunzip it back to the same bytes; the header
    // carries the "data" default name through, .gz appended.
    #[test]
    fn gzip_string_round_trips() {
        let out = execute(&json!({ "operation": "gzip" }), &json!("hello gzip")).unwrap();
        assert_eq!(out["filename"], json!("data.gz"));
        assert_eq!(out["mime_type"], json!("application/gzip"));
        let path = out["binary"]["local_path"].as_str().unwrap().to_string();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let restored = do_gunzip(&json!({}), bytes, &Value::Null).unwrap();
        assert_eq!(restored["filename"], json!("data"));
        let restored_path = restored["binary"]["local_path"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            std::fs::read_to_string(&restored_path).unwrap(),
            "hello gzip"
        );
        std::fs::remove_file(&restored_path).ok();
    }

    // gzip a staged file: the header embeds the ORIGINAL name (not "data"),
    // so gunzip recovers it without any config.
    #[test]
    fn gzip_file_embeds_original_name() {
        let path = temp_path("notes.txt");
        std::fs::write(&path, b"file body").unwrap();
        let input = descriptor(&path, "notes.txt");
        let out = execute(&json!({ "operation": "gzip" }), &input).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(out["filename"], json!("notes.txt.gz"));

        let gz_path = out["binary"]["local_path"].as_str().unwrap().to_string();
        let bytes = std::fs::read(&gz_path).unwrap();
        std::fs::remove_file(&gz_path).ok();
        let restored = do_gunzip(&json!({}), bytes, &Value::Null).unwrap();
        assert_eq!(restored["filename"], json!("notes.txt"));
        let restored_path = restored["binary"]["local_path"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            std::fs::read_to_string(&restored_path).unwrap(),
            "file body"
        );
        std::fs::remove_file(&restored_path).ok();
    }

    // An explicit fileName config overrides the embedded/derived default.
    #[test]
    fn gunzip_filename_override() {
        let cfg = json!({ "operation": "gzip", "fileName": unique_name("override", "gz") });
        let out = execute(&cfg, &json!("x")).unwrap();
        let gz_path = out["binary"]["local_path"].as_str().unwrap().to_string();
        let bytes = std::fs::read(&gz_path).unwrap();
        std::fs::remove_file(&gz_path).ok();
        let restored =
            do_gunzip(&json!({ "fileName": "renamed.bin" }), bytes, &Value::Null).unwrap();
        assert_eq!(restored["filename"], json!("renamed.bin"));
        let restored_path = restored["binary"]["local_path"]
            .as_str()
            .unwrap()
            .to_string();
        std::fs::remove_file(&restored_path).ok();
    }

    // Invalid gzip bytes error cleanly instead of panicking.
    #[test]
    fn gunzip_garbage_bytes_errors() {
        let err = do_gunzip(&json!({}), b"not gzip data".to_vec(), &Value::Null).unwrap_err();
        assert!(err.contains("gunzip error"), "got: {err}");
    }

    // Base64 source works for unzip the same way it does for Extract from
    // File (line wraps tolerated).
    #[test]
    fn unzip_base64_source() {
        let bytes = build_zip(&[json!("hi")], None).unwrap();
        let b64 = STANDARD.encode(&bytes);
        let cfg = json!({ "operation": "unzip", "source": "base64", "data": b64 });
        let out = execute(&cfg, &Value::Null).unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let p = arr[0]["binary"]["local_path"].as_str().unwrap();
        std::fs::remove_file(p).ok();
    }

    // No path and no descriptor is a teaching error for unzip/gunzip.
    #[test]
    fn unzip_no_source_errors() {
        let err = execute(&json!({ "operation": "unzip" }), &Value::Null).unwrap_err();
        assert!(err.contains("binary descriptor"), "got: {err}");
    }
}
