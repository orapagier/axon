//! Shared text helpers.
//!
//! Every truncation in this codebase must count **characters, not bytes**.
//! Slicing a `&str` at a byte offset panics when the cut lands inside a
//! multi-byte character, and essentially all the text that flows through Axon
//! (tool results, email bodies, chat messages, search snippets) is arbitrary
//! UTF-8. That panic has already shipped once — in the memory compressor, where
//! it ran inside a spawned task and so was completely silent, dropping the
//! observation with no error anywhere.
//!
//! These helpers live here rather than being redefined per call site so the next
//! one is a one-line import instead of another chance to get it wrong.

/// Truncate to `max` characters, appending `…[truncated]` when anything was cut.
///
/// Used for the LLM-facing node prompts (Summarize, Classifier, Sentiment,
/// Information Extractor), which only need the model to know the text was cut.
pub fn truncate_chars(s: &str, max: usize) -> String {
    match dropped_char_count(s, max) {
        0 => s.to_string(),
        _ => format!("{}…[truncated]", take_chars(s, max)),
    }
}

/// Like [`truncate_chars`], but the marker reports *how many* characters were
/// dropped. Worth the extra tokens where the model is asked to judge whether it
/// saw enough — a note that 40 characters were cut reads very differently from
/// one saying 40,000 were.
pub fn truncate_chars_counted(s: &str, max: usize) -> String {
    match dropped_char_count(s, max) {
        0 => s.to_string(),
        n => format!("{}... [trimmed {n} chars]", take_chars(s, max)),
    }
}

/// Characters that would be lost by truncating to `max`; 0 when it all fits.
///
/// Counts lazily: `s.chars().count()` walks the whole string, which is wasted
/// work on the common path where the input is far under the limit.
fn dropped_char_count(s: &str, max: usize) -> usize {
    // Cheap reject: a string of `max` chars is at most `4 * max` bytes.
    if s.len() <= max {
        return 0;
    }
    s.chars().count().saturating_sub(max)
}

fn take_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_under_the_limit() {
        assert_eq!(truncate_chars("short", 100), "short");
        assert_eq!(truncate_chars_counted("short", 100), "short");
    }

    #[test]
    fn counts_characters_not_bytes() {
        // 5 chars, 10 bytes: a byte-based limit would cut this in half.
        let s = "áéíóú";
        assert_eq!(s.len(), 10);
        assert_eq!(truncate_chars(s, 5), s, "exactly at the limit is a noop");
        assert!(truncate_chars(s, 2).starts_with("áé"));
    }

    /// The regression that motivated this module: a cut landing mid-character.
    #[test]
    fn a_cut_inside_a_multibyte_char_does_not_panic() {
        let s = format!("{}é{}", "a".repeat(1499), "b".repeat(50));
        assert!(!s.is_char_boundary(1500), "byte 1500 is inside the 'é'");

        assert!(truncate_chars(&s, 1500).ends_with("…[truncated]"));
        assert!(truncate_chars_counted(&s, 1500).contains("[trimmed 50 chars]"));
    }

    #[test]
    fn astral_plane_chars_are_whole() {
        // Emoji are 4 bytes each; taking 4 chars must yield 4 whole emoji.
        let s = "🌍".repeat(10);
        assert_eq!(truncate_chars(&s, 10), s);
        assert!(truncate_chars(&s, 4).starts_with(&"🌍".repeat(4)));
        assert_eq!(
            truncate_chars_counted(&s, 4),
            format!("{}... [trimmed 6 chars]", "🌍".repeat(4))
        );
    }

    #[test]
    fn zero_max_keeps_nothing() {
        assert_eq!(truncate_chars("abc", 0), "…[truncated]");
        assert_eq!(truncate_chars_counted("abc", 0), "... [trimmed 3 chars]");
    }

    /// `dropped_char_count`'s byte-length fast path must never claim a string
    /// fits when it does not.
    #[test]
    fn byte_fast_path_agrees_with_the_char_count() {
        for s in ["", "a", "áéíóú", "🌍🌍🌍", "mixed ábc 🌍 text"] {
            for max in 0..12 {
                let expected = s.chars().count().saturating_sub(max);
                assert_eq!(dropped_char_count(s, max), expected, "s={s:?} max={max}");
            }
        }
    }
}
