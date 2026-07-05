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
            Tool {
                name: "microsoft_auth_url".into(),
                description:
                    "Get Microsoft OAuth2 authorization URL. Open it in a browser to sign in."
                        .into(),
                input_schema: schema!({}, []),
            },
            Tool {
                name: "microsoft_exchange_code".into(),
                description: "Exchange Microsoft OAuth code for tokens.".into(),
                input_schema: schema!({"code":{"type":"string"}}, ["code"]),
            },
            Tool {
                name: "microsoft_auth_status".into(),
                description: "Check Microsoft authentication status.".into(),
                input_schema: schema!({}, []),
            },
            Tool {
                name: "microsoft_revoke".into(),
                description: "Delete stored Microsoft tokens (local revoke).".into(),
                input_schema: schema!({}, []),
            },
            // Outlook
            Tool {
                name: "outlook_list_emails".into(),
                description: "List Outlook emails with subject, from, date, and preview. Use this when asked to check Outlook/Microsoft email or unread mail (filter 'isRead eq false').".into(),
                input_schema: schema!({"max_items":{"type":"integer","default":10},"folder_id":{"type":"string","description":"Folder ID (default: inbox)"},"filter":{"type":"string","description":"OData filter, e.g. isRead eq false"}}, []),
            },
            Tool {
                name: "outlook_get_email".into(),
                description: "Get a full Outlook email with body.".into(),
                input_schema: schema!({"message_id":{"type":"string"}}, ["message_id"]),
            },
            Tool {
                name: "outlook_send_email".into(),
                description: "Send an Outlook email.".into(),
                input_schema: schema!({"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"},"cc":{"type":"string"},"bcc":{"type":"string"},"is_html":{"type":"boolean","default":false}}, ["to","subject","body"]),
            },
            Tool {
                name: "outlook_reply".into(),
                description: "Reply to an Outlook message.".into(),
                input_schema: schema!({"message_id":{"type":"string"},"body":{"type":"string"},"reply_all":{"type":"boolean","default":false}}, ["message_id","body"]),
            },
            Tool {
                name: "outlook_search".into(),
                description: "Search Outlook emails by keyword.".into(),
                input_schema: schema!({"query":{"type":"string"},"max_items":{"type":"integer","default":10}}, ["query"]),
            },
            Tool {
                name: "outlook_delete".into(),
                description: "Delete an Outlook email.".into(),
                input_schema: schema!({"message_id":{"type":"string"}}, ["message_id"]),
            },
            Tool {
                name: "outlook_mark_read".into(),
                description: "Mark an Outlook email as read or unread.".into(),
                input_schema: schema!({"message_id":{"type":"string"},"is_read":{"type":"boolean","default":true}}, ["message_id"]),
            },
            Tool {
                name: "outlook_list_folders".into(),
                description: "List Outlook mail folders.".into(),
                input_schema: schema!({}, []),
            },
            Tool {
                name: "outlook_download_attachment".into(),
                description: "Download an Outlook attachment to a local file path so the agent can upload/send it.".into(),
                input_schema: schema!({"message_id":{"type":"string"},"attachment_id":{"type":"string"},"filename":{"type":"string"}}, ["message_id","attachment_id","filename"]),
            },
            // Calendar
            Tool {
                name: "mscal_list_calendars".into(),
                description: "List all Microsoft calendars in the user's account. Use this to discover calendar IDs before querying a specific calendar.".into(),
                input_schema: schema!({}, []),
            },
            Tool {
                name: "mscal_list_events".into(),
                description: "List Microsoft Calendar events. Use this when asked about the Outlook/Microsoft calendar or schedule. Supports free-text search via 'query'. When no time window is given, listing covers now through the next 30 days. Note: 'calendar_id' and 'query' cannot be used together — if both are supplied an error is returned.".into(),
                input_schema: schema!({"max_count":{"type":"integer","default":10,"description":"Max events to return (up to 50)"},"start_datetime":{"type":"string","description":"Window start. Any common datetime format works: ISO 8601, '2026-07-05 09:00', 'July 5, 2026 3pm', or a Unix timestamp.","displayOptions":{"inlineGroup":"time_window"}},"end_datetime":{"type":"string","description":"Window end. Accepts the same flexible formats as start_datetime.","displayOptions":{"inlineGroup":"time_window"}},"query":{"type":"string","description":"Free-text search. Cannot be combined with calendar_id."},"calendar_id":{"type":"string","description":"Specific calendar ID. Cannot be combined with query."}}, []),
            },
            Tool {
                name: "mscal_get_event".into(),
                description: "Get a single Microsoft Calendar event by ID, including full body, attendees, and online meeting details.".into(),
                input_schema: schema!({"event_id":{"type":"string"}}, ["event_id"]),
            },
            Tool { name: "mscal_create_event".into(), description: "Create a Microsoft Calendar event. Defaults to 'Asia/Manila' timezone. Set 'is_online_meeting' to true to generate a Microsoft Teams meeting link. For an ALL-DAY event pass dates only (e.g. start '2025-06-15', end '2025-06-15').".into(),
                input_schema: schema!({
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
                }, ["subject","start","end"])
            },
            Tool { name: "mscal_update_event".into(), description: "Update a Microsoft Calendar event. Only the provided fields change — blank fields are left untouched. Defaults to 'Asia/Manila' timezone. A date alone ('2025-06-15') switches the event to all-day; timed values switch it back.".into(),
                input_schema: schema!({
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
                }, ["event_id"])
            },
            Tool {
                name: "mscal_delete_event".into(),
                description: "Delete a Microsoft Calendar event silently (no attendee notification). Use mscal_cancel_event instead if you are the organizer and want to notify attendees.".into(),
                input_schema: schema!({"event_id":{"type":"string"}}, ["event_id"]),
            },
            Tool {
                name: "mscal_cancel_event".into(),
                description: "Cancel a Microsoft Calendar event as organizer and send a cancellation notice to all attendees. Use this instead of delete when you want attendees to be notified.".into(),
                input_schema: schema!({"event_id":{"type":"string"},"comment":{"type":"string","description":"Optional message to include in the cancellation notice"}}, ["event_id"]),
            },
            Tool {
                name: "mscal_accept_event".into(),
                description: "Accept a meeting invitation.".into(),
                input_schema: schema!({"event_id":{"type":"string"},"comment":{"type":"string"}}, ["event_id"]),
            },
            Tool {
                name: "mscal_decline_event".into(),
                description: "Decline a meeting invitation.".into(),
                input_schema: schema!({"event_id":{"type":"string"},"comment":{"type":"string"}}, ["event_id"]),
            },
            Tool {
                name: "mscal_tentatively_accept_event".into(),
                description: "Tentatively accept a meeting invitation.".into(),
                input_schema: schema!({"event_id":{"type":"string"},"comment":{"type":"string"}}, ["event_id"]),
            },
            Tool {
                name: "mscal_get_schedule".into(),
                description: "Check free/busy availability for one or more users or calendars over a time range. Returns busy blocks and a 30-minute-slot availability view.".into(),
                input_schema: schema!({"emails":{"type":"array","description":"List of people to check. Each item is a person with their email address.","items":{"type":"object","properties":{"email":{"type":"string","description":"Email address to check, e.g. john@example.com"}}}},"start":{"type":"string","description":"Window start; any common datetime format or Unix timestamp","displayOptions":{"inlineGroup":"time_window"}},"end":{"type":"string","description":"Window end; same flexible formats as start","displayOptions":{"inlineGroup":"time_window"}},"time_zone":{"type":"string","default":"Asia/Manila"}}, ["emails","start","end"]),
            },
            Tool {
                name: "mscal_find_meeting_times".into(),
                description: "Find available meeting times where all required attendees are free. Returns ranked suggestions within the given time window.".into(),
                input_schema: schema!({"attendees":{"type":"array","description":"List of required attendees. Each item is an attendee with their email address.","items":{"type":"object","properties":{"email":{"type":"string","description":"Attendee email address, e.g. john@example.com"}}}},"duration_minutes":{"type":"integer","description":"Required meeting duration in minutes"},"time_min":{"type":"string","description":"Window start; any common datetime format or Unix timestamp","displayOptions":{"inlineGroup":"time_window"}},"time_max":{"type":"string","description":"Window end; same flexible formats as time_min","displayOptions":{"inlineGroup":"time_window"}},"time_zone":{"type":"string","default":"Asia/Manila"}}, ["attendees","duration_minutes","time_min","time_max"]),
            },
            // OneDrive
            Tool {
                name: "onedrive_list".into(),
                description: "List OneDrive files and folders. Use this when asked what files or folders are in OneDrive.".into(),
                input_schema: schema!({"folder_id":{"type":"string"},"max_count":{"type":"integer","default":10}}, []),
            },
            Tool {
                name: "onedrive_search".into(),
                description: "Search OneDrive files.".into(),
                input_schema: schema!({"query":{"type":"string"},"max_count":{"type":"integer","default":10}}, ["query"]),
            },
            Tool {
                name: "onedrive_move_file".into(),
                description: "Move a OneDrive file to another folder.".into(),
                input_schema: schema!({"item_id":{"type":"string"},"new_folder_id":{"type":"string"}}, ["item_id","new_folder_id"]),
            },
            Tool {
                name: "onedrive_download_binary".into(),
                description: "Download a non-text OneDrive file to a local path so the agent can upload/send it.".into(),
                input_schema: schema!({"item_id":{"type":"string"}}, ["item_id"]),
            },
            Tool {
                name: "onedrive_upload_binary".into(),
                description: "Upload a binary file from a local path to OneDrive.".into(),
                input_schema: schema!({"local_path":{"type":"string","description":"Local file path"},"name":{"type":"string","description":"Target file name in OneDrive"},"folder_id":{"type":"string"}}, ["local_path","name"]),
            },
            Tool {
                name: "onedrive_create_share_link".into(),
                description: "Create a public viewable web link for a OneDrive file to share with anyone.".into(),
                input_schema: schema!({"item_id":{"type":"string"}}, ["item_id"]),
            },
            Tool {
                name: "outlook_send_with_attachment".into(),
                description: "Send an Outlook email with an attachment (Max 3MB).".into(),
                input_schema: schema!({"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"},"local_path":{"type":"string","description":"Local file path to attach"},"is_html":{"type":"boolean","default":false}}, ["to","subject","body","local_path"]),
            },
            Tool {
                name: "onedrive_delete".into(),
                description: "Delete a OneDrive item.".into(),
                input_schema: schema!({"item_id":{"type":"string"}}, ["item_id"]),
            },
            // Teams
            Tool {
                name: "teams_list_joined".into(),
                description: "List Microsoft Teams the user has joined.".into(),
                input_schema: schema!({}, []),
            },
            Tool {
                name: "teams_list_channels".into(),
                description: "List channels in a Team.".into(),
                input_schema: schema!({"team_id":{"type":"string"}}, ["team_id"]),
            },
            Tool {
                name: "teams_send_message".into(),
                description: "Send a message to a Teams channel.".into(),
                input_schema: schema!({"team_id":{"type":"string"},"channel_id":{"type":"string"},"content":{"type":"string"}}, ["team_id","channel_id","content"]),
            },
            Tool {
                name: "teams_list_chats".into(),
                description: "List personal Teams chats.".into(),
                input_schema: schema!({"max_count":{"type":"integer","default":10}}, []),
            },
            Tool {
                name: "teams_send_chat_message".into(),
                description: "Send a message to a Teams personal chat.".into(),
                input_schema: schema!({"chat_id":{"type":"string"},"content":{"type":"string"}}, ["chat_id","content"]),
            },
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

            "mscal_list_calendars" => calendar::list_calendars(&self.0).await,
            "mscal_list_events" => {
                calendar::list_events(
                    &self.0,
                    n("max_count", 10.0).min(50.0) as u32,
                    a.get("start_datetime").and_then(|v| v.as_str()),
                    a.get("end_datetime").and_then(|v| v.as_str()),
                    a.get("query").and_then(|v| v.as_str()),
                    a.get("calendar_id").and_then(|v| v.as_str()),
                )
                .await
            }
            "mscal_get_event" => calendar::get_event(&self.0, s("event_id")?).await,
            "mscal_create_event" => {
                calendar::create_event(
                    &self.0,
                    s("subject")?,
                    s("start")?,
                    s("end")?,
                    a.get("time_zone")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Asia/Manila"),
                    a.get("body").and_then(|v| v.as_str()),
                    a.get("location").and_then(|v| v.as_str()),
                    json_arr_opt(a, "attendees"),
                    b("is_online_meeting", false),
                )
                .await
            }
            "mscal_update_event" => {
                calendar::update_event(
                    &self.0,
                    s("event_id")?,
                    a.get("subject").and_then(|v| v.as_str()),
                    a.get("start").and_then(|v| v.as_str()),
                    a.get("end").and_then(|v| v.as_str()),
                    a.get("body").and_then(|v| v.as_str()),
                    a.get("location").and_then(|v| v.as_str()),
                    a.get("time_zone").and_then(|v| v.as_str()),
                )
                .await
            }
            "mscal_delete_event" => calendar::delete_event(&self.0, s("event_id")?).await,
            "mscal_cancel_event" => {
                calendar::cancel_event(
                    &self.0,
                    s("event_id")?,
                    a.get("comment").and_then(|v| v.as_str()),
                )
                .await
            }
            "mscal_accept_event" => {
                calendar::respond_event(
                    &self.0,
                    s("event_id")?,
                    "accept",
                    a.get("comment").and_then(|v| v.as_str()),
                )
                .await
            }
            "mscal_decline_event" => {
                calendar::respond_event(
                    &self.0,
                    s("event_id")?,
                    "decline",
                    a.get("comment").and_then(|v| v.as_str()),
                )
                .await
            }
            "mscal_tentatively_accept_event" => {
                calendar::respond_event(
                    &self.0,
                    s("event_id")?,
                    "tentativelyAccept",
                    a.get("comment").and_then(|v| v.as_str()),
                )
                .await
            }
            "mscal_get_schedule" => {
                calendar::get_schedule(
                    &self.0,
                    json_arr_opt(a, "emails").unwrap_or_default(),
                    s("start")?,
                    s("end")?,
                    a.get("time_zone").and_then(|v| v.as_str()),
                )
                .await
            }
            "mscal_find_meeting_times" => {
                calendar::find_meeting_times(
                    &self.0,
                    json_arr_opt(a, "attendees").unwrap_or_default(),
                    n("duration_minutes", 30.0) as u32,
                    s("time_min")?,
                    s("time_max")?,
                    a.get("time_zone").and_then(|v| v.as_str()),
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

fn json_arr_opt<'a>(args: &'a Map<String, Value>, key: &str) -> Option<Vec<&'a str>> {
    args.get(key)?
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
}
