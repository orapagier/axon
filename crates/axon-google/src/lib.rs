pub mod auth;
pub mod calendar;
pub mod chat;
pub mod contacts;
pub mod docs;
pub mod drive;
pub mod forms;
pub mod gmail;
pub mod meet;
pub mod places;
pub mod sheets;
pub mod slides;
pub mod tasks;
pub mod youtube;

use anyhow::Result;
use axon_core::{err_json, ok_json, schema, AppState};
use rmcp::model::{CallToolResult, Tool};
use serde_json::{json, Map, Value};
use std::sync::Arc;

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

// ── Service ───────────────────────────────────────────────────────────────────

pub struct GoogleService(pub Arc<AppState>);

impl GoogleService {
    pub fn new(state: Arc<AppState>) -> Self {
        Self(state)
    }

    // ── Tool catalogue ────────────────────────────────────────────────────────

    pub fn tool_list() -> Vec<Tool> {
        let mut tools = vec![
            // Auth
            Tool { name: "google_auth_url".into(),      description: "Get the Google OAuth2 URL. Open it in a browser to sign in.".into(),        input_schema: schema!({}, []) },
            Tool { name: "google_exchange_code".into(), description: "Exchange the Google OAuth code for tokens after signing in.".into(),         input_schema: schema!({"code":{"type":"string","description":"The code param from the redirect URL"}}, ["code"]) },
            Tool { name: "google_auth_status".into(),   description: "Check Google authentication status.".into(),                                 input_schema: schema!({}, []) },
            Tool { name: "google_revoke".into(),        description: "Revoke and delete stored Google tokens.".into(),                             input_schema: schema!({}, []) },

            // Gmail
            Tool { name: "gmail_list".into(),       description: "List Gmail messages. Returns id, subject, from, date, snippet.".into(),          input_schema: schema!({"max_results":{"type":"integer","default":10,"description":"Max messages (max 10)"},"query":{"type":"string","description":"Gmail search query, e.g. 'is:unread from:boss@co.com'"}}, []) },
            Tool { name: "gmail_get".into(),        description: "Get a full Gmail message including decoded body.".into(),                         input_schema: schema!({"id":{"type":"string","description":"Message ID"}}, ["id"]) },
            Tool { name: "gmail_send".into(),       description: "Send a Gmail email.".into(),                                                     input_schema: schema!({"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"},"cc":{"type":"string"},"bcc":{"type":"string"}}, ["to","subject","body"]) },
            Tool { name: "gmail_reply".into(),      description: "Reply to a Gmail message thread.".into(),                                        input_schema: schema!({"thread_id":{"type":"string"},"message_id":{"type":"string"},"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"}}, ["thread_id","message_id","to","subject","body"]) },
            Tool { name: "gmail_search".into(),     description: "Search Gmail messages by query string. Limited to 10 results.".into(),           input_schema: schema!({"query":{"type":"string"},"max_results":{"type":"integer","default":10}}, ["query"]) },
            Tool { name: "gmail_trash".into(),      description: "Move a Gmail message to trash.".into(),                                          input_schema: schema!({"id":{"type":"string"}}, ["id"]) },
            Tool { name: "gmail_mark_read".into(),  description: "Mark Gmail messages as read.".into(),                                            input_schema: schema!({"ids":{"type":"array","items":{"type":"string"}}}, ["ids"]) },
            Tool { name: "gmail_add_label".into(),  description: "Add a label to a Gmail message.".into(),                                         input_schema: schema!({"id":{"type":"string"},"label_id":{"type":"string"}}, ["id","label_id"]) },
            Tool { name: "gmail_remove_label".into(),description: "Remove a label from a Gmail message.".into(),                                   input_schema: schema!({"id":{"type":"string"},"label_id":{"type":"string"}}, ["id","label_id"]) },
            Tool { name: "gmail_download_attachment".into(),description: "Download a Gmail attachment to a local file path so the agent can upload/send it.".into(), input_schema: schema!({"message_id":{"type":"string"},"attachment_id":{"type":"string"},"filename":{"type":"string"}}, ["message_id","attachment_id","filename"]) },
            Tool { name: "gmail_list_labels".into(),description: "List all Gmail labels.".into(),                                                  input_schema: schema!({}, []) },
            Tool { name: "gmail_mark_unread".into(),description: "Mark Gmail messages as unread.".into(),                                            input_schema: schema!({"ids":{"type":"array","items":{"type":"string"}}}, ["ids"]) },
            Tool { name: "gmail_untrash".into(),    description: "Restore a Gmail message from trash.".into(),                                       input_schema: schema!({"id":{"type":"string"}}, ["id"]) },
            Tool { name: "gmail_delete".into(),     description: "Permanently delete a Gmail message. This is irreversible.".into(),                  input_schema: schema!({"id":{"type":"string"}}, ["id"]) },
            Tool { name: "gmail_forward".into(),    description: "Forward a Gmail message to another recipient.".into(),                              input_schema: schema!({"message_id":{"type":"string"},"to":{"type":"string"},"extra_note":{"type":"string","description":"Optional note to prepend"}}, ["message_id","to"]) },
            Tool { name: "gmail_create_draft".into(),description: "Create a Gmail draft.".into(),                                                    input_schema: schema!({"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"},"cc":{"type":"string"},"bcc":{"type":"string"}}, ["to","subject","body"]) },
            Tool { name: "gmail_list_drafts".into(),description: "List Gmail drafts.".into(),                                                        input_schema: schema!({"max_results":{"type":"integer","default":10}}, []) },
            Tool { name: "gmail_get_draft".into(), description: "Get a specific Gmail draft by ID.".into(),                                           input_schema: schema!({"draft_id":{"type":"string"}}, ["draft_id"]) },
            Tool { name: "gmail_update_draft".into(),description: "Update a Gmail draft's content.".into(),                                          input_schema: schema!({"draft_id":{"type":"string"},"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"},"cc":{"type":"string"},"bcc":{"type":"string"}}, ["draft_id","to","subject","body"]) },
            Tool { name: "gmail_send_draft".into(),description: "Send an existing Gmail draft.".into(),                                              input_schema: schema!({"draft_id":{"type":"string"}}, ["draft_id"]) },
            Tool { name: "gmail_delete_draft".into(),description: "Delete a Gmail draft.".into(),                                                    input_schema: schema!({"draft_id":{"type":"string"}}, ["draft_id"]) },


            // Calendar
            // Calendar
            Tool {
                name: "gcal_list_calendars".into(),
                description: "List all Google calendars in the user's account. Use this to discover calendar IDs.".into(),
                input_schema: schema!({}, []),
            },
            Tool { name: "gcal_list_events".into(),  description: "List Google Calendar events. Supports free-text search via 'query' parameter. Set 'single_events' to false to discover master events of recurring series (required for series deletion).".into(),                                  input_schema: schema!({"max_results":{"type":"integer","default":10},"time_min":{"type":"string","description":"ISO 8601 start time","displayOptions":{"inlineGroup":"time_window"}},"time_max":{"type":"string","description":"ISO 8601 end time","displayOptions":{"inlineGroup":"time_window"}},"query":{"type":"string","description":"Free-text search terms"},"calendar_id":{"type":"string","default":"primary"},"single_events":{"type":"boolean","default":true}}, []) },
            Tool {
                name: "gcal_get_event".into(),
                description: "Get a single Google Calendar event by ID.".into(),
                input_schema: schema!({"event_id":{"type":"string"},"calendar_id":{"type":"string","default":"primary"}}, ["event_id"]),
            },
            Tool { name: "gcal_create_event".into(), description: "Create a Google Calendar event. Defaults to 'Asia/Manila' timezone. Set 'create_meet_link' to true to generate a Google Meet link. For recurring events, provide RRULE strings in the 'recurrence' array (e.g. ['RRULE:FREQ=WEEKLY;BYDAY=FR']).".into(),
                input_schema: schema!({
                    "summary":         { "type": "string",  "description": "Event title / name (SUMMARY). What the event is called, e.g. 'Team Standup' or 'Doctor Appointment'." },
                    "start":           { "type": "string",  "description": "Start date and time in ISO 8601 format, e.g. '2025-06-15T09:00:00'. Include date AND time for timed events.", "displayOptions": { "inlineGroup": "event_time" } },
                    "end":             { "type": "string",  "description": "End date and time in ISO 8601 format, e.g. '2025-06-15T10:00:00'. Must be after start.", "displayOptions": { "inlineGroup": "event_time" } },
                    "description":     { "type": "string",  "description": "Optional notes or agenda for the event (DESCRIPTION). Supports plain text details about what this event is about." },
                    "location":        { "type": "string",  "description": "Physical or virtual place where the event occurs (LOCATION), e.g. 'Zoom', 'Conference Room A', or a full address." },
                    "attendees":       { "type": "array",   "description": "List of people to invite to this event. Each item is an attendee with their email address.", "items": { "type": "object", "properties": { "email": { "type": "string", "description": "Attendee email address, e.g. john@example.com" } } } },
                    "time_zone":       { "type": "string",  "description": "Timezone for the event times, e.g. 'Asia/Manila', 'America/New_York'.", "default": "Asia/Manila",
                        "enum": ["Asia/Manila","Asia/Singapore","Asia/Tokyo","Asia/Hong_Kong","Asia/Seoul","Asia/Bangkok","Asia/Kolkata","Asia/Dubai","Asia/Karachi","Asia/Jakarta","Asia/Shanghai","Australia/Sydney","Australia/Melbourne","Europe/London","Europe/Paris","Europe/Berlin","Europe/Rome","Europe/Madrid","Europe/Amsterdam","Europe/Moscow","America/New_York","America/Chicago","America/Denver","America/Los_Angeles","America/Toronto","America/Vancouver","America/Sao_Paulo","America/Buenos_Aires","America/Mexico_City","America/Bogota","Africa/Cairo","Africa/Lagos","Africa/Nairobi","Pacific/Auckland","Pacific/Honolulu","UTC"]
                    },
                    "create_meet_link": { "type": "boolean", "description": "Set to true to automatically generate a Google Meet video conference link for this event.", "default": false },
                    "calendar_id":     { "type": "string",  "description": "Which calendar to add this event to. Use 'primary' for your main calendar, or select from your available calendars.", "default": "primary" },
                    "recurrence":      { "type": "array",   "description": "Recurrence rules in RRULE format. Controls how often the event repeats.", "items": { "type": "string" },
                        "enum": ["","RRULE:FREQ=DAILY","RRULE:FREQ=WEEKLY","RRULE:FREQ=MONTHLY","RRULE:FREQ=YEARLY","RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR","RRULE:FREQ=WEEKLY;BYDAY=MO","RRULE:FREQ=WEEKLY;BYDAY=TU","RRULE:FREQ=WEEKLY;BYDAY=WE","RRULE:FREQ=WEEKLY;BYDAY=TH","RRULE:FREQ=WEEKLY;BYDAY=FR","RRULE:FREQ=WEEKLY;BYDAY=SA","RRULE:FREQ=WEEKLY;BYDAY=SU"]
                    }
                }, ["summary","start","end"])
            },
            Tool { name: "gcal_update_event".into(), description: "Update a Google Calendar event. Defaults to 'Asia/Manila' timezone. To edit an entire recurring series, provide the master ID (found via 'gcal_list_events' with single_events=false). You can also update the 'recurrence' rules for a series.".into(),
                input_schema: schema!({
                    "event_id":    { "type": "string", "description": "ID of the event to update." },
                    "summary":     { "type": "string", "description": "New event title / name (SUMMARY)." },
                    "start":       { "type": "string", "description": "New start time in ISO 8601 format, e.g. '2025-06-15T09:00:00'." },
                    "end":         { "type": "string", "description": "New end time in ISO 8601 format, e.g. '2025-06-15T10:00:00'." },
                    "description": { "type": "string", "description": "New event notes / agenda (DESCRIPTION)." },
                    "location":    { "type": "string", "description": "New event location (LOCATION)." },
                    "attendees":   { "type": "array",  "description": "Updated attendee list. Each item is an attendee with their email.", "items": { "type": "object", "properties": { "email": { "type": "string", "description": "Attendee email address" } } } },
                    "time_zone":   { "type": "string", "description": "Timezone for the updated event times.", "default": "Asia/Manila",
                        "enum": ["Asia/Manila","Asia/Singapore","Asia/Tokyo","Asia/Hong_Kong","Asia/Seoul","Asia/Bangkok","Asia/Kolkata","Asia/Dubai","Asia/Karachi","Asia/Jakarta","Asia/Shanghai","Australia/Sydney","Australia/Melbourne","Europe/London","Europe/Paris","Europe/Berlin","Europe/Rome","Europe/Madrid","Europe/Amsterdam","Europe/Moscow","America/New_York","America/Chicago","America/Denver","America/Los_Angeles","America/Toronto","America/Vancouver","America/Sao_Paulo","America/Buenos_Aires","America/Mexico_City","America/Bogota","Africa/Cairo","Africa/Lagos","Africa/Nairobi","Pacific/Auckland","Pacific/Honolulu","UTC"]
                    },
                    "calendar_id": { "type": "string", "description": "Calendar containing the event.", "default": "primary" },
                    "recurrence":  { "type": "array",  "description": "Updated recurrence rules in RRULE format.", "items": { "type": "string" },
                        "enum": ["","RRULE:FREQ=DAILY","RRULE:FREQ=WEEKLY","RRULE:FREQ=MONTHLY","RRULE:FREQ=YEARLY","RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR","RRULE:FREQ=WEEKLY;BYDAY=MO","RRULE:FREQ=WEEKLY;BYDAY=TU","RRULE:FREQ=WEEKLY;BYDAY=WE","RRULE:FREQ=WEEKLY;BYDAY=TH","RRULE:FREQ=WEEKLY;BYDAY=FR","RRULE:FREQ=WEEKLY;BYDAY=SA","RRULE:FREQ=WEEKLY;BYDAY=SU"]
                    }
                }, ["event_id"])
            },
            Tool {
                name: "gcal_delete_event".into(),
                description: "Delete a Google Calendar event. Attendees are notified. Set 'all_events' to true to delete all instances of a recurring event.".into(),
                input_schema: schema!({"event_id":{"type":"string"},"calendar_id":{"type":"string","default":"primary"},"all_events":{"type":"boolean","default":false}}, ["event_id"]),
            },
            Tool {
                name: "gcal_move_event".into(),
                description: "Move an event from one Google calendar to another.".into(),
                input_schema: schema!({"event_id":{"type":"string"},"source_calendar_id":{"type":"string","default":"primary"},"destination_calendar_id":{"type":"string"}}, ["event_id","destination_calendar_id"]),
            },
            Tool { name: "gcal_quick_add".into(),    description: "Quick-add a calendar event from natural language, e.g. 'Team standup tomorrow 10am'.".into(), input_schema: schema!({"text":{"type":"string"},"calendar_id":{"type":"string","default":"primary"}}, ["text"]) },
            Tool { name: "gcal_get_freebusy".into(), description: "Check free/busy time for a list of calendars.".into(),                          input_schema: schema!({"calendar_ids":{"type":"array","items":{"type":"string"}},"time_min":{"type":"string","description":"ISO 8601"},"time_max":{"type":"string","description":"ISO 8601"}}, ["calendar_ids","time_min","time_max"]) },

            // Drive
            Tool { name: "gdrive_list".into(),         description: "List Google Drive files/folders.".into(),                                      input_schema: schema!({"max_results":{"type":"integer","default":10},"folder_id":{"type":"string"},"mime_type":{"type":"string"}}, []) },
            Tool { name: "gdrive_search".into(),       description: "Search Google Drive files by name or content.".into(),                         input_schema: schema!({"query":{"type":"string"},"max_results":{"type":"integer","default":10}}, ["query"]) },
            Tool { name: "gdrive_move_file".into(),    description: "Move a Google Drive file to another folder.".into(),                           input_schema: schema!({"file_id":{"type":"string"},"new_folder_id":{"type":"string"}}, ["file_id","new_folder_id"]) },
            Tool { name: "gdrive_share".into(),        description: "Share a Drive file. Also returns the public webViewLink if type=anyone.".into(),input_schema: schema!({"file_id":{"type":"string"},"role":{"type":"string","default":"reader"},"type":{"type":"string","default":"anyone"},"email":{"type":"string"}}, ["file_id"]) },
            Tool { name: "gdrive_export".into(),       description: "Export a Google Workspace document (Doc, Sheet, Slide) to a specific format like PDF, XLSX, or DOCX.".into(), input_schema: schema!({"file_id":{"type":"string"},"mime_type":{"type":"string","enum":["application/pdf","application/vnd.openxmlformats-officedocument.spreadsheetml.sheet","application/vnd.openxmlformats-officedocument.wordprocessingml.document","text/csv","text/plain","application/zip"]}}, ["file_id","mime_type"]) },
            Tool { name: "gdrive_download_binary".into(),description: "Download a non-text Google Drive file to a local path so the agent can upload/send it.".into(), input_schema: schema!({"file_id":{"type":"string"}}, ["file_id"]) },
            Tool { name: "gdrive_upload_binary".into(),description: "Upload a binary file from a local path to Google Drive.".into(),               input_schema: schema!({"local_path":{"type":"string","description":"Local file path"},"name":{"type":"string","description":"Target file name in Drive"},"mime_type":{"type":"string","default":"application/octet-stream"},"folder_id":{"type":"string"}}, ["local_path","name"]) },
            Tool { name: "gdrive_upload_folder".into(),description: "Upload a local folder recursively to Google Drive, preserving subfolder structure.".into(), input_schema: schema!({"local_folder_path":{"type":"string","description":"Local folder path"},"folder_name":{"type":"string","description":"Optional name for the new root folder in Drive"},"parent_folder_id":{"type":"string","description":"Optional destination parent folder ID"},"include_hidden":{"type":"boolean","default":false,"description":"Include hidden files/folders (dot-prefixed names)"}}, ["local_folder_path"]) },
            Tool { name: "gdrive_delete".into(),       description: "Permanently delete a Google Drive file or folder by ID. Supports bulk deletion with file_ids.".into(),                 input_schema: schema!({"file_id":{"type":"string","description":"Single file/folder ID. Can also be an expression that resolves to an array."},"file_ids":{"type":"array","items":{"type":"string"},"description":"Optional array of file/folder IDs for bulk deletion"}}, []) },
            Tool { name: "gmail_send_with_attachment".into(),description: "Send a Gmail email with a file attachment from a local path.".into(), input_schema: schema!({"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"},"local_path":{"type":"string","description":"Local file path to attach"}}, ["to","subject","body","local_path"]) },

            // Contacts
            Tool { name: "gcon_list_contacts".into(),  description: "List Google contacts (People API).".into(),                                     input_schema: schema!({"max_results":{"type":"integer","default":50}}, []) },
            Tool { name: "gcon_get_contact".into(),   description: "Get a single Google contact by resource name (e.g. 'people/c12345').".into(),    input_schema: schema!({"name":{"type":"string"}}, ["name"]) },
            Tool { name: "gcon_create_contact".into(), description: "Create a new Google contact.".into(),                                            input_schema: schema!({"given_name":{"type":"string"},"family_name":{"type":"string"},"email":{"type":"string"},"phone":{"type":"string"},"notes":{"type":"string"}}, ["given_name"]) },
            Tool { name: "gcon_update_contact".into(), description: "Update an existing Google contact.".into(),                                       input_schema: schema!({"name":{"type":"string"},"given_name":{"type":"string"},"family_name":{"type":"string"},"email":{"type":"string"},"phone":{"type":"string"},"notes":{"type":"string"}}, ["name"]) },
            Tool { name: "gcon_delete_contact".into(), description: "Delete a Google contact.".into(),                                                input_schema: schema!({"name":{"type":"string"}}, ["name"]) },
            Tool { name: "gcon_search_contacts".into(), description: "Search Google contacts by name, email, or phone.".into(),                       input_schema: schema!({"query":{"type":"string"},"max_results":{"type":"integer","default":10}}, ["query"]) },

            // Meet
            Tool { name: "gmeet_list_records".into(), description: "List past Google Meet conference records.".into(), input_schema: schema!({"max_results":{"type":"integer","default":10},"filter":{"type":"string"}}, []) },
            Tool { name: "gmeet_get_full_transcript".into(), description: "Get the full, chronological transcript text for a Meet call.".into(), input_schema: schema!({"conference_record_name":{"type":"string","description":"Format: conferenceRecords/XXXXXXXXXXXX"}}, ["conference_record_name"]) },

            // Tasks
            Tool { name: "gtasks_list_lists".into(), description: "List all Google Task lists.".into(), input_schema: schema!({"max_results":{"type":"integer","default":20}}, []) },
            Tool { name: "gtasks_list_tasks".into(), description: "List tasks in a specific task list.".into(), input_schema: schema!({"tasklist_id":{"type":"string"},"show_completed":{"type":"boolean","default":false}}, ["tasklist_id"]) },
            Tool { name: "gtasks_create_task".into(), description: "Create a new Google Task.".into(), input_schema: schema!({"tasklist_id":{"type":"string"},"title":{"type":"string"},"notes":{"type":"string"},"due":{"type":"string","description":"RFC 3339 timestamp"}}, ["tasklist_id","title"]) },
            Tool { name: "gtasks_complete_task".into(), description: "Mark a Google Task as completed.".into(), input_schema: schema!({"tasklist_id":{"type":"string"},"task_id":{"type":"string"}}, ["tasklist_id","task_id"]) },

            // Docs
            Tool { name: "gdocs_create".into(), description: "Create a new Google Doc.".into(), input_schema: schema!({"title":{"type":"string"}}, ["title"]) },
            Tool { name: "gdocs_get_text".into(), description: "Get the plain text content of a Google Doc.".into(), input_schema: schema!({"document_id":{"type":"string"}}, ["document_id"]) },
            Tool { name: "gdocs_append_text".into(), description: "Append text to the end of a Google Doc.".into(), input_schema: schema!({"document_id":{"type":"string"},"text":{"type":"string"}}, ["document_id","text"]) },

            // Sheets — Spreadsheet Management
            Tool { name: "gsheets_list".into(), description: "List Google Spreadsheets in the user's Drive. Returns id, name, modifiedTime for each.".into(), input_schema: schema!({"max_results":{"type":"integer","default":20}}, []) },
            Tool { name: "gsheets_create".into(), description: "Create a new Google Spreadsheet.".into(), input_schema: schema!({"title":{"type":"string"},"sheet_names":{"type":"array","items":{"type":"string"}}}, ["title"]) },
            Tool { name: "gsheets_get".into(), description: "Get spreadsheet metadata (title, sheet tabs, properties). Use to discover sheet IDs.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"}}, ["spreadsheet_id"]) },

            // Sheets — Reading & Writing
            Tool { name: "gsheets_read_range".into(), description: "Read cell values from a range (e.g. 'Sheet1!A1:D10').".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"range":{"type":"string"}}, ["spreadsheet_id","range"]) },
            Tool { name: "gsheets_batch_read".into(), description: "Read multiple ranges in a single request.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"ranges":{"type":"array","items":{"type":"string"}}}, ["spreadsheet_id","ranges"]) },
            Tool { name: "gsheets_write_range".into(), description: "Write/update cell values in a range. 'values' is a 2D array.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"range":{"type":"string"},"values":{"type":"array","items":{"type":"array"}}}, ["spreadsheet_id","range","values"]) },
            Tool { name: "gsheets_batch_write".into(), description: "Write to multiple ranges in one request. 'data' is an array of {range, values} objects.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"data":{"type":"array","items":{"type":"object","properties":{"range":{"type":"string"},"values":{"type":"array","items":{"type":"array"}}}}}}, ["spreadsheet_id","data"]) },
            Tool { name: "gsheets_append_rows".into(), description: "Append rows after the last row with data. 'values' is a 2D array.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"range":{"type":"string"},"values":{"type":"array","items":{"type":"array"}}}, ["spreadsheet_id","range","values"]) },
            Tool { name: "gsheets_clear_range".into(), description: "Clear all cell values in a range (keeps formatting).".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"range":{"type":"string"}}, ["spreadsheet_id","range"]) },
            Tool { name: "gsheets_find".into(), description: "Search for a value in a sheet range. Returns matching cell addresses and values.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"range":{"type":"string","description":"Range to search, e.g. 'Sheet1!A1:Z1000'"},"query":{"type":"string","description":"Text to search for (case-insensitive)"}}, ["spreadsheet_id","range","query"]) },

            // Sheets — Tab Management
            Tool { name: "gsheets_add_sheet".into(), description: "Add a new sheet tab to an existing spreadsheet.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"title":{"type":"string"}}, ["spreadsheet_id","title"]) },
            Tool { name: "gsheets_delete_sheet".into(), description: "Delete a sheet tab by its numeric sheet ID. Use gsheets_get to find IDs.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"}}, ["spreadsheet_id","sheet_id"]) },
            Tool { name: "gsheets_rename_sheet".into(), description: "Rename a sheet tab.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"new_title":{"type":"string"}}, ["spreadsheet_id","sheet_id","new_title"]) },
            Tool { name: "gsheets_duplicate_sheet".into(), description: "Duplicate a sheet tab within the same spreadsheet.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"new_title":{"type":"string"}}, ["spreadsheet_id","sheet_id"]) },
            Tool { name: "gsheets_copy_sheet_to".into(), description: "Copy a sheet tab to a different spreadsheet.".into(), input_schema: schema!({"source_spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"destination_spreadsheet_id":{"type":"string"}}, ["source_spreadsheet_id","sheet_id","destination_spreadsheet_id"]) },
            Tool { name: "gsheets_export_sheet".into(), description: "Export a specific sheet tab to PDF, XLSX, or CSV with print options.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"format":{"type":"string","enum":["pdf","xlsx","csv","tsv","ods","zip"],"default":"pdf"},"range":{"type":"string","description":"Optional cell range to export (e.g., 'A1:E20')"},"portrait":{"type":"boolean","default":true,"description":"True for portrait, false for landscape (PDF only)"},"fitw":{"type":"boolean","default":true,"description":"Fit to width (PDF only)"},"gridlines":{"type":"boolean","default":false,"description":"Show gridlines (PDF only)"}}, ["spreadsheet_id","sheet_id"]) },

            // Sheets — Row / Column Manipulation
            Tool { name: "gsheets_insert_dimension".into(), description: "Insert empty rows or columns. dimension: 'ROWS' or 'COLUMNS'. start_index is 0-based.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"dimension":{"type":"string","enum":["ROWS","COLUMNS"]},"start_index":{"type":"integer"},"count":{"type":"integer"}}, ["spreadsheet_id","sheet_id","dimension","start_index","count"]) },
            Tool { name: "gsheets_delete_dimension".into(), description: "Delete rows or columns. dimension: 'ROWS' or 'COLUMNS'. Indices are 0-based, end is exclusive.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"dimension":{"type":"string","enum":["ROWS","COLUMNS"]},"start_index":{"type":"integer"},"end_index":{"type":"integer"}}, ["spreadsheet_id","sheet_id","dimension","start_index","end_index"]) },

            // Sheets — Sort & Filter
            Tool { name: "gsheets_sort_range".into(), description: "Sort a range by a column. All indices are 0-based.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"start_row":{"type":"integer"},"end_row":{"type":"integer"},"start_col":{"type":"integer"},"end_col":{"type":"integer"},"sort_column":{"type":"integer","description":"0-based column index"},"ascending":{"type":"boolean","default":true}}, ["spreadsheet_id","sheet_id","start_row","end_row","start_col","end_col","sort_column"]) },
            Tool { name: "gsheets_create_filter".into(), description: "Add an auto-filter to a range. All indices are 0-based.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"start_row":{"type":"integer"},"end_row":{"type":"integer"},"start_col":{"type":"integer"},"end_col":{"type":"integer"}}, ["spreadsheet_id","sheet_id","start_row","end_row","start_col","end_col"]) },
            Tool { name: "gsheets_clear_filter".into(), description: "Remove the auto-filter from a sheet.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"}}, ["spreadsheet_id","sheet_id"]) },

            // Sheets — Merge / Unmerge
            Tool { name: "gsheets_merge_cells".into(), description: "Merge cells. merge_type: 'MERGE_ALL', 'MERGE_COLUMNS', or 'MERGE_ROWS'. Indices are 0-based.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"start_row":{"type":"integer"},"end_row":{"type":"integer"},"start_col":{"type":"integer"},"end_col":{"type":"integer"},"merge_type":{"type":"string","default":"MERGE_ALL"}}, ["spreadsheet_id","sheet_id","start_row","end_row","start_col","end_col"]) },
            Tool { name: "gsheets_unmerge_cells".into(), description: "Unmerge all merged cells in a range. Indices are 0-based.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"start_row":{"type":"integer"},"end_row":{"type":"integer"},"start_col":{"type":"integer"},"end_col":{"type":"integer"}}, ["spreadsheet_id","sheet_id","start_row","end_row","start_col","end_col"]) },

            // Sheets — Formatting
            Tool { name: "gsheets_bold_row".into(), description: "Make an entire row bold (e.g. for headers). row_index is 0-based.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"row_index":{"type":"integer"}}, ["spreadsheet_id","sheet_id","row_index"]) },
            Tool { name: "gsheets_freeze_rows".into(), description: "Freeze the first N rows of a sheet.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"row_count":{"type":"integer"}}, ["spreadsheet_id","sheet_id","row_count"]) },
            Tool { name: "gsheets_auto_resize".into(), description: "Auto-resize all columns in a sheet to fit content.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"}}, ["spreadsheet_id","sheet_id"]) },
            Tool { name: "gsheets_format_cells".into(), description: "Apply formatting to a cell range: bold, italic, font_size, bg_color (hex like '#FF0000'), fg_color (hex), h_align ('LEFT','CENTER','RIGHT'). Indices are 0-based.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"start_row":{"type":"integer"},"end_row":{"type":"integer"},"start_col":{"type":"integer"},"end_col":{"type":"integer"},"bold":{"type":"boolean"},"italic":{"type":"boolean"},"font_size":{"type":"integer"},"bg_color":{"type":"string","description":"Hex color like '#4285F4'"},"fg_color":{"type":"string","description":"Hex text color like '#FFFFFF'"},"h_align":{"type":"string","enum":["LEFT","CENTER","RIGHT"]}}, ["spreadsheet_id","sheet_id","start_row","end_row","start_col","end_col"]) },
            Tool { name: "gsheets_add_conditional_format".into(), description: "Add conditional formatting with custom formula. bg_color is hex like '#FF0000'. Indices are 0-based.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"start_row":{"type":"integer"},"end_row":{"type":"integer"},"start_col":{"type":"integer"},"end_col":{"type":"integer"},"formula":{"type":"string","description":"Custom formula, e.g. '=A1>100'"},"bg_color":{"type":"string","description":"Hex color like '#FF0000'"}}, ["spreadsheet_id","sheet_id","start_row","end_row","start_col","end_col","formula","bg_color"]) },
            Tool { name: "gsheets_clear_conditional_formats".into(), description: "Remove all conditional formatting rules from a sheet.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"}}, ["spreadsheet_id","sheet_id"]) },
            Tool { name: "gsheets_batch_update".into(), description: "Send arbitrary batchUpdate requests to a spreadsheet. 'requests' is a JSON array of request objects.".into(), input_schema: schema!({"spreadsheet_id":{"type":"string"},"requests":{"type":"array","items":{"type":"object"}}}, ["spreadsheet_id","requests"]) },

            // Slides
            Tool { name: "gslides_create".into(), description: "Create a new Google Slides presentation.".into(), input_schema: schema!({"title":{"type":"string"}}, ["title"]) },
            Tool { name: "gslides_replace_text".into(), description: "Replace all occurrences of text in a presentation.".into(), input_schema: schema!({"presentation_id":{"type":"string"},"find":{"type":"string"},"replacement":{"type":"string"}}, ["presentation_id","find","replacement"]) },

            // Chat
            Tool { name: "gchat_list_spaces".into(), description: "List Google Chat spaces.".into(), input_schema: schema!({"max_results":{"type":"integer","default":20}}, []) },
            Tool { name: "gchat_send_message".into(), description: "Send a message to a Google Chat space.".into(), input_schema: schema!({"space_name":{"type":"string","description":"Format: spaces/XXXXXX"},"text":{"type":"string"}}, ["space_name","text"]) },
        ];

        // Dynamic catalogues for YouTube Data API v3 and Places API.
        tools.extend(youtube::tool_list());
        tools.extend(places::tool_list());
        tools
    }

    // ── Dispatcher ────────────────────────────────────────────────────────────

    pub async fn call(&self, name: &str, args: Map<String, Value>) -> Result<CallToolResult> {
        let a = &args;
        let s = str!(a);
        let n = num!(a);

        if let Some(v) = youtube::try_call(&self.0, name, a).await? {
            return Ok(ok_json(v));
        }
        if let Some(v) = places::try_call(&self.0, name, a).await? {
            return Ok(ok_json(v));
        }

        let result: Result<Value> = match name {
            // Auth
            "google_auth_url" => auth::auth_url(&self.0).await,
            "google_exchange_code" => auth::exchange_code(&self.0, s("code")?).await,
            "google_auth_status" => auth::auth_status(&self.0).await,
            "google_revoke" => auth::revoke(&self.0).await,

            // Gmail
            "gmail_list" => {
                gmail::list(
                    &self.0,
                    n("max_results", 10.0).min(10.0) as u32,
                    a.get("query").and_then(|v| v.as_str()),
                )
                .await
            }
            "gmail_get" => gmail::get(&self.0, s("id")?).await,
            "gmail_send" => {
                gmail::send(
                    &self.0,
                    s("to")?,
                    s("subject")?,
                    s("body")?,
                    a.get("cc").and_then(|v| v.as_str()),
                    a.get("bcc").and_then(|v| v.as_str()),
                )
                .await
            }
            "gmail_reply" => {
                gmail::reply(
                    &self.0,
                    s("thread_id")?,
                    s("message_id")?,
                    s("to")?,
                    s("subject")?,
                    s("body")?,
                )
                .await
            }
            "gmail_search" => {
                gmail::search(
                    &self.0,
                    s("query")?,
                    n("max_results", 10.0).min(10.0) as u32,
                )
                .await
            }
            "gmail_trash" => gmail::trash(&self.0, s("id")?).await,
            "gmail_mark_read" => gmail::mark_read(&self.0, json_arr(a, "ids")?).await,
            "gmail_add_label" => gmail::add_label(&self.0, s("id")?, s("label_id")?).await,
            "gmail_remove_label" => gmail::remove_label(&self.0, s("id")?, s("label_id")?).await,
            "gmail_download_attachment" => {
                gmail::download_attachment(
                    &self.0,
                    s("message_id")?,
                    s("attachment_id")?,
                    s("filename")?,
                )
                .await
            }
            "gmail_list_labels" => gmail::list_labels(&self.0).await,
            "gmail_mark_unread" => gmail::mark_unread(&self.0, json_arr(a, "ids")?).await,
            "gmail_untrash" => gmail::untrash(&self.0, s("id")?).await,
            "gmail_delete" => gmail::delete(&self.0, s("id")?).await,
            "gmail_forward" => {
                gmail::forward(
                    &self.0,
                    s("message_id")?,
                    s("to")?,
                    a.get("extra_note").and_then(|v| v.as_str()),
                )
                .await
            }
            "gmail_create_draft" => {
                gmail::create_draft(
                    &self.0,
                    s("to")?,
                    s("subject")?,
                    s("body")?,
                    a.get("cc").and_then(|v| v.as_str()),
                    a.get("bcc").and_then(|v| v.as_str()),
                )
                .await
            }
            "gmail_list_drafts" => {
                gmail::list_drafts(&self.0, n("max_results", 10.0).min(10.0) as u32).await
            }
            "gmail_get_draft" => gmail::get_draft(&self.0, s("draft_id")?).await,
            "gmail_update_draft" => {
                gmail::update_draft(
                    &self.0,
                    s("draft_id")?,
                    s("to")?,
                    s("subject")?,
                    s("body")?,
                    a.get("cc").and_then(|v| v.as_str()),
                    a.get("bcc").and_then(|v| v.as_str()),
                )
                .await
            }
            "gmail_send_draft" => gmail::send_draft(&self.0, s("draft_id")?).await,
            "gmail_delete_draft" => gmail::delete_draft(&self.0, s("draft_id")?).await,

            // Calendar
            "gcal_list_calendars" => calendar::list_calendars(&self.0).await,
            "gcal_list_events" => {
                calendar::list_events(
                    &self.0,
                    n("max_results", 10.0).min(10.0) as u32,
                    a.get("time_min").and_then(|v| v.as_str()),
                    a.get("time_max").and_then(|v| v.as_str()),
                    a.get("query").and_then(|v| v.as_str()),
                    a.get("calendar_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("primary"),
                    a.get("single_events").and_then(|v| v.as_bool()),
                )
                .await
            }
            "gcal_create_event" => {
                calendar::create_event(
                    &self.0,
                    s("summary")?,
                    s("start")?,
                    s("end")?,
                    a.get("description").and_then(|v| v.as_str()),
                    a.get("location").and_then(|v| v.as_str()),
                    extract_attendees(a, "attendees"),
                    a.get("time_zone").and_then(|v| v.as_str()),
                    a.get("create_meet_link")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    a.get("calendar_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("primary"),
                    json_arr_opt(a, "recurrence"),
                )
                .await
            }
            "gcal_get_event" => {
                calendar::get_event(
                    &self.0,
                    s("event_id")?,
                    a.get("calendar_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("primary"),
                )
                .await
            }
            "gcal_update_event" => {
                calendar::update_event(
                    &self.0,
                    s("event_id")?,
                    a.get("summary").and_then(|v| v.as_str()),
                    a.get("start").and_then(|v| v.as_str()),
                    a.get("end").and_then(|v| v.as_str()),
                    a.get("description").and_then(|v| v.as_str()),
                    a.get("location").and_then(|v| v.as_str()),
                    a.get("time_zone").and_then(|v| v.as_str()),
                    a.get("calendar_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("primary"),
                    extract_attendees(a, "attendees"),
                    json_arr_opt(a, "recurrence"),
                )
                .await
            }
            "gcal_delete_event" => {
                calendar::delete_event(
                    &self.0,
                    s("event_id")?,
                    a.get("calendar_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("primary"),
                    a.get("all_events")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                )
                .await
            }
            "gcal_move_event" => {
                calendar::move_event(
                    &self.0,
                    s("event_id")?,
                    a.get("source_calendar_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("primary"),
                    s("destination_calendar_id")?,
                )
                .await
            }
            "gcal_quick_add" => {
                calendar::quick_add(
                    &self.0,
                    s("text")?,
                    a.get("calendar_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("primary"),
                )
                .await
            }
            "gcal_get_freebusy" => {
                calendar::get_freebusy(
                    &self.0,
                    json_arr(a, "calendar_ids")?,
                    s("time_min")?,
                    s("time_max")?,
                )
                .await
            }

            // Contacts
            "gcon_list_contacts" => {
                contacts::list_contacts(&self.0, n("max_results", 50.0) as u32).await
            }
            "gcon_get_contact" => contacts::get_contact(&self.0, s("name")?).await,
            "gcon_create_contact" => {
                contacts::create_contact(
                    &self.0,
                    s("given_name")?,
                    a.get("family_name").and_then(|v| v.as_str()),
                    a.get("email").and_then(|v| v.as_str()),
                    a.get("phone").and_then(|v| v.as_str()),
                    a.get("notes").and_then(|v| v.as_str()),
                )
                .await
            }
            "gcon_update_contact" => {
                contacts::update_contact(
                    &self.0,
                    s("name")?,
                    a.get("given_name").and_then(|v| v.as_str()),
                    a.get("family_name").and_then(|v| v.as_str()),
                    a.get("email").and_then(|v| v.as_str()),
                    a.get("phone").and_then(|v| v.as_str()),
                    a.get("notes").and_then(|v| v.as_str()),
                )
                .await
            }
            "gcon_delete_contact" => contacts::delete_contact(&self.0, s("name")?).await,
            "gcon_search_contacts" => {
                contacts::search_contacts(&self.0, s("query")?, n("max_results", 10.0) as u32).await
            }

            // Drive
            "gdrive_list" => {
                drive::list(
                    &self.0,
                    n("max_results", 10.0).min(10.0) as u32,
                    a.get("folder_id").and_then(|v| v.as_str()),
                    a.get("mime_type").and_then(|v| v.as_str()),
                )
                .await
            }
            "gdrive_search" => {
                drive::search(
                    &self.0,
                    s("query")?,
                    n("max_results", 10.0).min(10.0) as u32,
                )
                .await
            }
            "gdrive_move_file" => {
                drive::move_file(&self.0, s("file_id")?, s("new_folder_id")?).await
            }
            "gdrive_share" => {
                drive::share(
                    &self.0,
                    s("file_id")?,
                    a.get("role").and_then(|v| v.as_str()).unwrap_or("reader"),
                    a.get("type").and_then(|v| v.as_str()).unwrap_or("anyone"),
                    a.get("email").and_then(|v| v.as_str()),
                )
                .await
            }
            "gdrive_export" => drive::export(&self.0, s("file_id")?, s("mime_type")?).await,
            "gdrive_download_binary" => drive::download_binary(&self.0, s("file_id")?).await,
            "gdrive_upload_binary" => {
                drive::upload_binary(
                    &self.0,
                    s("local_path")?,
                    s("name")?,
                    a.get("mime_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("application/octet-stream"),
                    a.get("folder_id").and_then(|v| v.as_str()),
                )
                .await
            }
            "gdrive_upload_folder" => {
                drive::upload_folder(
                    &self.0,
                    s("local_folder_path")?,
                    a.get("folder_name").and_then(|v| v.as_str()),
                    a.get("parent_folder_id").and_then(|v| v.as_str()),
                    a.get("include_hidden")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                )
                .await
            }
            "gdrive_delete" => {
                let bulk_ids = a
                    .get("file_ids")
                    .and_then(|v| v.as_array())
                    .filter(|ids| !ids.is_empty())
                    .or_else(|| {
                        a.get("file_id")
                            .and_then(|v| v.as_array())
                            .filter(|ids| !ids.is_empty())
                    });

                if let Some(ids) = bulk_ids {
                    let mut deleted = Vec::new();
                    let mut errors = Vec::new();

                    for (index, raw_id) in ids.iter().enumerate() {
                        let id_opt = raw_id.as_str().map(str::trim).filter(|id| !id.is_empty());
                        let id = match id_opt {
                            Some(id) => id,
                            None => {
                                errors.push(json!({
                                    "index": index,
                                    "file_id": raw_id,
                                    "error": "Each file_id must be a non-empty string"
                                }));
                                continue;
                            }
                        };

                        match drive::delete(&self.0, id).await {
                            Ok(v) => deleted.push(v),
                            Err(e) => errors.push(json!({
                                "index": index,
                                "file_id": id,
                                "error": e.to_string()
                            })),
                        }
                    }

                    Ok(json!({
                        "success": errors.is_empty(),
                        "deleted_count": deleted.len(),
                        "error_count": errors.len(),
                        "deleted": deleted,
                        "errors": errors
                    }))
                } else {
                    drive::delete(&self.0, s("file_id")?).await
                }
            }
            "gmail_send_with_attachment" => {
                gmail::send_with_attachment(
                    &self.0,
                    s("to")?,
                    s("subject")?,
                    s("body")?,
                    s("local_path")?,
                    a.get("cc").and_then(|v| v.as_str()),
                    a.get("bcc").and_then(|v| v.as_str()),
                )
                .await
            }

            // Meet
            "gmeet_list_records" => {
                meet::list_conference_records(
                    &self.0,
                    n("max_results", 10.0) as u32,
                    a.get("filter").and_then(|v| v.as_str()),
                )
                .await
            }
            "gmeet_get_full_transcript" => {
                meet::get_full_transcript_text(&self.0, s("conference_record_name")?).await
            }

            // Tasks
            "gtasks_list_lists" => {
                tasks::list_task_lists(&self.0, n("max_results", 20.0) as u32).await
            }
            "gtasks_list_tasks" => {
                tasks::list_tasks(
                    &self.0,
                    s("tasklist_id")?,
                    50,
                    a.get("show_completed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    None,
                    None,
                )
                .await
            }
            "gtasks_create_task" => {
                tasks::create_task(
                    &self.0,
                    s("tasklist_id")?,
                    s("title")?,
                    a.get("notes").and_then(|v| v.as_str()),
                    a.get("due").and_then(|v| v.as_str()),
                    None,
                )
                .await
            }
            "gtasks_complete_task" => {
                tasks::complete_task(&self.0, s("tasklist_id")?, s("task_id")?).await
            }

            // Docs
            "gdocs_create" => docs::create_document(&self.0, s("title")?).await,
            "gdocs_get_text" => docs::get_text(&self.0, s("document_id")?).await,
            "gdocs_append_text" => docs::append_text(&self.0, s("document_id")?, s("text")?).await,

            // Sheets
            "gsheets_list" => {
                sheets::list_spreadsheets(&self.0, n("max_results", 20.0) as u32).await
            }
            "gsheets_create" => {
                sheets::create_spreadsheet(&self.0, s("title")?, json_arr_opt(a, "sheet_names"))
                    .await
            }
            "gsheets_get" => sheets::get_spreadsheet(&self.0, s("spreadsheet_id")?).await,
            "gsheets_read_range" => {
                sheets::read_range(&self.0, s("spreadsheet_id")?, s("range")?).await
            }
            "gsheets_batch_read" => {
                sheets::batch_read(&self.0, s("spreadsheet_id")?, json_arr(a, "ranges")?).await
            }
            "gsheets_write_range" => {
                let values = parse_2d_values(a);
                sheets::write_range(&self.0, s("spreadsheet_id")?, s("range")?, values).await
            }
            "gsheets_batch_write" => {
                let data_v = a.get("data");
                let data_arr = data_v
                    .and_then(|v| {
                        if let Some(obj) = v.as_object() {
                            if let Some(params) = obj.get("parameters") {
                                return params.as_array();
                            }
                        }
                        v.as_array()
                    })
                    .cloned()
                    .unwrap_or_default();
                let data: Vec<(String, Vec<Vec<Value>>)> = data_arr
                    .into_iter()
                    .filter_map(|entry| {
                        let obj = entry.as_object()?;
                        let range = obj.get("range")?.as_str()?.to_string();
                        // re-use parse_2d_values logic inside batch parsing
                        let values = parse_2d_values(obj);
                        Some((range, values))
                    })
                    .collect();
                sheets::batch_write(&self.0, s("spreadsheet_id")?, data).await
            }
            "gsheets_append_rows" => {
                let values = parse_2d_values(a);
                sheets::append_rows(&self.0, s("spreadsheet_id")?, s("range")?, values).await
            }
            "gsheets_clear_range" => {
                sheets::clear_range(&self.0, s("spreadsheet_id")?, s("range")?).await
            }
            "gsheets_find" => {
                sheets::find_in_sheet(&self.0, s("spreadsheet_id")?, s("range")?, s("query")?).await
            }
            "gsheets_add_sheet" => {
                sheets::add_sheet(&self.0, s("spreadsheet_id")?, s("title")?).await
            }
            "gsheets_delete_sheet" => {
                sheets::delete_sheet(&self.0, s("spreadsheet_id")?, n("sheet_id", 0.0) as u64).await
            }
            "gsheets_rename_sheet" => {
                sheets::rename_sheet(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    s("new_title")?,
                )
                .await
            }
            "gsheets_duplicate_sheet" => {
                sheets::duplicate_sheet(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    a.get("new_title").and_then(|v| v.as_str()),
                )
                .await
            }
            "gsheets_copy_sheet_to" => {
                sheets::copy_sheet_to(
                    &self.0,
                    s("source_spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    s("destination_spreadsheet_id")?,
                )
                .await
            }
            "gsheets_export_sheet" => {
                sheets::export_sheet(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    a.get("format").and_then(|v| v.as_str()).unwrap_or("pdf"),
                    a.get("range").and_then(|v| v.as_str()),
                    a.get("portrait").and_then(|v| v.as_bool()),
                    a.get("fitw").and_then(|v| v.as_bool()),
                    a.get("gridlines").and_then(|v| v.as_bool()),
                )
                .await
            }
            "gsheets_insert_dimension" => {
                sheets::insert_dimension(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    s("dimension")?,
                    n("start_index", 0.0) as u32,
                    n("count", 1.0) as u32,
                )
                .await
            }
            "gsheets_delete_dimension" => {
                sheets::delete_dimension(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    s("dimension")?,
                    n("start_index", 0.0) as u32,
                    n("end_index", 1.0) as u32,
                )
                .await
            }
            "gsheets_sort_range" => {
                sheets::sort_range(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    n("start_row", 0.0) as u32,
                    n("end_row", 0.0) as u32,
                    n("start_col", 0.0) as u32,
                    n("end_col", 0.0) as u32,
                    n("sort_column", 0.0) as u32,
                    a.get("ascending").and_then(|v| v.as_bool()).unwrap_or(true),
                )
                .await
            }
            "gsheets_create_filter" => {
                sheets::create_filter(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    n("start_row", 0.0) as u32,
                    n("end_row", 0.0) as u32,
                    n("start_col", 0.0) as u32,
                    n("end_col", 0.0) as u32,
                )
                .await
            }
            "gsheets_clear_filter" => {
                sheets::clear_filter(&self.0, s("spreadsheet_id")?, n("sheet_id", 0.0) as u64).await
            }
            "gsheets_merge_cells" => {
                sheets::merge_cells(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    n("start_row", 0.0) as u32,
                    n("end_row", 0.0) as u32,
                    n("start_col", 0.0) as u32,
                    n("end_col", 0.0) as u32,
                    a.get("merge_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("MERGE_ALL"),
                )
                .await
            }
            "gsheets_unmerge_cells" => {
                sheets::unmerge_cells(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    n("start_row", 0.0) as u32,
                    n("end_row", 0.0) as u32,
                    n("start_col", 0.0) as u32,
                    n("end_col", 0.0) as u32,
                )
                .await
            }
            "gsheets_bold_row" => {
                sheets::bold_row(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    n("row_index", 0.0) as u32,
                )
                .await
            }
            "gsheets_freeze_rows" => {
                sheets::freeze_rows(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    n("row_count", 1.0) as u32,
                )
                .await
            }
            "gsheets_auto_resize" => {
                sheets::auto_resize_columns(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                )
                .await
            }
            "gsheets_format_cells" => {
                sheets::format_cells(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    n("start_row", 0.0) as u32,
                    n("end_row", 0.0) as u32,
                    n("start_col", 0.0) as u32,
                    n("end_col", 0.0) as u32,
                    a.get("bold").and_then(|v| v.as_bool()),
                    a.get("italic").and_then(|v| v.as_bool()),
                    a.get("font_size")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u32),
                    a.get("bg_color")
                        .and_then(|v| v.as_str())
                        .and_then(parse_hex_color),
                    a.get("fg_color")
                        .and_then(|v| v.as_str())
                        .and_then(parse_hex_color),
                    a.get("h_align").and_then(|v| v.as_str()),
                )
                .await
            }
            "gsheets_add_conditional_format" => {
                let color = a
                    .get("bg_color")
                    .and_then(|v| v.as_str())
                    .and_then(parse_hex_color)
                    .unwrap_or((1.0, 0.0, 0.0));
                sheets::add_conditional_format(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                    n("start_row", 0.0) as u32,
                    n("end_row", 0.0) as u32,
                    n("start_col", 0.0) as u32,
                    n("end_col", 0.0) as u32,
                    s("formula")?,
                    color,
                )
                .await
            }
            "gsheets_clear_conditional_formats" => {
                sheets::clear_conditional_formats(
                    &self.0,
                    s("spreadsheet_id")?,
                    n("sheet_id", 0.0) as u64,
                )
                .await
            }
            "gsheets_batch_update" => {
                let requests = a
                    .get("requests")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                sheets::batch_update(&self.0, s("spreadsheet_id")?, requests).await
            }

            // Slides
            "gslides_create" => slides::create_presentation(&self.0, s("title")?).await,
            "gslides_replace_text" => {
                slides::replace_text(
                    &self.0,
                    s("presentation_id")?,
                    s("find")?,
                    s("replacement")?,
                    false,
                )
                .await
            }

            // Chat
            "gchat_list_spaces" => chat::list_spaces(&self.0, n("max_results", 20.0) as u32).await,
            "gchat_send_message" => {
                chat::send_message(&self.0, s("space_name")?, s("text")?, None).await
            }

            other => Err(anyhow::anyhow!("Unknown Google tool: {other}")),
        };

        Ok(match result {
            Ok(v) => ok_json(v),
            Err(e) => err_json(e),
        })
    }
}

// ── Small arg-extraction helpers ──────────────────────────────────────────────

fn json_arr<'a>(args: &'a Map<String, Value>, key: &str) -> Result<Vec<&'a str>> {
    args.get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing required param '{key}'"))?
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| anyhow::anyhow!("non-string in '{key}' array"))
        })
        .collect()
}

fn json_arr_opt<'a>(args: &'a Map<String, Value>, key: &str) -> Option<Vec<&'a str>> {
    args.get(key)?
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
}

/// Extract attendee emails from either:
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

    // Determine whether items are plain strings or objects
    let result: Vec<&str> = arr
        .iter()
        .filter_map(|item| {
            if let Some(s) = item.as_str() {
                // Plain string format
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            } else if let Some(obj) = item.as_object() {
                // Object format: { "email": "..." }
                obj.get("email")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            } else {
                None
            }
        })
        .collect();

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn parse_2d_values(a: &Map<String, Value>) -> Vec<Vec<Value>> {
    let v = match a.get("values") {
        Some(val) => val.clone(),
        None => return vec![],
    };

    // If it's a string, try parsing it as JSON first.
    let parsed_val = if let Some(s) = v.as_str() {
        serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
    } else {
        v
    };

    match parsed_val {
        // Direct 2D array: [[...]]
        Value::Array(arr) if arr.first().map_or(false, |first| first.is_array()) => arr
            .into_iter()
            .filter_map(|r| {
                if let Value::Array(row) = r {
                    Some(row)
                } else {
                    None
                }
            })
            .collect(),
        // 1D Array: [...] -> [[...]]
        Value::Array(arr) => vec![arr],
        // Single value not an array -> [[value]]
        other => vec![vec![other]],
    }
}

/// Parse a hex color string like "#FF0000" or "4285F4" into (r, g, b) floats 0.0–1.0.
fn parse_hex_color(hex: &str) -> Option<(f64, f64, f64)> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()? as f64 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()? as f64 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()? as f64 / 255.0;
    Some((r, g, b))
}
