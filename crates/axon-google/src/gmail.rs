use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use base64::{
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};
use serde_json::{json, Value};

use crate::auth::access_token;

const BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

/// Options for assembling a raw RFC 2822 message. `in_reply_to` must already be
/// a bracketed RFC Message-ID (`<id@host>`); when set it adds the
/// In-Reply-To/References headers Gmail needs to thread a reply.
/// `attachment_path` switches the body to multipart/mixed with the file attached.
struct MimeOptions<'a> {
    to: &'a str,
    subject: &'a str,
    body: &'a str,
    cc: Option<&'a str>,
    bcc: Option<&'a str>,
    in_reply_to: Option<&'a str>,
    attachment_path: Option<&'a str>,
}

/// Build a base64url-encoded raw message for the Gmail `messages.send`/`drafts`
/// endpoints, with optional Cc/Bcc, reply-threading headers, and a single file
/// attachment. Centralizes MIME assembly so send, reply, attachment-send and
/// drafts stay byte-for-byte consistent.
fn build_raw_message(opts: &MimeOptions) -> Result<String> {
    let mut headers = vec![
        format!("To: {}", opts.to),
        format!("Subject: {}", opts.subject),
        "MIME-Version: 1.0".to_owned(),
    ];
    if let Some(cc) = opts.cc.filter(|s| !s.is_empty()) {
        headers.push(format!("Cc: {cc}"));
    }
    if let Some(bcc) = opts.bcc.filter(|s| !s.is_empty()) {
        headers.push(format!("Bcc: {bcc}"));
    }
    if let Some(irt) = opts.in_reply_to.filter(|s| !s.is_empty()) {
        headers.push(format!("In-Reply-To: {irt}"));
        headers.push(format!("References: {irt}"));
    }

    match opts.attachment_path.filter(|p| !p.is_empty()) {
        Some(path) => {
            let data = std::fs::read(path)
                .map_err(|e| anyhow::anyhow!("Failed to read attachment '{path}': {e}"))?;
            let filename = std::path::Path::new(path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            let encoded_file = base64::engine::general_purpose::STANDARD.encode(data);
            let boundary = "axon_gmail_boundary";
            headers.push(format!(
                "Content-Type: multipart/mixed; boundary=\"{boundary}\""
            ));
            let header_block = headers.join("\r\n");
            let raw = format!(
                "{header_block}\r\n\r\n\
--{boundary}\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\n\
{body}\r\n\
--{boundary}\r\n\
Content-Type: application/octet-stream\r\n\
Content-Disposition: attachment; filename=\"{filename}\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
{encoded_file}\r\n\
--{boundary}--",
                body = opts.body,
            );
            Ok(URL_SAFE_NO_PAD.encode(raw))
        }
        None => {
            headers.push("Content-Type: text/plain; charset=utf-8".to_owned());
            let raw = format!("{}\r\n\r\n{}", headers.join("\r\n"), opts.body);
            Ok(URL_SAFE_NO_PAD.encode(raw))
        }
    }
}

pub async fn list(state: &AppState, max_results: u32, query: Option<&str>) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut params = vec![("maxResults", max_results.to_string())];
    if let Some(q) = query {
        params.push(("q", q.to_string()));
        // Gmail's messages.list hides SPAM/TRASH unless explicitly opted in, so a
        // query like `in:spam` / `in:trash` returns nothing without this flag.
        let ql = q.to_lowercase();
        if [
            "in:spam",
            "in:trash",
            "in:anywhere",
            "label:spam",
            "label:trash",
        ]
        .iter()
        .any(|needle| ql.contains(needle))
        {
            params.push(("includeSpamTrash", "true".to_string()));
        }
    }

    let list: Value = state
        .client
        .get(format!("{BASE}/messages"))
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?
        .ensure_ok()
        .await?
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
        .ensure_ok()
        .await?
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

    // Decompose the body the way a human reads it: the actual message, the
    // trailing quoted reply/forward thread, and the sender's signature / company
    // boilerplate that sits below the message. Quote is stripped first (it lives
    // at the very bottom), then the signature is split off what remains.
    let (without_quote, quoted) = split_trailing_quote(&body);
    let (body_main, signature) = split_signature(&without_quote);

    // Richer attachment metadata: byte size, whether the part is inline/embedded
    // (e.g. an image referenced from the HTML body) and a coarse kind so
    // downstream nodes can branch on images vs documents without re-parsing MIME.
    let mut attachments = Vec::new();
    collect_attachments(&msg["payload"], &mut attachments);
    let images: Vec<Value> = attachments
        .iter()
        .filter(|a| a["kind"] == "image")
        .cloned()
        .collect();
    let attachment_count = attachments.len();

    // Parse the sender/recipients into display name + bare address.
    let (from_name, from_email) =
        parse_address(headers.get("from").map(String::as_str).unwrap_or(""));
    let to_emails = split_addresses(headers.get("to").map(String::as_str).unwrap_or(""));

    let subj_l = headers
        .get("subject")
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let is_reply = subj_l.starts_with("re:") || headers.contains_key("in-reply-to");
    let is_forward = subj_l.starts_with("fwd:") || subj_l.starts_with("fw:");

    let links = extract_links(&body_main);
    // Structured contacts lifted from the signature — the "company info below the
    // main body" the user wants surfaced separately.
    let contacts = signature.as_ref().map(|sig| {
        json!({
            "emails": extract_emails(sig),
            "phones": extract_phones(sig),
            "links":  extract_links(sig),
        })
    });

    Ok(json!({
        "id":               msg["id"],
        "threadId":         msg["threadId"],
        "labelIds":         msg["labelIds"],
        "snippet":          decoded_value(&msg["snippet"]),
        "subject":          decoded_header(headers.get("subject")),
        "from":             decoded_header(headers.get("from")),
        "from_name":        from_name,
        "from_email":       from_email,
        "to":               decoded_header(headers.get("to")),
        "to_emails":        to_emails,
        "date":             headers.get("date"),
        "messageId":        headers.get("message-id"),
        "is_reply":         is_reply,
        "is_forward":       is_forward,
        "body":             body,
        "body_main":        body_main,
        "signature":        signature,
        "quoted_text":      quoted,
        "links":            links,
        "contacts":         contacts,
        "attachments":      attachments,
        "images":           images,
        "attachment_count": attachment_count,
        "has_attachments":  attachment_count > 0,
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
    let raw = build_raw_message(&MimeOptions {
        to,
        subject,
        body,
        cc,
        bcc,
        in_reply_to: None,
        attachment_path: None,
    })?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/send"))
        .bearer_auth(&tok)
        .json(&json!({ "raw": raw }))
        .send()
        .await?
        .ensure_ok()
        .await?
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
    let raw = build_raw_message(&MimeOptions {
        to,
        subject,
        body,
        cc,
        bcc,
        in_reply_to: None,
        attachment_path: Some(local_path),
    })?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/send"))
        .bearer_auth(&tok)
        .json(&json!({ "raw": raw }))
        .send()
        .await?
        .ensure_ok()
        .await?
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
    attachment_path: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;

    // Gmail attaches a reply to a thread only when In-Reply-To/References carry
    // the original message's RFC822 Message-ID (e.g. <CA+x@mail.gmail.com>) AND
    // the Subject matches. Workflows almost always pass Gmail's *API* id (from a
    // list/get/trigger node), which is NOT a valid Message-ID — so without
    // resolving it the reply lands as a brand-new conversation. When the id
    // isn't already an RFC message-id, fetch the original to recover its real
    // Message-ID, threadId and Subject.
    let provided = message_id.trim();
    let (rfc_id, resolved_thread, subject_src) = if provided.contains('@') {
        (
            provided.to_string(),
            thread_id.to_string(),
            subject.to_string(),
        )
    } else if provided.is_empty() {
        (String::new(), thread_id.to_string(), subject.to_string())
    } else {
        match get(state, provided).await {
            Ok(orig) => (
                orig["messageId"].as_str().unwrap_or_default().to_string(),
                orig["threadId"].as_str().unwrap_or(thread_id).to_string(),
                if subject.trim().is_empty() {
                    orig["subject"].as_str().unwrap_or_default().to_string()
                } else {
                    subject.to_string()
                },
            ),
            Err(_) => (String::new(), thread_id.to_string(), subject.to_string()),
        }
    };

    // Normalize to a bracketed RFC 2822 message-id.
    let in_reply_to = if rfc_id.is_empty() || rfc_id.starts_with('<') {
        rfc_id.clone()
    } else {
        format!("<{rfc_id}>")
    };

    let reply_subject = if subject_src.to_lowercase().starts_with("re:") {
        subject_src.clone()
    } else {
        format!("Re: {subject_src}")
    };

    let raw = build_raw_message(&MimeOptions {
        to,
        subject: &reply_subject,
        body,
        cc: None,
        bcc: None,
        in_reply_to: Some(&in_reply_to),
        attachment_path,
    })?;

    let send_thread = if resolved_thread.trim().is_empty() {
        thread_id
    } else {
        resolved_thread.as_str()
    };
    let resp: Value = state
        .client
        .post(format!("{BASE}/messages/send"))
        .bearer_auth(&tok)
        .json(&json!({ "raw": raw, "threadId": send_thread }))
        .send()
        .await?
        .ensure_ok()
        .await?
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
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn mark_read(state: &AppState, ids: Vec<String>) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .post(format!("{BASE}/messages/batchModify"))
        .bearer_auth(&tok)
        .json(&json!({ "ids": ids, "removeLabelIds": ["UNREAD"] }))
        .send()
        .await?
        .ensure_ok()
        .await?;
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
        .ensure_ok()
        .await?
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
        .ensure_ok()
        .await?
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
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

pub async fn mark_unread(state: &AppState, ids: Vec<String>) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .post(format!("{BASE}/messages/batchModify"))
        .bearer_auth(&tok)
        .json(&json!({ "ids": ids, "addLabelIds": ["UNREAD"] }))
        .send()
        .await?
        .ensure_ok()
        .await?;
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
        .ensure_ok()
        .await?
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
        .ensure_ok()
        .await?;
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

pub async fn create_draft(
    state: &AppState,
    to: &str,
    subject: &str,
    body: &str,
    cc: Option<&str>,
    bcc: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let raw = build_raw_message(&MimeOptions {
        to,
        subject,
        body,
        cc,
        bcc,
        in_reply_to: None,
        attachment_path: None,
    })?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/drafts"))
        .bearer_auth(&tok)
        .json(&json!({ "message": { "raw": raw } }))
        .send()
        .await?
        .ensure_ok()
        .await?
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
        .ensure_ok()
        .await?
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
        .ensure_ok()
        .await?
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
    let raw = build_raw_message(&MimeOptions {
        to,
        subject,
        body,
        cc,
        bcc,
        in_reply_to: None,
        attachment_path: None,
    })?;
    let resp: Value = state
        .client
        .put(format!("{BASE}/drafts/{draft_id}"))
        .bearer_auth(&tok)
        .json(&json!({ "message": { "raw": raw } }))
        .send()
        .await?
        .ensure_ok()
        .await?
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
        .ensure_ok()
        .await?
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
        .ensure_ok()
        .await?;
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
        "p",
        "blockquote",
        "table",
        "ul",
        "ol",
        "hr",
        "pre",
        "section",
        "article",
        "header",
        "footer",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
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
        let tag: String = chars[name_start..j]
            .iter()
            .collect::<String>()
            .to_lowercase();

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

// ── Email intelligence (body decomposition, contacts, attachments) ─────────────
//
// All dependency-free and pure so they stay unit-testable, matching the rest of
// this module (hand-rolled HTML-to-text, entity decoding, …).

/// Walk a Gmail payload MIME tree, collecting every real file part (one with a
/// non-empty filename and an attachmentId). Records byte size, whether the part
/// is inline/embedded (an image referenced from the HTML body), and a coarse
/// `kind` (image | document | other).
fn collect_attachments(part: &Value, out: &mut Vec<Value>) {
    let filename = part["filename"].as_str().unwrap_or("");
    if !filename.is_empty() {
        if let Some(att_id) = part["body"]["attachmentId"].as_str() {
            let mime = part["mimeType"].as_str().unwrap_or("");
            out.push(json!({
                "filename":     filename,
                "mimeType":     mime,
                "attachmentId": att_id,
                "size":         part["body"]["size"].as_u64().unwrap_or(0),
                "inline":       part_is_inline(part),
                "kind":         attachment_kind(mime, filename),
            }));
        }
    }
    if let Some(parts) = part["parts"].as_array() {
        for p in parts {
            collect_attachments(p, out);
        }
    }
}

/// True when a MIME part is an inline/embedded element (e.g. an image displayed
/// in the body) rather than a "real" downloadable attachment.
fn part_is_inline(part: &Value) -> bool {
    let Some(hs) = part["headers"].as_array() else {
        return false;
    };
    for h in hs {
        let name = h["name"].as_str().unwrap_or("").to_ascii_lowercase();
        if name == "content-id" || name == "x-attachment-id" {
            return true;
        }
        if name == "content-disposition"
            && h["value"]
                .as_str()
                .unwrap_or("")
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("inline")
        {
            return true;
        }
    }
    false
}

/// Coarse attachment category from MIME type, falling back to file extension.
fn attachment_kind(mime: &str, filename: &str) -> &'static str {
    let m = mime.to_ascii_lowercase();
    if m.starts_with("image/") {
        return "image";
    }
    const DOC_MIMES: &[&str] = &[
        "application/pdf",
        "application/msword",
        "application/rtf",
        "application/vnd", // covers office openxml + opendocument families
        "text/",
    ];
    if DOC_MIMES.iter().any(|d| m.starts_with(d)) {
        return "document";
    }
    match filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "heic" | "tif" | "tiff" => {
            "image"
        }
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "csv" | "rtf"
        | "odt" | "ods" => "document",
        _ => "other",
    }
}

/// Split an RFC 5322 address like `"Jane Doe" <jane@acme.com>` into
/// (display name, email). Handles the bare-address and name-only cases too.
fn parse_address(raw: &str) -> (Option<String>, Option<String>) {
    let raw = raw.trim();
    if raw.is_empty() {
        return (None, None);
    }
    if let (Some(lt), Some(gt)) = (raw.find('<'), raw.rfind('>')) {
        if lt < gt {
            let email = raw[lt + 1..gt].trim().to_string();
            let name = raw[..lt].trim().trim_matches('"').trim().to_string();
            return (
                (!name.is_empty()).then_some(name),
                (!email.is_empty()).then_some(email),
            );
        }
    }
    if raw.contains('@') && !raw.contains(' ') {
        return (None, Some(raw.to_string()));
    }
    (Some(raw.to_string()), None)
}

/// Split a header value holding several addresses into a list of bare emails.
/// Respects quotes and angle brackets so a display name like `"Doe, Jane"` is
/// not mistaken for two recipients.
fn split_addresses(raw: &str) -> Vec<String> {
    let mut emails = Vec::new();
    let mut in_angle = false;
    let mut in_quote = false;
    let mut cur = String::new();
    for c in raw.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            '<' => {
                in_angle = true;
                cur.push(c);
            }
            '>' => {
                in_angle = false;
                cur.push(c);
            }
            ',' if !in_angle && !in_quote => {
                if let Some(e) = parse_address(&cur).1 {
                    emails.push(e);
                }
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if let Some(e) = parse_address(&cur).1 {
        emails.push(e);
    }
    emails
}

/// Collect unique `http(s)://` URLs from text, stripping trailing punctuation.
fn extract_links(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("http") {
        let start = from + rel;
        let rest = &text[start..];
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let end = rest
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(
                            c,
                            '<' | '>' | '"' | '\'' | '|' | '[' | ']' | '{' | '}' | ')'
                        )
                })
                .unwrap_or(rest.len());
            let mut url = rest[..end].to_string();
            while url.ends_with(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?')) {
                url.pop();
            }
            if url.len() > 8 && !out.contains(&url) {
                out.push(url);
            }
            from = start + end.max(1);
        } else {
            from = start + 4;
        }
    }
    out
}

/// Collect unique email addresses appearing anywhere in `text`.
fn extract_emails(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';' | '<' | '>' | '(' | ')' | '[' | ']' | '"' | '\''
            )
    }) {
        let tok = tok.trim_matches(|c: char| matches!(c, '.' | ':' | '|'));
        if is_email(tok) && !out.iter().any(|e| e.eq_ignore_ascii_case(tok)) {
            out.push(tok.to_string());
        }
    }
    out
}

