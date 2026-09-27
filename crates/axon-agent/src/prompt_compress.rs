//! Prompt compression + dedup, applied once per outgoing LLM request in
//! `router::model_router::call_llm_with_options`.
//!
//! Two stages, both gated by settings:
//!   - Stage 1 (`compression.enabled`, default true): whitespace normalization.
//!     Collapses runs of blank lines, strips trailing spaces, trims edges. The
//!     text itself is never changed, so this is safe by construction and costs
//!     a linear pass.
//!   - Stage 2 (`compression.dedup`, default false): exact-repeat elision.
//!     Whole segments that are byte-identical (after normalization) to an
//!     earlier one and at least `compression.dedup_min_chars` chars long have
//!     their later occurrences replaced with a short marker. The model still
//!     knows the content appeared earlier without paying its tokens again.
//!     Opt-in because replacing content with a pointer changes what the model
//!     literally sees.

use crate::config::RuntimeSettings;
use crate::providers::types::{ContentBlock, Message, MessageContent};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

/// Total chars removed since process start, for logs / a future dashboard read.
static SAVED_CHARS: AtomicU64 = AtomicU64::new(0);

pub fn total_saved_chars() -> u64 {
    SAVED_CHARS.load(Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub struct CompressionConfig {
    pub enabled: bool,
    pub dedup: bool,
    pub dedup_min_chars: usize,
}

impl CompressionConfig {
    pub fn from_settings(s: &RuntimeSettings) -> Self {
        CompressionConfig {
            enabled: s.get_bool("compression.enabled", true),
            dedup: s.get_bool("compression.dedup", false),
            dedup_min_chars: s.get_int("compression.dedup_min_chars", 400).max(64) as usize,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CompressionStats {
    pub original_chars: usize,
    pub compressed_chars: usize,
    pub deduplicated_segments: usize,
}

impl CompressionStats {
    pub fn saved_chars(&self) -> usize {
        self.original_chars.saturating_sub(self.compressed_chars)
    }
    pub fn saved_ratio(&self) -> f64 {
        if self.original_chars == 0 {
            0.0
        } else {
            self.saved_chars() as f64 / self.original_chars as f64
        }
    }
}

/// Compress `system` + `messages` ahead of one outgoing chat request.
pub fn compress_request(
    system: &str,
    messages: &[Message],
    settings: &RuntimeSettings,
) -> (String, Vec<Message>, CompressionStats) {
    let cfg = CompressionConfig::from_settings(settings);
    if !cfg.enabled {
        return (
            system.to_string(),
            messages.to_vec(),
            CompressionStats::default(),
        );
    }

    let original_chars = system.chars().count()
        + messages
            .iter()
            .map(|m| text_chars_of_content(&m.content))
            .sum::<usize>();

    let system_n = normalize_whitespace(system);

    // hash -> sequence index. The system string registers as a dedup *source*
    // only (it is never elided itself); the first whole-segment duplicate
    // survives and every later exact copy is elided.
    let mut seen: HashMap<u64, usize> = HashMap::new();
    let mut next_seq = 0usize;
    if cfg.dedup && system_n.chars().count() >= cfg.dedup_min_chars {
        seen.insert(hash_of(&system_n), next_seq);
        next_seq += 1;
    }

    let mut out = Vec::with_capacity(messages.len());
    let mut deduplicated = 0usize;
    for m in messages {
        let content = match &m.content {
            MessageContent::Text(t) => MessageContent::Text(compress_segment(
                t,
                &cfg,
                &mut seen,
                &mut next_seq,
                &mut deduplicated,
            )),
            MessageContent::Blocks(blocks) => {
                let mut nb = Vec::with_capacity(blocks.len());
                for b in blocks {
                    match b {
                        ContentBlock::Text { text } => nb.push(ContentBlock::Text {
                            text: compress_segment(
                                text,
                                &cfg,
                                &mut seen,
                                &mut next_seq,
                                &mut deduplicated,
                            ),
                        }),
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                        } => nb.push(ContentBlock::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            content: compress_segment(
                                content,
                                &cfg,
                                &mut seen,
                                &mut next_seq,
                                &mut deduplicated,
                            ),
                        }),
                        other => nb.push(other.clone()),
                    }
                }
                MessageContent::Blocks(nb)
            }
        };
        out.push(Message {
            role: m.role.clone(),
            content,
        });
    }

    let compressed_chars = system_n.chars().count()
        + out
            .iter()
            .map(|m| text_chars_of_content(&m.content))
            .sum::<usize>();

    let stats = CompressionStats {
        original_chars,
        compressed_chars,
        deduplicated_segments: deduplicated,
    };
    let saved = stats.saved_chars() as u64;
    if saved > 0 {
        let _ = SAVED_CHARS.fetch_add(saved, Ordering::Relaxed);
    }
    (system_n, out, stats)
}

const DEDUP_MARKER: &str =
    "\n[elided: byte-identical content already appears earlier in this context]\n";

fn compress_segment(
    seg: &str,
    cfg: &CompressionConfig,
    seen: &mut HashMap<u64, usize>,
    next_seq: &mut usize,
    deduplicated: &mut usize,
) -> String {
    let norm = normalize_whitespace(seg);
    if cfg.dedup && norm.chars().count() >= cfg.dedup_min_chars {
        let key = hash_of(&norm);
        if seen.contains_key(&key) {
            *deduplicated += 1;
            return DEDUP_MARKER.to_string();
        }
        seen.insert(key, *next_seq);
        *next_seq += 1;
    }
    norm
}

fn normalize_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut blank_run = false;
    for line in input.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            if !blank_run {
                out.push('\n');
                blank_run = true;
            }
        } else {
            out.push_str(trimmed);
            out.push('\n');
            blank_run = false;
        }
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

fn hash_of(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    h.write(s.as_bytes());
    h.finish()
}

fn text_chars_of_content(c: &MessageContent) -> usize {
    match c {
        MessageContent::Text(t) => t.chars().count(),
        MessageContent::Blocks(bs) => bs
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => text.chars().count(),
                ContentBlock::ToolResult { content, .. } => content.chars().count(),
                _ => 0,
            })
            .sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dedup: bool, min: usize) -> CompressionConfig {
        CompressionConfig {
            enabled: true,
            dedup,
            dedup_min_chars: min,
        }
    }

    #[test]
    fn no_change_when_disabled() {
        let s = normalize_whitespace("a\n\n\nb");
        assert_eq!(s, "a\n\nb");
    }

    #[test]
    fn collapses_blank_line_runs_and_trims() {
        let s = normalize_whitespace("  hello   \n\n\n\n  world   \n");
        assert_eq!(s, "  hello\n\n  world");
    }

    #[test]
    fn dedup_elides_second_identical_segment() {
        let mut seen = HashMap::new();
        let mut seq = 0usize;
        let mut n = 0usize;
        let big = "x".repeat(500);
        let first = compress_segment(&big, &cfg(true, 400), &mut seen, &mut seq, &mut n);
        let second = compress_segment(&big, &cfg(true, 400), &mut seen, &mut seq, &mut n);
        assert_eq!(first, big);
        assert!(second.len() < 100);
        assert_eq!(n, 1);
    }

    #[test]
    fn dedup_misses_below_threshold() {
        let mut seen = HashMap::new();
        let mut seq = 0usize;
        let mut n = 0usize;
        let small = "short";
        let first = compress_segment(small, &cfg(true, 400), &mut seen, &mut seq, &mut n);
        let second = compress_segment(small, &cfg(true, 400), &mut seen, &mut seq, &mut n);
        assert_eq!(first, small);
        assert_eq!(second, small);
        assert_eq!(n, 0);
    }
}
