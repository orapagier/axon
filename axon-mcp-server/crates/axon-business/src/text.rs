use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

pub fn word_count(text: &str) -> Result<Value> {
    let words = text.split_whitespace().count();
    let chars = text.chars().count();
    let chars_nsp = text.chars().filter(|c| !c.is_whitespace()).count();
    let sentences = text
        .split(['.', '!', '?'])
        .filter(|s| !s.trim().is_empty())
        .count();
    let paragraphs = text.split("\n\n").filter(|s| !s.trim().is_empty()).count();
    let lines = text.lines().count();

    Ok(json!({
        "words":             words,
        "characters":        chars,
        "characters_no_spaces": chars_nsp,
        "sentences":         sentences,
        "paragraphs":        paragraphs,
        "lines":             lines,
        "reading_time_min":  ((words as f64) / 200.0).ceil() as u32, // avg reading speed
    }))
}

pub fn summarize_lines(text: &str, lines: usize) -> Result<Value> {
    let result: Vec<&str> = text.lines().take(lines).collect();
    Ok(json!({
        "summary":     result.join("\n"),
        "total_lines": text.lines().count(),
        "shown_lines": result.len(),
    }))
}

pub fn extract_emails(text: &str) -> Result<Value> {
    // Simple regex-free email extractor
    let emails: Vec<&str> = text
        .split_whitespace()
        .chain(text.split([',', ';', '<', '>', '"', '\'', '(', ')', '[', ']']))
        .map(|w| {
            w.trim_matches(|c: char| {
                !c.is_alphanumeric() && c != '@' && c != '.' && c != '_' && c != '-' && c != '+'
            })
        })
        .filter(|w| {
            let parts: Vec<&str> = w.splitn(2, '@').collect();
            parts.len() == 2 && !parts[0].is_empty() && parts[1].contains('.')
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    Ok(json!({ "emails": emails, "count": emails.len() }))
}

pub fn extract_urls(text: &str) -> Result<Value> {
    let prefixes = ["http://", "https://", "ftp://"];
    let urls: Vec<&str> = text
        .split_whitespace()
        .flat_map(|w| {
            // Also split on common delimiters that might appear around URLs
            let trimmed = w.trim_matches(|c: char| {
                matches!(c, ',' | ';' | '"' | '\'' | '(' | ')' | '[' | ']')
            });
            if prefixes.iter().any(|p| trimmed.starts_with(p)) {
                Some(trimmed)
            } else {
                None
            }
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    Ok(json!({ "urls": urls, "count": urls.len() }))
}

pub fn slugify(text: &str) -> Result<Value> {
    let slug: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        // Collapse multiple hyphens
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    Ok(json!({ "input": text, "slug": slug }))
}

pub fn render_template(template: &str, vars: &Map<String, Value>) -> Result<Value> {
    let mut output = template.to_owned();
    let mut missing = Vec::new();

    // Extract all {{key}} placeholders
    let mut i = 0;
    let bytes = template.as_bytes();
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            if let Some(end) = template[i + 2..].find("}}") {
                let key = template[i + 2..i + 2 + end].trim();
                if !vars.contains_key(key) {
                    missing.push(key.to_owned());
                }
                i += 2 + end + 2;
                continue;
            }
        }
        i += 1;
    }

    if !missing.is_empty() {
        return Err(anyhow!(
            "Template references undefined variables: {}. Provide them in 'vars'.",
            missing.join(", ")
        ));
    }

    for (key, value) in vars {
        let placeholder = format!("{{{{{}}}}}", key);
        let replacement = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        output = output.replace(&placeholder, &replacement);
    }

    Ok(json!({ "result": output, "vars_used": vars.len() }))
}
