pub mod auth;
pub mod calendar;
pub mod onedrive;
pub mod outlook;
pub mod teams;

use anyhow::Result;
use axon_core::{err_json, ok_json, schema, AppState};
use rmcp::model::{CallToolResult, Tool};
use serde_json::{Map, Value};
use std::sync::Arc;

pub struct MicrosoftService(pub Arc<AppState>);

impl MicrosoftService {
    pub fn new(state: Arc<AppState>) -> Self {
        Self(state)
    }

    pub fn tool_list() -> Vec<Tool> {
        vec![
            // Auth
            Tool::new("microsoft_auth_url", "Get Microsoft OAuth2 authorization URL. Open it in a browser to sign in.", schema!({}, [])),
            Tool::new("microsoft_exchange_code", "Exchange Microsoft OAuth code for tokens.", schema!({"code":{"type":"string"}}, ["code"])),
            Tool::new("microsoft_auth_status", "Check Microsoft authentication status.", schema!({}, [])),
            Tool::new("microsoft_revoke", "Delete stored Microsoft tokens (local revoke).", schema!({}, [])),
            // Outlook
            Tool::new("outlook_list_emails", "List Outlook emails with subject, from, date, and preview. Use this when asked to check Outlook/Microsoft email or unread mail (filter 'isRead eq false').", schema!({"max_items":{"type":"integer","default":10},"folder_id":{"type":"string","description":"Folder ID (default: inbox)"},"filter":{"type":"string","description":"OData filter, e.g. isRead eq false"}}, [])),
            Tool::new("outlook_get_email", "Get a full Outlook email with body.", schema!({"message_id":{"type":"string"}}, ["message_id"])),
            Tool::new("outlook_send_email", "Send an Outlook email.", schema!({"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"},"cc":{"type":"string"},"bcc":{"type":"string"},"is_html":{"type":"boolean","default":false}}, ["to","subject","body"])),
            Tool::new("outlook_reply", "Reply to an Outlook message.", schema!({"message_id":{"type":"string"},"body":{"type":"string"},"reply_all":{"type":"boolean","default":false}}, ["message_id","body"])),
            Tool::new("outlook_search", "Search Outlook emails by keyword.", schema!({"query":{"type":"string"},"max_items":{"type":"integer","default":10}}, ["query"])),
            Tool::new("outlook_delete", "Delete an Outlook email.", schema!({"message_id":{"type":"string"}}, ["message_id"])),
            Tool::new("outlook_mark_read", "Mark an Outlook email as read or unread.", schema!({"message_id":{"type":"string"},"is_read":{"type":"boolean","default":true}}, ["message_id"])),
            Tool::new("outlook_list_folders", "List Outlook mail folders.", schema!({}, [])),
            Tool::new("outlook_download_attachment", "Download an Outlook attachment to a local file path so the agent can upload/send it.", schema!({"message_id":{"type":"string"},"attachment_id":{"type":"string"},"filename":{"type":"string"}}, ["message_id","attachment_id","filename"])),
            // Calendar
            Tool::new("mscal_list_calendars", "List all Microsoft calendars in the user's account. Use this to discover calendar IDs before querying a specific calendar.", schema!({}, [])),
            Tool::new("mscal_list_events", "List Microsoft Calendar events. Use this when asked about the Outlook/Microsoft calendar or schedule. Supports free-text search via 'query'. When no time window is given, listing covers now through the next 30 days. Note: 'calendar_id' and 'query' cannot be used together — if both are supplied an error is returned.", schema!({"max_count":{"type":"integer","default":10,"description":"Max events to return (up to 50)"},"start_datetime":{"type":"string","description":"Window start. Any common datetime format works: ISO 8601, '2026-07-05 09:00', 'July 5, 2026 3pm', or a Unix timestamp.","displayOptions":{"inlineGroup":"time_window"}},"end_datetime":{"type":"string","description":"Window end. Accepts the same flexible formats as start_datetime.","displayOptions":{"inlineGroup":"time_window"}},"query":{"type":"string","description":"Free-text search. Cannot be combined with calendar_id."},"calendar_id":{"type":"string","description":"Specific calendar ID. Cannot be combined with query."}}, [])),
            Tool::new("mscal_get_event", "Get a single Microsoft Calendar event by ID, including full body, attendees, and online meeting details.", schema!({"event_id":{"type":"string"}}, ["event_id"])),
            Tool::new("mscal_create_event", "Create a Microsoft Calendar event. Defaults to 'Asia/Manila' timezone. Set 'is_online_meeting' to true to generate a Microsoft Teams meeting link. For an ALL-DAY event pass dates only (e.g. start '2025-06-15', end '2025-06-15').", schema!({
                    "subject":           { "type": "string",  "description": "Event title / name. What the event is called, e.g. 'Team Standup' or 'Doctor Appointment'." },
                    "start":             { "type": "string",  "description": "Start date and time, e.g. '2025-06-15T09:00:00'. Any common format works: ISO 8601, '2025-06-15 09:00', 'June 15, 2025 9am', '06/15/2025 9:00 AM', or a Unix timestamp. A date alone ('2025-06-15') makes an all-day event.", "displayOptions": { "inlineGroup": "event_time" } },
                    "end":               { "type": "string",  "description": "End date and time, e.g. '2025-06-15T10:00:00'. Accepts the same flexible formats as start. Must be after start. For all-day events use a date; same date as start means a one-day event.", "displayOptions": { "inlineGroup": "event_time" } },
                    "body":              { "type": "string",  "description": "Optional notes or agenda for the event. Supports plain text details about what this event is about." },
                    "location":          { "type": "string",  "description": "Physical or virtual place where the event occurs, e.g. 'Teams', 'Conference Room A', or a full address." },
                    "attendees":         { "type": "array",   "description": "List of people to invite to this event. Each item is an attendee with their email address.", "items": { "type": "object", "properties": { "email": { "type": "string", "description": "Attendee email address, e.g. john@example.com" } } } },
                    "time_zone":         { "type": "string",  "description": "Timezone for the event times, e.g. 'Asia/Manila', 'America/New_York'.", "default": "Asia/Manila",
                        "enum": ["Asia/Manila","Asia/Singapore","Asia/Tokyo","Asia/Hong_Kong","Asia/Seoul","Asia/Bangkok","Asia/Kolkata","Asia/Dubai","Asia/Karachi","Asia/Jakarta","Asia/Shanghai","Australia/Sydney","Australia/Melbourne","Europe/London","Europe/Paris","Europe/Berlin","Europe/Rome","Europe/Madrid","Europe/Amsterdam","Europe/Moscow","America/New_York","America/Chicago","America/Denver","America/Los_Angeles","America/Toronto","America/Vancouver","America/Sao_Paulo","America/Buenos_Aires","America/Mexico_City","America/Bogota","Africa/Cairo","Africa/Lagos","Africa/Nairobi","Pacific/Auckland","Pacific/Honolulu","UTC"]
                    },
                    "is_online_meeting": { "type": "boolean", "description": "Set to true to automatically generate a Microsoft Teams online meeting link for this event.", "default": false }
                }, ["subject","start","end"])),
            Tool::new("mscal_update_event", "Update a Microsoft Calendar event. Only the provided fields change — blank fields are left untouched. Defaults to 'Asia/Manila' timezone. A date alone ('2025-06-15') switches the event to all-day; timed values switch it back.", schema!({
                    "event_id":  { "type": "string", "description": "ID of the event to update." },
                    "subject":   { "type": "string", "description": "New event title / name." },
                    "start":     { "type": "string", "description": "New start time, e.g. '2025-06-15T09:00:00'. Any common datetime format or a Unix timestamp works. A date alone ('2025-06-15') switches the event to all-day." },
                    "end":       { "type": "string", "description": "New end time, e.g. '2025-06-15T10:00:00'. Accepts the same flexible formats as start. For all-day events use a date." },
                    "body":      { "type": "string", "description": "New event notes / agenda." },
                    "location":  { "type": "string", "description": "New event location." },
                    "attendees": { "type": "array",  "description": "Updated attendee list (replaces the existing one). Each item is an attendee with their email.", "items": { "type": "object", "properties": { "email": { "type": "string", "description": "Attendee email address" } } } },
                    "time_zone": { "type": "string", "description": "Timezone for the updated event times.", "default": "Asia/Manila",
                        "enum": ["Asia/Manila","Asia/Singapore","Asia/Tokyo","Asia/Hong_Kong","Asia/Seoul","Asia/Bangkok","Asia/Kolkata","Asia/Dubai","Asia/Karachi","Asia/Jakarta","Asia/Shanghai","Australia/Sydney","Australia/Melbourne","Europe/London","Europe/Paris","Europe/Berlin","Europe/Rome","Europe/Madrid","Europe/Amsterdam","Europe/Moscow","America/New_York","America/Chicago","America/Denver","America/Los_Angeles","America/Toronto","America/Vancouver","America/Sao_Paulo","America/Buenos_Aires","America/Mexico_City","America/Bogota","Africa/Cairo","Africa/Lagos","Africa/Nairobi","Pacific/Auckland","Pacific/Honolulu","UTC"]
                    }
                }, ["event_id"])),
            Tool::new("mscal_delete_event", "Delete a Microsoft Calendar event silently (no attendee notification). Use mscal_cancel_event instead if you are the organizer and want to notify attendees.", schema!({"event_id":{"type":"string"}}, ["event_id"])),
            Tool::new("mscal_cancel_event", "Cancel a Microsoft Calendar event as organizer and send a cancellation notice to all attendees. Use this instead of delete when you want attendees to be notified.", schema!({"event_id":{"type":"string"},"comment":{"type":"string","description":"Optional message to include in the cancellation notice"}}, ["event_id"])),
            Tool::new("mscal_accept_event", "Accept a meeting invitation.", schema!({"event_id":{"type":"string"},"comment":{"type":"string"}}, ["event_id"])),
            Tool::new("mscal_decline_event", "Decline a meeting invitation.", schema!({"event_id":{"type":"string"},"comment":{"type":"string"}}, ["event_id"])),
            Tool::new("mscal_tentatively_accept_event", "Tentatively accept a meeting invitation.", schema!({"event_id":{"type":"string"},"comment":{"type":"string"}}, ["event_id"])),
            Tool::new("mscal_get_schedule", "Check free/busy availability for one or more users or calendars over a time range. Returns busy blocks and a 30-minute-slot availability view.", schema!({"emails":{"type":"array","description":"List of people to check. Each item is a person with their email address.","items":{"type":"object","properties":{"email":{"type":"string","description":"Email address to check, e.g. john@example.com"}}}},"start":{"type":"string","description":"Window start; any common datetime format or Unix timestamp","displayOptions":{"inlineGroup":"time_window"}},"end":{"type":"string","description":"Window end; same flexible formats as start","displayOptions":{"inlineGroup":"time_window"}},"time_zone":{"type":"string","default":"Asia/Manila"}}, ["emails","start","end"])),
            Tool::new("mscal_find_meeting_times", "Find available meeting times where all required attendees are free. Returns ranked suggestions within the given time window.", schema!({"attendees":{"type":"array","description":"List of required attendees. Each item is an attendee with their email address.","items":{"type":"object","properties":{"email":{"type":"string","description":"Attendee email address, e.g. john@example.com"}}}},"duration_minutes":{"type":"integer","description":"Required meeting duration in minutes"},"time_min":{"type":"string","description":"Window start; any common datetime format or Unix timestamp","displayOptions":{"inlineGroup":"time_window"}},"time_max":{"type":"string","description":"Window end; same flexible formats as time_min","displayOptions":{"inlineGroup":"time_window"}},"time_zone":{"type":"string","default":"Asia/Manila"}}, ["attendees","duration_minutes","time_min","time_max"])),
            // OneDrive
            Tool::new("onedrive_list", "List OneDrive files and folders. Use this when asked what files or folders are in OneDrive.", schema!({"folder_id":{"type":"string"},"max_count":{"type":"integer","default":10}}, [])),
            Tool::new("onedrive_search", "Search OneDrive files.", schema!({"query":{"type":"string"},"max_count":{"type":"integer","default":10}}, ["query"])),
            Tool::new("onedrive_move_file", "Move a OneDrive file to another folder.", schema!({"item_id":{"type":"string"},"new_folder_id":{"type":"string"}}, ["item_id","new_folder_id"])),
            Tool::new("onedrive_download_binary", "Download a non-text OneDrive file to a local path so the agent can upload/send it.", schema!({"item_id":{"type":"string"}}, ["item_id"])),
            Tool::new("onedrive_upload_binary", "Upload a binary file from a local path to OneDrive.", schema!({"local_path":{"type":"string","description":"Local file path"},"name":{"type":"string","description":"Target file name in OneDrive"},"folder_id":{"type":"string"}}, ["local_path","name"])),
            Tool::new("onedrive_create_share_link", "Create a public viewable web link for a OneDrive file to share with anyone.", schema!({"item_id":{"type":"string"}}, ["item_id"])),
            Tool::new("outlook_send_with_attachment", "Send an Outlook email with an attachment (Max 3MB).", schema!({"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"},"local_path":{"type":"string","description":"Local file path to attach"},"is_html":{"type":"boolean","default":false}}, ["to","subject","body","local_path"])),
            Tool::new("onedrive_delete", "Delete a OneDrive item.", schema!({"item_id":{"type":"string"}}, ["item_id"])),
            // Teams
            Tool::new("teams_list_joined", "List Microsoft Teams the user has joined.", schema!({}, [])),
            Tool::new("teams_list_channels", "List channels in a Team.", schema!({"team_id":{"type":"string"}}, ["team_id"])),
            Tool::new("teams_send_message", "Send a message to a Teams channel.", schema!({"team_id":{"type":"string"},"channel_id":{"type":"string"},"content":{"type":"string"}}, ["team_id","channel_id","content"])),
            Tool::new("teams_list_chats", "List personal Teams chats.", schema!({"max_count":{"type":"integer","default":10}}, [])),
            Tool::new("teams_send_chat_message", "Send a message to a Teams personal chat.", schema!({"chat_id":{"type":"string"},"content":{"type":"string"}}, ["chat_id","content"])),
        ]
    }

    pub async fn call(&self, name: &str, args: Map<String, Value>) -> Result<CallToolResult> {
        let a = &args;
        let s = str!(a);
        let n = num!(a);
        let b = boo!(a);

        let result: Result<Value> = match name {
            "microsoft_auth_url" => auth::auth_url(&self.0).await,
            "microsoft_exchange_code" => auth::exchange_code(&self.0, s("code")?).await,
            "microsoft_auth_status" => auth::auth_status(&self.0).await,
            "microsoft_revoke" => auth::revoke(&self.0).await,

            "outlook_list_emails" => {
                outlook::list(
                    &self.0,
                    n("max_items", 10.0).min(10.0) as u32,
                    a.get("folder_id").and_then(|v| v.as_str()),
                    a.get("filter").and_then(|v| v.as_str()),
                )
                .await
            }
            "outlook_get_email" => outlook::get(&self.0, s("message_id")?).await,
            "outlook_send_email" => {
                outlook::send(
                    &self.0,
                    s("to")?,
                    s("subject")?,
                    s("body")?,
                    a.get("cc").and_then(|v| v.as_str()),
                    a.get("bcc").and_then(|v| v.as_str()),
                    b("is_html", false),
                )
                .await
            }
            "outlook_reply" => {
                outlook::reply(&self.0, s("message_id")?, s("body")?, b("reply_all", false)).await
            }
            "outlook_search" => {
                outlook::search(&self.0, s("query")?, n("max_items", 10.0).min(10.0) as u32).await
            }
            "outlook_delete" => outlook::delete(&self.0, s("message_id")?).await,
            "outlook_mark_read" => {
                outlook::mark_read(&self.0, s("message_id")?, b("is_read", true)).await
            }
            "outlook_list_folders" => outlook::list_folders(&self.0).await,

            // Calendar. Optional params go through opt_str/opt_bool so the
            // blank strings workflow nodes send for untouched fields read as
            // "not provided" instead of empty values; datetime params go
            // through opt_dt/req_dt so bare Unix-timestamp numbers work too.
            "mscal_list_calendars" => calendar::list_calendars(&self.0).await,
            "mscal_list_events" => {
                let start = opt_dt(a, "start_datetime");
                let end = opt_dt(a, "end_datetime");
                calendar::list_events(
                    &self.0,
                    n("max_count", 10.0).clamp(1.0, 50.0) as u32,
                    start.as_deref(),
                    end.as_deref(),
                    opt_str(a, "query"),
                    opt_str(a, "calendar_id"),
                )
                .await
            }
            "mscal_get_event" => calendar::get_event(&self.0, req_str(a, "event_id")?).await,
            "mscal_create_event" => {
                let start = req_dt(a, "start")?;
                let end = req_dt(a, "end")?;
                calendar::create_event(
                    &self.0,
                    req_str(a, "subject")?,
                    &start,
                    &end,
                    opt_str(a, "time_zone"),
                    opt_str(a, "body"),
                    opt_str(a, "location"),
                    extract_attendees(a, "attendees"),
                    opt_bool(a, "is_online_meeting").unwrap_or(false),
                )
                .await
            }
            "mscal_update_event" => {
                let start = opt_dt(a, "start");
                let end = opt_dt(a, "end");
                calendar::update_event(
                    &self.0,
                    req_str(a, "event_id")?,
                    opt_str(a, "subject"),
                    start.as_deref(),
                    end.as_deref(),
                    opt_str(a, "body"),
                    opt_str(a, "location"),
                    opt_str(a, "time_zone"),
                    extract_attendees(a, "attendees"),
                )
                .await
            }
            "mscal_delete_event" => calendar::delete_event(&self.0, req_str(a, "event_id")?).await,
            "mscal_cancel_event" => {
                calendar::cancel_event(&self.0, req_str(a, "event_id")?, opt_str(a, "comment"))
                    .await
            }
            "mscal_accept_event" => {
                calendar::respond_event(
                    &self.0,
                    req_str(a, "event_id")?,
                    "accept",
                    opt_str(a, "comment"),
                )
                .await
            }
            "mscal_decline_event" => {
                calendar::respond_event(
                    &self.0,
                    req_str(a, "event_id")?,
                    "decline",
                    opt_str(a, "comment"),
                )
                .await
            }
            "mscal_tentatively_accept_event" => {
                calendar::respond_event(
                    &self.0,
                    req_str(a, "event_id")?,
                    "tentativelyAccept",
                    opt_str(a, "comment"),
                )
                .await
            }
            "mscal_get_schedule" => {
                let start = req_dt(a, "start")?;
                let end = req_dt(a, "end")?;
                calendar::get_schedule(
                    &self.0,
                    extract_attendees(a, "emails").unwrap_or_default(),
                    &start,
                    &end,
                    opt_str(a, "time_zone"),
                )
                .await
            }
            "mscal_find_meeting_times" => {
                let time_min = req_dt(a, "time_min")?;
                let time_max = req_dt(a, "time_max")?;
                calendar::find_meeting_times(
                    &self.0,
                    extract_attendees(a, "attendees").unwrap_or_default(),
                    n("duration_minutes", 30.0) as u32,
                    &time_min,
                    &time_max,
                    opt_str(a, "time_zone"),
                )
                .await
            }

            "onedrive_list" => {
                onedrive::list(
                    &self.0,
                    a.get("folder_id").and_then(|v| v.as_str()),
                    n("max_count", 10.0).min(10.0) as u32,
                )
                .await
            }
            "onedrive_search" => {
                onedrive::search(&self.0, s("query")?, n("max_count", 10.0).min(10.0) as u32).await
            }
            "onedrive_move_file" => {
                onedrive::move_file(&self.0, s("item_id")?, s("new_folder_id")?).await
            }
            "onedrive_download_binary" => onedrive::download_binary(&self.0, s("item_id")?).await,
            "onedrive_upload_binary" => {
                onedrive::upload_binary(
                    &self.0,
                    s("local_path")?,
                    s("name")?,
                    a.get("folder_id").and_then(|v| v.as_str()),
                )
                .await
            }
            "onedrive_create_share_link" => {
                onedrive::create_share_link(&self.0, s("item_id")?).await
            }
            "outlook_send_with_attachment" => {
                outlook::send_with_attachment(
                    &self.0,
                    s("to")?,
                    s("subject")?,
                    s("body")?,
                    s("local_path")?,
                    b("is_html", false),
                )
                .await
            }
            "outlook_download_attachment" => {
                outlook::download_attachment(
                    &self.0,
                    s("message_id")?,
                    s("attachment_id")?,
                    s("filename")?,
                )
                .await
            }
            "onedrive_delete" => onedrive::delete(&self.0, s("item_id")?).await,

            "teams_list_joined" => teams::list_joined(&self.0).await,
            "teams_list_channels" => teams::list_channels(&self.0, s("team_id")?).await,
            "teams_send_message" => {
                teams::send_message(&self.0, s("team_id")?, s("channel_id")?, s("content")?).await
            }
            "teams_list_chats" => {
                teams::list_chats(&self.0, n("max_count", 10.0).min(10.0) as u32).await
            }
            "teams_send_chat_message" => {
                teams::send_chat_message(&self.0, s("chat_id")?, s("content")?).await
            }

            other => Err(anyhow::anyhow!("Unknown Microsoft tool: {other}")),
        };

        Ok(match result {
            Ok(v) => ok_json(v),
            Err(e) => err_json(e),
        })
    }
}

