use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};
use serde_json::{json, Value};

use crate::auth::access_token;

const BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

pub async fn list(state: &AppState, max_results: u32, query: Option<&str>) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut params = vec![("maxResults", max_results.to_string())];
    if let Some(q) = query {
        params.push(("q", q.to_string()));
    }

    let list: Value = state
        .client
        .get(format!("{BASE}/messages"))
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;

    let msgs = list["messages"].as_array();
    if msgs.map_or(true, |m| m.is_empty()) {
        return Ok(json!({ "messages": [] }));
    }

    // Fetch metadata for each message in parallel (up to 20)
    let ids: Vec<String> = msgs
        .unwrap()
        .iter()
        .take(20)
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect();

    let mut handles = Vec::new();
    for id in ids {
        let c = state.client.clone();
        let t = tok.clone();
        handles.push(tokio::spawn(async move {
            c.get(format!("{BASE}/messages/{id}"))
                .bearer_auth(&t)
                .query(&[
                    ("format", "metadata"),
                    ("metadataHeaders", "Subject"),
                    ("metadataHeaders", "From"),
                    ("metadataHeaders", "To"),
                    ("metadataHeaders", "Date"),
                    ("metadataHeaders", "Message-ID"),
                ])
                .send()
                .await?
                .ensure_ok()
                .await?
                .json::<Value>()
                .await
                .map_err(anyhow::Error::from)
        }));
    }

    let mut result = Vec::new();
    for h in handles {
        if let Ok(Ok(msg)) = h.await {
            let headers: std::collections::HashMap<String, String> = msg["payload"]["headers"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|h| {
                            let name = h["name"].as_str()?;
                            let value = h["value"].as_str()?;
                            Some((name.to_lowercase(), value.to_owned()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            result.push(json!({
                "id":        msg["id"],
                "threadId":  msg["threadId"],
                "labelIds":  msg["labelIds"],
                "snippet":   decoded_value(&msg["snippet"]),
                "subject":   decoded_header(headers.get("subject")),
                "from":      decoded_header(headers.get("from")),
                "to":        decoded_header(headers.get("to")),
                "date":      headers.get("date"),
            }));
        }
    }
    Ok(json!({ "messages": result, "nextPageToken": list["nextPageToken"] }))
}

pub async fn get(state: &AppState, id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let msg: Value = state
        .client
        .get(format!("{BASE}/messages/{id}"))
        .bearer_auth(&tok)
        .query(&[("format", "full")])
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;

    let headers: std::collections::HashMap<String, String> = msg["payload"]["headers"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|h| {
            Some((
                h["name"].as_str()?.to_lowercase(),
                h["value"].as_str()?.to_owned(),
            ))
        })
        .collect();

    let body = extract_body(&msg["payload"]);

    // Extract attachments
    let mut attachments = Vec::new();
    fn find_attachments(part: &Value, out: &mut Vec<Value>) {
        if let Some(filename) = part["filename"].as_str() {
            if !filename.is_empty() {
                if let Some(att_id) = part["body"]["attachmentId"].as_str() {
                    out.push(json!({
                        "filename": filename,
                        "mimeType": part["mimeType"].as_str().unwrap_or(""),
                        "attachmentId": att_id
                    }));
                }
            }
        }
        if let Some(parts) = part["parts"].as_array() {
            for p in parts {
                find_attachments(p, out);
            }
        }
    }
    find_attachments(&msg["payload"], &mut attachments);

    Ok(json!({
        "id":        msg["id"],
        "threadId":  msg["threadId"],
        "labelIds":  msg["labelIds"],
        "snippet":   decoded_value(&msg["snippet"]),
        "subject":   decoded_header(headers.get("subject")),
        "from":      decoded_header(headers.get("from")),
        "to":        decoded_header(headers.get("to")),
        "date":      headers.get("date"),
        "messageId": headers.get("message-id"),
        "body":      body,
        "attachments": attachments,
    }))
}

pub async fn send(
    state: &AppState,
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut lines = vec![
        format!("To: {to}"),
        format!("Subject: {subject}"),
        "Content-Type: text/plain; charset=utf-8".to_owned(),
        "MIME-Version: 1.0".to_owned(),
    ];
    if let Some(cc) = cc {
        lines.push(format!("Cc: {cc}"));
    }
    if let Some(bcc) = bcc {
        lines.push(format!("Bcc: {bcc}"));
    }
    lines.push(String::new());
    lines.push(body.to_owned());

    let raw = URL_SAFE_NO_PAD.encode(lines.join("\r\n"));
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/send"))
        .bearer_auth(&tok)
        .json(&json!({ "raw": raw }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn send_with_attachment(
    state: &AppState,
    to: &str,
    subject: &str,
    body: &str,
    local_path: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let data = std::fs::read(local_path)?;
    let filename = std::path::Path::new(local_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let encoded_file = base64::engine::general_purpose::STANDARD.encode(data);

    let boundary = "axon_gmail_boundary";
    let mut header_lines = format!(
        "To: {to}\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{boundary}\""
    );
    if let Some(cc) = cc {
        header_lines.push_str(&format!("\r\nCc: {cc}"));
    }
    if let Some(bcc) = bcc {
        header_lines.push_str(&format!("\r\nBcc: {bcc}"));
    }

    let mut raw_body = Vec::new();
    raw_body.extend_from_slice(
        format!(
            "{header_lines}\r\n\r\n\
--{boundary}\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\n\
{body}\r\n\
--{boundary}\r\n\
Content-Type: application/octet-stream\r\n\
Content-Disposition: attachment; filename=\"{filename}\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
{encoded_file}\r\n\
--{boundary}--"
        )
        .as_bytes(),
    );

    let raw = URL_SAFE_NO_PAD.encode(raw_body);
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/send"))
        .bearer_auth(&tok)
        .json(&json!({ "raw": raw }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn reply(
    state: &AppState,
    thread_id: &str,
    message_id: &str,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let lines = vec![
        format!("To: {to}"),
        format!(
            "Subject: {}",
            if subject.to_lowercase().starts_with("re:") {
                subject.to_owned()
            } else {
                format!("Re: {subject}")
            }
        ),
        format!("In-Reply-To: {message_id}"),
        format!("References: {message_id}"),
        "Content-Type: text/plain; charset=utf-8".to_owned(),
        "MIME-Version: 1.0".to_owned(),
        String::new(),
        body.to_owned(),
    ];
    let raw = URL_SAFE_NO_PAD.encode(lines.join("\r\n"));
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/send"))
        .bearer_auth(&tok)
        .json(&json!({ "raw": raw, "threadId": thread_id }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn search(state: &AppState, query: &str, max_results: u32) -> Result<Value> {
    list(state, max_results, Some(query)).await
}

pub async fn trash(state: &AppState, id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/{id}/trash"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn mark_read(state: &AppState, ids: Vec<&str>) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .post(format!("{BASE}/messages/batchModify"))
        .bearer_auth(&tok)
        .json(&json!({ "ids": ids, "removeLabelIds": ["UNREAD"] }))
        .send()
        .await?
        .ensure_ok().await?;
    Ok(json!({ "success": true, "count": ids.len() }))
}

pub async fn add_label(state: &AppState, id: &str, label_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/{id}/modify"))
        .bearer_auth(&tok)
        .json(&json!({ "addLabelIds": [label_id] }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn remove_label(state: &AppState, id: &str, label_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/{id}/modify"))
        .bearer_auth(&tok)
        .json(&json!({ "removeLabelIds": [label_id] }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn list_labels(state: &AppState) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/labels"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn mark_unread(state: &AppState, ids: Vec<&str>) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .post(format!("{BASE}/messages/batchModify"))
        .bearer_auth(&tok)
        .json(&json!({ "ids": ids, "addLabelIds": ["UNREAD"] }))
        .send()
        .await?
        .ensure_ok().await?;
    Ok(json!({ "success": true, "count": ids.len() }))
}

pub async fn untrash(state: &AppState, id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/{id}/untrash"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Permanently delete a message. Unlike `trash`, this is irreversible.
pub async fn delete(state: &AppState, id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/messages/{id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?;
    Ok(json!({ "success": true, "deletedId": id }))
}

/// Forward an email by fetching its body and re-sending to a new recipient.
pub async fn forward(
    state: &AppState,
    message_id: &str,
    to: &str,
    extra_note: Option<&str>,
) -> Result<Value> {
    // Fetch the original message so we can re-use its subject and body.
    let original = get(state, message_id).await?;
    let orig_subject = original["subject"].as_str().unwrap_or("(no subject)");
    let orig_body = original["body"].as_str().unwrap_or("");
    let orig_from = original["from"].as_str().unwrap_or("");
    let orig_date = original["date"].as_str().unwrap_or("");

    let fwd_subject = if orig_subject.to_lowercase().starts_with("fwd:") {
        orig_subject.to_owned()
    } else {
        format!("Fwd: {orig_subject}")
    };

    let note = extra_note.unwrap_or("");
    let fwd_body = format!(
        "{note}\r\n\r\n---------- Forwarded message ----------\r\nFrom: {orig_from}\r\nDate: {orig_date}\r\nSubject: {orig_subject}\r\n\r\n{orig_body}"
    );

    send(state, to, &fwd_subject, &fwd_body, None, None).await
}

// ── Drafts ────────────────────────────────────────────────────────────────────

fn build_raw_mime(
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> String {
    let mut lines = vec![
        format!("To: {to}"),
        format!("Subject: {subject}"),
        "Content-Type: text/plain; charset=utf-8".to_owned(),
        "MIME-Version: 1.0".to_owned(),
    ];
    if let Some(cc) = cc {
        lines.push(format!("Cc: {cc}"));
    }
    if let Some(bcc) = bcc {
        lines.push(format!("Bcc: {bcc}"));
    }
    lines.push(String::new());
    lines.push(body.to_owned());
    URL_SAFE_NO_PAD.encode(lines.join("\r\n"))
}

pub async fn create_draft(
    state: &AppState,
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let raw = build_raw_mime(to, subject, body, cc, bcc);
    let resp: Value = state
        .client
        .post(format!("{BASE}/drafts"))
        .bearer_auth(&tok)
        .json(&json!({ "message": { "raw": raw } }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn list_drafts(state: &AppState, max_results: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/drafts"))
        .bearer_auth(&tok)
        .query(&[("maxResults", max_results.to_string())])
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn get_draft(state: &AppState, draft_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/drafts/{draft_id}"))
        .bearer_auth(&tok)
        .query(&[("format", "full")])
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Replace a draft's content entirely (PUT semantics).
pub async fn update_draft(
    state: &AppState,
    draft_id: &str,
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let raw = build_raw_mime(to, subject, body, cc, bcc);
    let resp: Value = state
        .client
        .put(format!("{BASE}/drafts/{draft_id}"))
        .bearer_auth(&tok)
        .json(&json!({ "message": { "raw": raw } }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn send_draft(state: &AppState, draft_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/drafts/send"))
        .bearer_auth(&tok)
        .json(&json!({ "id": draft_id }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn delete_draft(state: &AppState, draft_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/drafts/{draft_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?;
    Ok(json!({ "success": true, "deletedDraftId": draft_id }))
}

// ── Internal body extractor ───────────────────────────────────────────────────

/// Decode a single MIME part's base64url body data, if present.
fn decode_part_data(part: &Value) -> Option<String> {
    let data = part["body"]["data"].as_str()?;
    // Gmail body data is URL-safe base64, but padding presence varies.
    // Try NO_PAD first; fall back to the padded variant so neither case silently fails.
    let bytes = URL_SAFE_NO_PAD
        .decode(data)
        .or_else(|_| URL_SAFE.decode(data))
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// Depth-first search the MIME tree for the first non-empty part of `mime`.
fn find_part(payload: &Value, mime: &str) -> Option<String> {
    if payload["mimeType"].as_str() == Some(mime) {
        if let Some(text) = decode_part_data(payload) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    if let Some(parts) = payload["parts"].as_array() {
        for part in parts {
            if let Some(text) = find_part(part, mime) {
                return Some(text);
            }
        }
    }
    None
}

/// Best human-readable body for a Gmail message payload. Prefers a `text/plain`
/// part; otherwise converts the `text/html` part to readable plain text so the
/// output matches what the user sees when opening the email in the Gmail inbox.
fn extract_body(payload: &Value) -> String {
    if let Some(text) = find_part(payload, "text/plain") {
        return text.trim().to_string();
    }
    if let Some(html) = find_part(payload, "text/html") {
        return html_to_text(&html);
    }
    // Single-part message: the body data hangs directly off the payload.
    if let Some(raw) = decode_part_data(payload) {
        if looks_like_html(&raw) {
            return html_to_text(&raw);
        }
        return raw.trim().to_string();
    }
    String::new()
}

/// Heuristic: does this decoded body look like HTML rather than plain text?
fn looks_like_html(s: &str) -> bool {
    let lower = s.to_lowercase();
    lower.contains("<!doctype")
        || lower.contains("<html")
        || lower.contains("<body")
        || lower.contains("<div")
        || lower.contains("<table")
        || lower.contains("<p>")
        || lower.contains("<br")
        || lower.contains("<span")
}

/// Convert an HTML email body into readable plain text, approximating what the
/// user sees when opening the message in Gmail. Dependency-free: drops
/// script/style contents, turns block-level tags into line breaks, removes the
/// remaining tags, decodes common HTML entities, and collapses whitespace using
/// the same "insignificant whitespace" rule browsers apply.
fn html_to_text(html: &str) -> String {
    // Block tags that read as a new paragraph (a blank line) vs. a single line break.
    const PARA: &[&str] = &[
        "p", "blockquote", "table", "ul", "ol", "hr", "pre", "section", "article", "header",
        "footer", "h1", "h2", "h3", "h4", "h5", "h6",
    ];
    const LINE: &[&str] = &["br", "div", "tr", "li"];

    let chars: Vec<char> = html.chars().collect();
    let mut out = String::with_capacity(chars.len());
    // Queued, not-yet-emitted whitespace. Breaks from adjacent block boundaries
    // coalesce (max 2 = one blank line) instead of stacking up; runs of source
    // whitespace between tags collapse to a single space, like a browser.
    let mut pending_break: usize = 0;
    let mut pending_space = false;

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '<' {
            if c.is_whitespace() {
                // Source whitespace is insignificant; remember it as one space.
                pending_space = true;
                i += 1;
                continue;
            }
            // A visible character: flush any queued break (preferred) or space.
            if out.is_empty() {
                pending_break = 0;
                pending_space = false;
            }
            if pending_break > 0 {
                for _ in 0..pending_break {
                    out.push('\n');
                }
            } else if pending_space {
                out.push(' ');
            }
            pending_break = 0;
            pending_space = false;
            out.push(c);
            i += 1;
            continue;
        }

        // Read the (optional leading '/') tag name.
        let mut j = i + 1;
        if j < chars.len() && chars[j] == '/' {
            j += 1;
        }
        let name_start = j;
        while j < chars.len() && chars[j].is_ascii_alphanumeric() {
            j += 1;
        }
        let tag: String = chars[name_start..j].iter().collect::<String>().to_lowercase();

        // Drop the entire contents of <script> and <style> elements: skip from
        // here past the matching closing tag (case-insensitive, char-indexed).
        if tag == "script" || tag == "style" {
            let close = format!("</{tag}>");
            let mut k = j;
            while k < chars.len() && !matches_ci(&chars, k, &close) {
                k += 1;
            }
            i = if k < chars.len() {
                k + close.chars().count()
            } else {
                chars.len()
            };
            continue;
        }

        // Skip to the end of the tag.
        while i < chars.len() && chars[i] != '>' {
            i += 1;
        }
        i += 1; // consume '>'

        // Queue the break this tag implies. <br> stacks (so <br><br> = blank
        // line); block tags set a floor so an adjacent open/close pair doesn't
        // double up.
        if tag == "br" {
            pending_break = (pending_break + 1).min(2);
        } else if PARA.contains(&tag.as_str()) {
            pending_break = pending_break.max(2);
        } else if LINE.contains(&tag.as_str()) {
            pending_break = pending_break.max(1);
        }
    }

    let decoded = decode_html_entities(&out);
    normalize_whitespace(&decoded)
}

/// Case-insensitive match of an ASCII `needle` (already lowercase) against `chars[idx..]`.
fn matches_ci(chars: &[char], idx: usize, needle: &str) -> bool {
    let nb: Vec<char> = needle.chars().collect();
    if idx + nb.len() > chars.len() {
        return false;
    }
    nb.iter()
        .enumerate()
        .all(|(k, nc)| chars[idx + k].to_ascii_lowercase() == *nc)
}

/// Decode HTML entities in an optional header value (e.g. `subject`, `from`),
/// preserving JSON `null` when the header is absent. Gmail occasionally returns
/// header values containing entities like `&#39;` / `&amp;`.
fn decoded_header(value: Option<&String>) -> Value {
    value.map_or(Value::Null, |s| json!(decode_html_entities(s)))
}

/// Decode HTML entities in a string-valued JSON field. Gmail's `snippet` is
/// always HTML-escaped (e.g. `what&#39;s` / `&amp;`), so decode it before
/// surfacing it to workflow expressions. Non-string values pass through.
fn decoded_value(value: &Value) -> Value {
    value
        .as_str()
        .map_or_else(|| value.clone(), |s| json!(decode_html_entities(s)))
}

/// Decode the HTML entities commonly found in email bodies (named + numeric).
fn decode_html_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        let mut entity = String::new();
        let mut terminated = false;
        for _ in 0..12 {
            match chars.peek() {
                Some(';') => {
                    chars.next();
                    terminated = true;
                    break;
                }
                Some(&ch) if ch.is_ascii_alphanumeric() || ch == '#' => {
                    entity.push(ch);
                    chars.next();
                }
                _ => break,
            }
        }
        match decode_entity(&entity) {
            Some(decoded) if terminated => out.push_str(&decoded),
            _ => {
                // Unrecognized — emit it back verbatim so nothing is lost.
                out.push('&');
                out.push_str(&entity);
                if terminated {
                    out.push(';');
                }
            }
        }
    }
    out
}

fn decode_entity(entity: &str) -> Option<String> {
    // Numeric: &#123; (decimal) or &#x1F600; (hex)
    if let Some(num) = entity.strip_prefix('#') {
        let code = if num.starts_with('x') || num.starts_with('X') {
            u32::from_str_radix(&num[1..], 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        return char::from_u32(code).map(|c| c.to_string());
    }
    let named = match entity {
        "nbsp" => " ",
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "mdash" => "\u{2014}",
        "ndash" => "\u{2013}",
        "hellip" => "\u{2026}",
        "copy" => "\u{00A9}",
        "reg" => "\u{00AE}",
        "trade" => "\u{2122}",
        "lsquo" => "\u{2018}",
        "rsquo" => "\u{2019}",
        "ldquo" => "\u{201C}",
        "rdquo" => "\u{201D}",
        "bull" => "\u{2022}",
        "middot" => "\u{00B7}",
        "euro" => "\u{20AC}",
        "pound" => "\u{00A3}",
        "cent" => "\u{00A2}",
        "deg" => "\u{00B0}",
        _ => return None,
    };
    Some(named.to_string())
}

/// Collapse runs of whitespace within lines and runs of blank lines, so the
/// result reads like the rendered email rather than raw, irregularly-spaced markup.
fn normalize_whitespace(s: &str) -> String {
    let unified = s.replace("\r\n", "\n").replace('\r', "\n");
    let mut out: Vec<String> = Vec::new();
    let mut prev_blank = false;
    for line in unified.split('\n') {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            // Keep at most one blank line between paragraphs.
            if !prev_blank && !out.is_empty() {
                out.push(String::new());
            }
            prev_blank = true;
        } else {
            out.push(collapsed);
            prev_blank = false;
        }
    }
    while out.last().map_or(false, |l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

pub async fn download_attachment(
    state: &AppState,
    message_id: &str,
    attachment_id: &str,
    filename: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!(
            "{BASE}/messages/{message_id}/attachments/{attachment_id}"
        ))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;

    let data = resp["data"].as_str().unwrap_or("");

    // Gmail returns attachments in URL-safe base64. Ensure it's padded.
    let data_padded = if data.len() % 4 != 0 {
        let pad_len = 4 - (data.len() % 4);
        format!("{}{}", data, "=".repeat(pad_len))
    } else {
        data.to_owned()
    };

    let bytes = match URL_SAFE.decode(&data_padded) {
        Ok(b) => b,
        Err(_) => URL_SAFE_NO_PAD.decode(data).unwrap_or_default(),
    };

    let download_dir = std::path::PathBuf::from("/data/files");
    std::fs::create_dir_all(&download_dir)?;
    let path = download_dir.join(filename);
    std::fs::write(&path, &bytes)?;

    Ok(json!({
        "file_path": path.to_string_lossy(),
        "message": "Attachment downloaded successfully. You can now access it at this local path to upload/send to the user."
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_and_paragraphs_get_blank_line() {
        // <p> blocks read as paragraphs (blank line between), like Gmail shows.
        let html = "<div><p>Hello&nbsp;<b>World</b></p><p>Line two</p></div>";
        assert_eq!(html_to_text(html), "Hello World\n\nLine two");
    }

    #[test]
    fn adjacent_divs_are_single_spaced() {
        // Gmail composes each line as its own <div>; these must NOT double-space.
        let html = "<div>line one</div><div>line two</div><div>line three</div>";
        assert_eq!(html_to_text(html), "line one\nline two\nline three");
    }

    #[test]
    fn collapses_source_whitespace_between_tags() {
        let html = "<div>line one</div>\n   <div>line two</div>";
        assert_eq!(html_to_text(html), "line one\nline two");
    }

    #[test]
    fn br_becomes_newline() {
        assert_eq!(html_to_text("a<br>b<br/>c"), "a\nb\nc");
        assert_eq!(html_to_text("a<br><br>b"), "a\n\nb");
    }

    #[test]
    fn drops_script_and_style() {
        let html =
            "<style>.x{color:red}</style><p>Keep</p><script>alert('x')</script><p>This</p>";
        assert_eq!(html_to_text(html), "Keep\n\nThis");
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(
            html_to_text("Tom &amp; Jerry &lt;3 &#39;quote&#39; &#x263A;"),
            "Tom & Jerry <3 'quote' \u{263A}"
        );
    }

    #[test]
    fn collapses_excess_whitespace_and_blank_lines() {
        let html = "<p>One</p><p></p><p></p><p>   Two    words   </p>";
        assert_eq!(html_to_text(html), "One\n\nTwo words");
    }

    #[test]
    fn unknown_entity_is_preserved() {
        assert_eq!(html_to_text("a &notareal; b"), "a &notareal; b");
    }

    #[test]
    fn snippet_entities_are_decoded() {
        let snippet = json!("Scope out what&#39;s changed. Upload files &amp; ask about");
        assert_eq!(
            decoded_value(&snippet),
            json!("Scope out what's changed. Upload files & ask about")
        );
    }

    #[test]
    fn subject_header_entities_are_decoded() {
        let subject = "Tom &amp; Jerry &#39;news&#39;".to_string();
        assert_eq!(
            decoded_header(Some(&subject)),
            json!("Tom & Jerry 'news'")
        );
        assert_eq!(decoded_header(None), Value::Null);
    }
}
