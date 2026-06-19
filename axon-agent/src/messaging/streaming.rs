use super::gateway::{MessageGateway, OutgoingFile};
use crate::agent::context::AgentEvent;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};

/// Convert a raw Thinking/ToolStart text into a natural, human-readable status line.
/// Avoids the robotic "Axon is X..." pattern that breaks grammar on many inputs.
fn humanize_status(raw: &str) -> String {
    let s = raw.trim();
    // Already a natural sentence (ends with punctuation or starts with capital + verb)
    if s.ends_with('!') || s.ends_with('?') {
        return s.to_string();
    }
    // Map known internal strings to friendly equivalents
    match s {
        s if s.starts_with("Iteration ") => {
            // e.g. "Iteration 2/10" → "Working on it..."
            format!("Working on it...")
        }
        "Analyzing request..." => "Let me think about this...".into(),
        "Drafting response..." => "Drafting a response...".into(),
        "Planning next step..." => "Planning the next step...".into(),
        "Reviewing tool results..." => "Reviewing the tool results...".into(),
        "Refining response..." => "Refining the response...".into(),
        "Quality checking response..." => "Double-checking my answer...".into(),
        s if s.starts_with("Quality issue found") => "Refining my response...".into(),
        s if s.starts_with("Nudging model") => "Trying a different approach...".into(),
        s if s.starts_with("Corrected service mismatch") => "Adjusting tool selection...".into(),
        s if s.starts_with("Waiting for the model response...") => {
            "Waiting for the model response...".into()
        }
        s if s.starts_with("Waiting for the model to decide the next step...") => {
            "Waiting for the model to plan the next step...".into()
        }
        s if s.starts_with("Tool '") && s.contains("missing — writing") => {
            "Building a new tool for this task...".into()
        }
        // ToolStart texts like "working with gmail_list"
        s if s.starts_with("working with ") => {
            let tool = s.trim_start_matches("working with ").trim_end_matches('.');
            let friendly = tool
                .replace('_', " ")
                .replace("fb ", "Facebook ")
                .replace("gdrive", "Google Drive")
                .replace("gcal", "Google Calendar")
                .replace("mscal", "Microsoft Calendar")
                .replace("onedrive", "OneDrive");
            format!("Using {}...", friendly)
        }
        // Anything else — just keep it as-is, don't prepend "Axon is"
        other => {
            if other.ends_with("...") {
                other.to_string()
            } else {
                format!("{}...", other)
            }
        }
    }
}