macro_rules! str {
    ($args:expr) => {
        |key: &str| -> Result<&str> {
            $args
                .get(key)
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing required param '{key}'"))
        }
    };
}
macro_rules! num {
    ($args:expr) => {
        |key: &str, default: f64| -> f64 {
            $args.get(key).and_then(|v| v.as_f64()).unwrap_or(default)
        }
    };
}
macro_rules! boo {
    ($args:expr) => {
        |key: &str, default: bool| -> bool {
            $args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
        }
    };
}
use boo;
use num;
use str;

// ── Workflow-tolerant param helpers ───────────────────────────────────────────
// Same semantics as the Google service crate: workflow nodes send "" for
// every untouched field and preserve JSON types for bare expression
// references, so blanks read as "not provided" and numbers are accepted
// where datetimes are expected.

/// Optional string param. Workflow nodes send `""` for every untouched field
/// (the UI initializes all config keys), so blank must read as "not provided" —
/// treating it as a value would PATCH empty subjects and `dateTime: ""` into
/// Graph 400s.
fn opt_str<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    let s = args.get(key)?.as_str()?.trim();
    (!s.is_empty()).then_some(s)
}

/// Required string param: present and non-empty after trimming, for the same
/// workflow-sends-"" reason as [`opt_str`].
fn req_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    opt_str(args, key).ok_or_else(|| anyhow::anyhow!("missing required param '{key}'"))
}

