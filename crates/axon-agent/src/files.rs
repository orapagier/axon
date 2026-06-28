use anyhow::Result;
use std::path::{Path, PathBuf};

/// Ensure the central staging directory exists and return its canonical path.
///
/// All file transfers (received from users or downloaded by tools/nodes) land
/// here. The directory is resolved by [`axon_core::data_files_dir`] so every
/// node and the agent share ONE location — a file a node saves is found by the
/// sender and the Files page. Files keep their original (sanitized) name and
/// saving the same name overwrites the previous file (newest only).
pub fn staging_dir() -> PathBuf {
    let dir = axon_core::data_files_dir();
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
    }
    // Return canonical path for security comparisons
    dir.canonicalize().unwrap_or(dir)
}

/// Save raw bytes to the staging directory under the file's original name.
/// If a file with the same name already exists it is overwritten, so only the
/// newest copy is kept.
/// Returns the absolute path of the staged file.
pub fn stage_bytes(data: &[u8], original_name: &str) -> Result<PathBuf> {
    let dir = staging_dir();
    let staged_name = sanitize_filename(original_name);
    let path = dir.join(&staged_name);
    std::fs::write(&path, data)?;
    Ok(path.canonicalize().unwrap_or(path))
}

/// Copy a local file into the staging directory.
/// Returns the absolute path of the staged file.
pub fn stage_file(source_path: &Path) -> Result<PathBuf> {
    let data = std::fs::read(source_path)?;
    let name = source_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    stage_bytes(&data, &name)
}

/// Validate that a given path is inside the staging directory.
/// Prevents path traversal attacks.
pub fn is_valid_staged_path(path: &str) -> bool {
    let requested = match Path::new(path).canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let staging = staging_dir();
    requested.starts_with(&staging)
}

/// Remove staged files older than `max_age`.
pub fn cleanup_old(max_age: std::time::Duration) {
    let dir = staging_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0u32;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                if let Ok(modified) = meta.modified() {
                    if let Ok(age) = now.duration_since(modified) {
                        if age > max_age {
                            let _ = std::fs::remove_file(entry.path());
                            removed += 1;
                        }
                    }
                }
            }
        }
    }
    if removed > 0 {
        tracing::info!("Cleaned up {} old staged files", removed);
    }
}

/// Sanitize a filename to remove path separators, control characters and
/// leading/trailing whitespace.
pub fn sanitize_filename(name: &str) -> String {
    let mut normalized = name.trim().to_string();
    // Some upstream headers return escaped EOL sequences as literal text (e.g. "\n").
    // Strip those suffixes so downstream extension checks stay stable.
    while let Some(stripped) = normalized
        .strip_suffix("\\r\\n")
        .or_else(|| normalized.strip_suffix("\\n"))
        .or_else(|| normalized.strip_suffix("\\r"))
    {
        normalized = stripped.trim_end().to_string();
    }

    let trimmed = normalized.trim_matches(|c: char| c.is_whitespace() || c.is_control());
    let mut sanitized: String = trimmed
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            _ => c,
        })
        .collect();

    if sanitized.is_empty() {
        sanitized = "file".to_string();
    }

    sanitized
}

/// Metadata about a file attached by the user.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AttachedFile {
    pub original_name: String,
    pub local_path: String,
    pub mime_type: String,
    pub size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(
            sanitize_filename("file/with\\bad:chars"),
            "file_with_bad_chars"
        );
        assert_eq!(sanitize_filename("normal.pdf"), "normal.pdf");
        assert_eq!(sanitize_filename(" song.mp3\n"), "song.mp3");
        assert_eq!(sanitize_filename(" song.mp3\\n"), "song.mp3");
        assert_eq!(sanitize_filename(" song.mp3\\r\\n"), "song.mp3");
        assert_eq!(sanitize_filename("line\r\nbreak.txt"), "line__break.txt");
        assert_eq!(sanitize_filename(" \n\t "), "file");
    }

    #[test]
    fn test_stage_and_validate() {
        let path = stage_bytes(b"hello world", "test.txt").unwrap();
        assert!(is_valid_staged_path(&path.display().to_string()));
        std::fs::remove_file(&path).ok();
    }
}