/// Loose RFC-ish email shape check (good enough to scrape signatures).
fn is_email(s: &str) -> bool {
    let mut parts = s.splitn(2, '@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-'))
}

/// Loosely pull phone-number-looking runs out of text. Conservative: a candidate
/// must consist only of phone glyphs and hold 7–15 digits.
fn extract_phones(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let trimmed = cur
            .trim()
            .trim_matches(|c: char| matches!(c, '-' | '.' | '(' | ')' | ' '))
            .to_string();
        let digits = trimmed.chars().filter(char::is_ascii_digit).count();
        if (7..=15).contains(&digits) && !out.contains(&trimmed) {
            out.push(trimmed);
        }
        cur.clear();
    };
    for c in text.chars() {
        if c.is_ascii_digit() || matches!(c, '+' | '-' | '(' | ')' | ' ' | '.') {
            cur.push(c);
        } else {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Split off a trailing quoted reply/forward thread, returning (message, quote).
/// Detects the attribution lines and header blocks Gmail / Outlook / Apple Mail
/// insert above quoted text.
fn split_trailing_quote(body: &str) -> (String, Option<String>) {
    let lines: Vec<&str> = body.lines().collect();
    let mut cut = None;
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        let lower = line.to_ascii_lowercase();
        let is_marker = (line.starts_with("On ") && lower.ends_with("wrote:"))
            || lower.starts_with("-----original message-----")
            || lower.starts_with("---------- forwarded message")
            || (line.starts_with('_') && line.chars().all(|c| c == '_') && line.len() >= 10)
            || line.starts_with('>')
            || (lower.starts_with("from:") && header_block_follows(&lines, i));
        if is_marker {
            cut = Some(i);
            break;
        }
    }
    match cut {
        Some(i) => {
            let main = lines[..i].join("\n").trim_end().to_string();
            let quote = lines[i..].join("\n").trim().to_string();
            (main, (!quote.is_empty()).then_some(quote))
        }
        None => (body.trim_end().to_string(), None),
    }
}

/// True when a `From:` line opens an Outlook-style quoted header block, i.e. a
/// `Sent:`/`Date:` and a `To:`/`Subject:` line follow within a few lines.
fn header_block_follows(lines: &[&str], from_idx: usize) -> bool {
    let window: Vec<String> = lines
        .iter()
        .skip(from_idx + 1)
        .take(4)
        .map(|l| l.trim().to_ascii_lowercase())
        .collect();
    let has_when = window
        .iter()
        .any(|l| l.starts_with("sent:") || l.starts_with("date:"));
    let has_who = window
        .iter()
        .any(|l| l.starts_with("to:") || l.starts_with("subject:"));
    has_when && has_who
}

const SIGNOFFS: &[&str] = &[
    "best regards",
    "kind regards",
    "warm regards",
    "warmest regards",
    "best wishes",
    "all the best",
    "many thanks",
    "thanks and regards",
    "yours sincerely",
    "yours faithfully",
    "yours truly",
    "best",
    "regards",
    "thanks",
    "thank you",
    "sincerely",
    "cheers",
    "respectfully",
    "cordially",
    "warmly",
];

/// Split the sender's signature / company boilerplate off the bottom of a
/// message. Honors the RFC 3676 `-- ` delimiter first; otherwise looks in the
/// tail for a short closing salutation ("Best regards", "Thanks", …) or a
/// mobile-sent footer. Conservative — returns `None` when nothing clearly marks
/// a signature so the full text is preserved as the message.
fn split_signature(body: &str) -> (String, Option<String>) {
    let lines: Vec<&str> = body.lines().collect();
    if lines.is_empty() {
        return (String::new(), None);
    }

    // 1. RFC 3676 delimiter: a line that is exactly "--" or "-- ".
    for (i, raw) in lines.iter().enumerate() {
        if *raw == "-- " || raw.trim() == "--" {
            let main = lines[..i].join("\n").trim_end().to_string();
            let sig = lines[i + 1..].join("\n").trim().to_string();
            return (main, (!sig.is_empty()).then_some(sig));
        }
    }

    // 2. Closing salutation / mobile footer in the tail. Never the very first
    //    line (a message that *opens* with "Hi" isn't a signature), and a
    //    salutation line must be short so a sentence like "Thanks for the help
    //    you gave me yesterday" doesn't get misread as a sign-off.
    let start = lines.len().saturating_sub(12).max(1);
    for i in start..lines.len() {
        let line = lines[i].trim();
        let lower = line.to_ascii_lowercase();
        let salutation = line.len() <= 30
            && SIGNOFFS.iter().any(|p| {
                lower == *p
                    || lower == format!("{p},")
                    || lower.starts_with(&format!("{p} "))
                    || lower.starts_with(&format!("{p},"))
            });
        let mobile = lower.starts_with("sent from my")
            || lower.starts_with("get outlook for")
            || lower.starts_with("sent via");
        if salutation || mobile {
            let main = lines[..i].join("\n").trim_end().to_string();
            let sig = lines[i..].join("\n").trim().to_string();
            return (main, (!sig.is_empty()).then_some(sig));
        }
    }

    (body.trim_end().to_string(), None)
}

// ── Attachment download ────────────────────────────────────────────────────────

/// Fetch and base64url-decode a single attachment's raw bytes.
async fn fetch_attachment_bytes(
    state: &AppState,
    message_id: &str,
    attachment_id: &str,
) -> Result<Vec<u8>> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!(
            "{BASE}/messages/{message_id}/attachments/{attachment_id}"
        ))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;

    let data = resp["data"].as_str().unwrap_or("");
    // Gmail returns attachments in URL-safe base64; padding presence varies.
    let data_padded = if data.len() % 4 != 0 {
        format!("{}{}", data, "=".repeat(4 - (data.len() % 4)))
    } else {
        data.to_owned()
    };
    let bytes = match URL_SAFE.decode(&data_padded) {
        Ok(b) => b,
        Err(_) => URL_SAFE_NO_PAD.decode(data).unwrap_or_default(),
    };
    Ok(bytes)
}