/// Datetime param: like [`opt_str`] but also accepts bare JSON numbers —
/// an expression that is a single reference to a Unix timestamp (e.g.
/// Telegram's message.date) resolves with its source type preserved, so the
/// value arrives as a number, not a string.
fn opt_dt(args: &Map<String, Value>, key: &str) -> Option<String> {
    match args.get(key)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Required datetime param, same number tolerance as [`opt_dt`].
fn req_dt(args: &Map<String, Value>, key: &str) -> Result<String> {
    opt_dt(args, key).ok_or_else(|| anyhow::anyhow!("missing required param '{key}'"))
}

/// Interpret a config toggle that the UI/LLM may send as a real bool, or as a
/// string/number ("true"/"1"/"yes"/"on") — workflow nodes and serializers are
/// inconsistent about boolean encoding.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
        _ => false,
    }
}

/// Optional boolean that tolerates the string/number encodings workflow
/// serializers produce (see [`truthy`]); null and blank strings read as unset
/// so schema defaults still apply.
fn opt_bool(args: &Map<String, Value>, key: &str) -> Option<bool> {
    match args.get(key)? {
        Value::Null => None,
        Value::String(s) if s.trim().is_empty() => None,
        v => Some(truthy(v)),
    }
}

/// Extract email addresses from either:
///   - Old plain format: ["email@a.com", "email@b.com"]
///   - New fixedCollection format: [{"email": "email@a.com"}, ...] or {"parameters": [{"email": "..."}]}
fn extract_attendees<'a>(args: &'a Map<String, Value>, key: &str) -> Option<Vec<&'a str>> {
    let raw = args.get(key)?;

    // Unwrap fixedCollection envelope: { "parameters": [...] }
    let arr = if let Some(obj) = raw.as_object() {
        obj.get("parameters").and_then(|v| v.as_array())?
    } else {
        raw.as_array()?
    };

    if arr.is_empty() {
        return None;
    }

    let result: Vec<&str> = arr
        .iter()
        .filter_map(|item| {
            if let Some(s) = item.as_str() {
                (!s.is_empty()).then_some(s)
            } else if let Some(obj) = item.as_object() {
                obj.get("email")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            } else {
                None
            }
        })
        .collect();

    (!result.is_empty()).then_some(result)
}