pub async fn stream_to_gateway(
    mut rx: mpsc::Receiver<AgentEvent>,
    gateway: Arc<dyn MessageGateway>,
    chat_id: String,
) -> Result<String> {
    let mut message_id: Option<String> = None;
    let mut status_msg = String::new();
    let mut extracted_files: Vec<String> = vec![];
    let mut accumulated_thoughts = String::new();
    let mut current_iteration_tokens = String::new();
    let mut last_update = Instant::now();
    let mut last_event = Instant::now(); // tracks last received event for stuck detection
    let mut final_answer = String::new();
    let mut is_done = false;
    let mut received_done = false; // guard: did we cleanly receive Done/Error?

    // Heartbeat interval — if we haven't received any event for this long, show
    // a "still working" nudge so the user knows the stream isn't frozen.
    let heartbeat = Duration::from_secs(20);
    // Hard timeout — if nothing happens for this long, abandon and say so.
    let hard_timeout = Duration::from_secs(120);

    loop {
        // Wait for the next event with a timeout for heartbeat detection
        let maybe_event = tokio::time::timeout(heartbeat, rx.recv()).await;

        match maybe_event {
            // ── Normal event received ────────────────────────────────────────
            Ok(Some(event)) => {
                last_event = Instant::now();
                let mut force_update = false;

                match event {
                    AgentEvent::Thinking { text, .. } => {
                        status_msg = humanize_status(&text);
                    }
                    AgentEvent::ToolStart { tool, .. } => {
                        if !current_iteration_tokens.is_empty() {
                            if !accumulated_thoughts.is_empty() {
                                accumulated_thoughts.push_str("\n\n");
                            }
                            accumulated_thoughts.push_str(current_iteration_tokens.trim());
                            current_iteration_tokens.clear();
                        }
                        status_msg =
                            humanize_status(&format!("working with {}", tool.replace("_tool", "")));
                        force_update = true;
                    }
                    AgentEvent::Token { text, .. } => {
                        current_iteration_tokens.push_str(&text);
                        status_msg.clear();
                    }
                    AgentEvent::Done { full_text, .. } => {
                        let mut clean = full_text.trim().to_string();
                        if clean.to_lowercase().starts_with("final answer:") {
                            clean = clean[13..].trim().to_string();
                        } else if clean.to_lowercase().starts_with("final answer") {
                            clean = clean[12..].trim().to_string();
                            if clean.starts_with(':') {
                                clean = clean[1..].trim().to_string();
                            }
                        }
                        while let (Some(s), Some(e)) =
                            (clean.find("<send_file>"), clean.find("</send_file>"))
                        {
                            if s < e {
                                let path = clean[s + 11..e].trim().to_string();
                                extracted_files.push(path);
                                clean = format!("{}{}", &clean[..s], &clean[e + 12..]);
                            } else {
                                break;
                            }
                        }
                        final_answer = clean.trim().to_string();
                        accumulated_thoughts.clear();
                        current_iteration_tokens.clear();
                        is_done = true;
                        received_done = true;
                        force_update = true;
                    }
                    AgentEvent::Error { message, .. } => {
                        final_answer = format!("⚠️ {}", message);
                        is_done = true;
                        received_done = true;
                        force_update = true;
                    }
                    AgentEvent::Notification {
                        level,
                        title,
                        message,
                        ..
                    } => {
                        let mut note = String::new();
                        let normalized_level = level.trim().to_uppercase();
                        if !normalized_level.is_empty() {
                            note.push('[');
                            note.push_str(&normalized_level);
                            note.push_str("] ");
                        }
                        if !title.trim().is_empty() {
                            note.push_str(title.trim());
                            note.push('\n');
                        }
                        note.push_str(message.trim());
                        let _ = gateway.send_text(&chat_id, note.trim()).await;
                    }
                    _ => {}
                }

                // Throttle updates (at most once per 1.5s unless forced)
                let elapsed = last_update.elapsed();
                if force_update
                    || (elapsed > Duration::from_millis(1500)
                        && (!accumulated_thoughts.is_empty()
                            || !current_iteration_tokens.is_empty()
                            || !status_msg.is_empty()))
                {
                    let display_text = build_display(
                        is_done,
                        &final_answer,
                        &accumulated_thoughts,
                        &current_iteration_tokens,
                        &status_msg,
                    );

                    if !display_text.is_empty() {
                        if let Some(ref mid) = message_id {
                            let _ = gateway.edit_text(&chat_id, mid, &display_text).await;
                        } else {
                            if let Ok(mid) = gateway.send_text(&chat_id, &display_text).await {
                                message_id = Some(mid);
                            }
                        }
                        last_update = Instant::now();
                    }

                    if is_done {
                        break;
                    }
                }
            }

            // ── Channel closed (agent task finished/panicked) ────────────────
            Ok(None) => {
                if !received_done && !final_answer.is_empty() {
                    // We had partial content — show it
                    if let Some(ref mid) = message_id {
                        let _ = gateway.edit_text(&chat_id, mid, &final_answer).await;
                    } else {
                        let _ = gateway.send_text(&chat_id, &final_answer).await;
                    }
                } else if !received_done {
                    // Channel closed without a Done event — task died unexpectedly
                    let err = "⚠️ Something went wrong and the response was interrupted. Please try again.";
                    if let Some(ref mid) = message_id {
                        let _ = gateway.edit_text(&chat_id, mid, err).await;
                    } else {
                        let _ = gateway.send_text(&chat_id, err).await;
                    }
                }
                break;
            }

            // ── Heartbeat timeout — no event for `heartbeat` seconds ─────────
            Err(_timeout) => {
                if last_event.elapsed() >= hard_timeout {
                    // Hard timeout hit — give up and notify user
                    let err =
                        "⚠️ The task is taking too long and has been stopped. Please try again.";
                    if let Some(ref mid) = message_id {
                        let _ = gateway.edit_text(&chat_id, mid, err).await;
                    } else {
                        let _ = gateway.send_text(&chat_id, err).await;
                    }
                    break;
                } else {
                    // Soft heartbeat — still alive, just slow. Edit with a "still working" nudge.
                    let still_working = build_display(
                        false,
                        "",
                        &accumulated_thoughts,
                        &current_iteration_tokens,
                        "Still working on it, hang tight...",
                    );
                    if !still_working.is_empty() {
                        if let Some(ref mid) = message_id {
                            let _ = gateway.edit_text(&chat_id, mid, &still_working).await;
                        } else if let Ok(mid) = gateway.send_text(&chat_id, &still_working).await {
                            message_id = Some(mid);
                        }
                        last_update = Instant::now();
                    }
                }
            }
        }
    }

    // Send extracted files
    for path in extracted_files {
        if let Ok(data) = std::fs::read(&path) {
            let filename = std::path::Path::new(&path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let mime_type = mime_guess::from_path(&path)
                .first_or_octet_stream()
                .to_string();
            let _ = gateway
                .send_file(
                    &chat_id,
                    OutgoingFile {
                        filename,
                        mime_type,
                        data,
                    },
                )
                .await;
        }
    }

    Ok(final_answer)
}

/// Build the display string shown to the user during and after streaming.
fn build_display(
    is_done: bool,
    final_answer: &str,
    accumulated_thoughts: &str,
    current_tokens: &str,
    status: &str,
) -> String {
    if is_done {
        return final_answer.trim().to_string();
    }
    let mut parts: Vec<&str> = Vec::new();
    if !accumulated_thoughts.is_empty() {
        parts.push(accumulated_thoughts.trim());
    }
    if !current_tokens.is_empty() {
        parts.push(current_tokens.trim());
    }
    let mut display = parts.join("\n\n");
    if !status.is_empty() {
        if !display.is_empty() {
            display.push_str("\n\n");
        }
        display.push_str(status);
    }
    display.trim().to_string()
}