/// Strip directory components and unsafe characters from an email-supplied
/// filename so a malicious attachment name can't escape the staging directory.
fn sanitize_filename(name: &str) -> String {
    let base = std::path::Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        "attachment".to_string()
    } else {
        cleaned
    }
}

pub async fn download_attachment(
    state: &AppState,
    message_id: &str,
    attachment_id: &str,
    filename: &str,
) -> Result<Value> {
    let bytes = fetch_attachment_bytes(state, message_id, attachment_id).await?;

    let download_dir = axon_core::data_files_dir();
    std::fs::create_dir_all(&download_dir)?;
    let path = download_dir.join(filename);
    std::fs::write(&path, &bytes)?;

    Ok(json!({
        "file_path": path.to_string_lossy(),
        "size": bytes.len(),
        "message": "Attachment downloaded successfully. You can now access it at this local path to upload/send to the user."
    }))
}

/// Download *every* attachment on a message to local file paths. Best-effort:
/// a single failing attachment is skipped rather than aborting the rest.
pub async fn download_all_attachments(state: &AppState, message_id: &str) -> Result<Value> {
    let msg = get(state, message_id).await?;
    let download_dir = axon_core::data_files_dir();
    std::fs::create_dir_all(&download_dir)?;

    let mut files = Vec::new();
    if let Some(atts) = msg["attachments"].as_array() {
        for att in atts {
            let att_id = att["attachmentId"].as_str().unwrap_or("");
            if att_id.is_empty() {
                continue;
            }
            let filename = att["filename"].as_str().unwrap_or("attachment");
            let bytes = match fetch_attachment_bytes(state, message_id, att_id).await {
                Ok(b) => b,
                Err(_) => continue,
            };
            // Overwrite same-named attachments so only the newest copy is kept.
            let path = download_dir.join(sanitize_filename(filename));
            if std::fs::write(&path, &bytes).is_err() {
                continue;
            }
            files.push(json!({
                "filename":  filename,
                "file_path": path.to_string_lossy(),
                "mimeType":  att["mimeType"],
                "size":      bytes.len(),
                "inline":    att["inline"],
                "kind":      att["kind"],
            }));
        }
    }

    Ok(json!({
        "message_id": message_id,
        "count":      files.len(),
        "files":      files,
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
        let html = "<style>.x{color:red}</style><p>Keep</p><script>alert('x')</script><p>This</p>";
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
        assert_eq!(decoded_header(Some(&subject)), json!("Tom & Jerry 'news'"));
        assert_eq!(decoded_header(None), Value::Null);
    }

    // ── Email intelligence ──────────────────────────────────────────────────

    #[test]
    fn parses_address_forms() {
        assert_eq!(
            parse_address("\"Jane Doe\" <jane@acme.com>"),
            (Some("Jane Doe".into()), Some("jane@acme.com".into()))
        );
        assert_eq!(
            parse_address("Support Team <support@x.com>"),
            (Some("Support Team".into()), Some("support@x.com".into()))
        );
        assert_eq!(parse_address("bob@x.com"), (None, Some("bob@x.com".into())));
    }

    #[test]
    fn splits_multiple_recipients_respecting_quotes() {
        assert_eq!(
            split_addresses("\"Doe, Jane\" <jane@x.com>, bob@y.com"),
            vec!["jane@x.com".to_string(), "bob@y.com".to_string()]
        );
    }

    #[test]
    fn extracts_links_and_strips_trailing_punctuation() {
        assert_eq!(
            extract_links("See https://example.com/path, and http://foo.org. Done."),
            vec![
                "https://example.com/path".to_string(),
                "http://foo.org".to_string()
            ]
        );
    }

    #[test]
    fn extracts_emails_from_signature() {
        assert_eq!(
            extract_emails("John Smith\nAcme Corp | john.smith@acme.com\nsales@acme.com"),
            vec![
                "john.smith@acme.com".to_string(),
                "sales@acme.com".to_string()
            ]
        );
    }

    #[test]
    fn extracts_phone_numbers() {
        let phones = extract_phones("Call: +1 (555) 123-4567 or 020 7946 0958");
        assert!(phones
            .iter()
            .any(|p| p.contains("555") && p.contains("4567")));
        assert!(phones.iter().any(|p| p.contains("7946")));
    }

    #[test]
    fn strips_gmail_quoted_reply() {
        let body = "Thanks, that works.\n\nOn Mon, Jun 23, 2025 at 9:00 AM, Jane Doe <jane@x.com> wrote:\n> previous\n> more";
        let (main, quote) = split_trailing_quote(body);
        assert_eq!(main, "Thanks, that works.");
        assert!(quote.unwrap().starts_with("On Mon"));
    }

    #[test]
    fn strips_outlook_quoted_reply() {
        let body = "Here is my reply.\n\n________________________________\nFrom: Jane <jane@x.com>\nSent: Monday\nTo: Me\nSubject: Hi\n\nOriginal text";
        let (main, quote) = split_trailing_quote(body);
        assert_eq!(main, "Here is my reply.");
        assert!(quote.is_some());
    }

    #[test]
    fn splits_signature_on_rfc_delimiter() {
        let body = "The actual message.\nSecond line.\n-- \nJohn Smith\nAcme Corp\n+1 555 123 4567";
        let (main, sig) = split_signature(body);
        assert_eq!(main, "The actual message.\nSecond line.");
        assert_eq!(sig.unwrap(), "John Smith\nAcme Corp\n+1 555 123 4567");
    }

    #[test]
    fn splits_signature_on_signoff() {
        let body = "Can you review the doc?\n\nBest regards,\nJane Doe\nVP, Acme";
        let (main, sig) = split_signature(body);
        assert_eq!(main, "Can you review the doc?");
        assert!(sig.unwrap().starts_with("Best regards,"));
    }

    #[test]
    fn signature_ignores_long_thanks_sentence() {
        let body = "Thanks for sending the report over yesterday, it was very helpful.";
        let (main, sig) = split_signature(body);
        assert_eq!(main, body);
        assert!(sig.is_none());
    }

    #[test]
    fn classifies_attachment_kind() {
        assert_eq!(attachment_kind("image/png", "logo.png"), "image");
        assert_eq!(attachment_kind("application/pdf", "report.pdf"), "document");
        assert_eq!(
            attachment_kind(
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                "q.xlsx"
            ),
            "document"
        );
        assert_eq!(attachment_kind("", "sheet.xlsx"), "document");
        assert_eq!(attachment_kind("application/zip", "data.zip"), "other");
    }

    #[test]
    fn sanitizes_traversal_filenames() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("in voice#1.pdf"), "in voice_1.pdf");
        assert_eq!(sanitize_filename(""), "attachment");
    }
}
