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
            $args
                .get(key)
                .and_then(|v| {
                    // Expressions and workflow serializers often deliver
                    // numbers as strings ("50"); accept those too.
                    v.as_f64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                })
                .unwrap_or(default)
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
            Tool::new("google_auth_url", "Get the Google OAuth2 URL. Open it in a browser to sign in.", schema!({}, [])),
            Tool::new("google_exchange_code", "Exchange the Google OAuth code for tokens after signing in.", schema!({"code":{"type":"string","description":"The code param from the redirect URL"}}, ["code"])),
            Tool::new("google_auth_status", "Check Google authentication status.", schema!({}, [])),
            Tool::new("google_revoke", "Revoke and delete stored Google tokens.", schema!({}, [])),

            // Gmail
            Tool::new("gmail_list", "List Gmail messages. Returns id, subject, from, date, snippet. Use this when asked to check email, the inbox, or unread mail (query 'is:unread').", schema!({"max_results":{"type":"integer","default":10,"description":"Max messages (max 10)"},"query":{"type":"string","description":"Gmail search query, e.g. 'is:unread from:boss@co.com'"}}, [])),
            Tool::new("gmail_get", "Get a full Gmail message: decoded body split into main text / signature / quoted reply, parsed sender, links, contacts, and attachment metadata.", schema!({"id":{"type":"string","description":"Message ID"}}, ["id"])),
            Tool::new("gmail_send", "Send a Gmail email, optionally with a file attachment.", schema!({"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"},"cc":{"type":"string"},"bcc":{"type":"string"},"send_attachment":{"type":"boolean","default":false,"title":"Send Attachment","description":"Toggle on to attach a file from a local/server path"},"attachment_path":{"type":"string","title":"Attachment Path","description":"Local/server file path to attach (e.g. /data/files/quote.pdf)","displayOptions":{"show":{"send_attachment":[true]}}}}, ["to","subject","body"])),
            Tool::new("gmail_reply", "Reply to a Gmail message thread, optionally with a file attachment. Pass the original message_id (Gmail id or RFC Message-ID); thread_id/subject are derived when omitted.", schema!({"message_id":{"type":"string"},"to":{"type":"string"},"body":{"type":"string"},"subject":{"type":"string"},"thread_id":{"type":"string"},"send_attachment":{"type":"boolean","default":false,"title":"Send Attachment","description":"Toggle on to attach a file from a local/server path"},"attachment_path":{"type":"string","title":"Attachment Path","description":"Local/server file path to attach","displayOptions":{"show":{"send_attachment":[true]}}}}, ["message_id","to","body"])),
            Tool::new("gmail_search", "Search Gmail messages by query string. Limited to 10 results.", schema!({"query":{"type":"string"},"max_results":{"type":"integer","default":10}}, ["query"])),
            Tool::new("gmail_trash", "Move a Gmail message to trash.", schema!({"id":{"type":"string"}}, ["id"])),
            Tool::new("gmail_mark_read", "Mark Gmail messages as read.", schema!({"ids":{"type":"array","items":{"type":"string"}}}, ["ids"])),
            Tool::new("gmail_add_label", "Add a label to a Gmail message.", schema!({"id":{"type":"string"},"label_id":{"type":"string"}}, ["id","label_id"])),
            Tool::new("gmail_remove_label", "Remove a label from a Gmail message.", schema!({"id":{"type":"string"},"label_id":{"type":"string"}}, ["id","label_id"])),
            Tool::new("gmail_download_attachment", "Download a Gmail attachment to a local file path so the agent can upload/send it.", schema!({"message_id":{"type":"string"},"attachment_id":{"type":"string"},"filename":{"type":"string"}}, ["message_id","attachment_id","filename"])),
            Tool::new("gmail_download_all_attachments", "Download every attachment on a Gmail message to local file paths, returning each with size, kind (image/document/other) and inline flag.", schema!({"message_id":{"type":"string"}}, ["message_id"])),
            Tool::new("gmail_list_labels", "List all Gmail labels.", schema!({}, [])),
            Tool::new("gmail_mark_unread", "Mark Gmail messages as unread.", schema!({"ids":{"type":"array","items":{"type":"string"}}}, ["ids"])),
            Tool::new("gmail_untrash", "Restore a Gmail message from trash.", schema!({"id":{"type":"string"}}, ["id"])),
            Tool::new("gmail_delete", "Permanently delete a Gmail message. This is irreversible.", schema!({"id":{"type":"string"}}, ["id"])),
            Tool::new("gmail_forward", "Forward a Gmail message to another recipient.", schema!({"message_id":{"type":"string"},"to":{"type":"string"},"extra_note":{"type":"string","description":"Optional note to prepend"}}, ["message_id","to"])),
            Tool::new("gmail_create_draft", "Create a Gmail draft.", schema!({"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"},"cc":{"type":"string"},"bcc":{"type":"string"}}, ["to","subject","body"])),
            Tool::new("gmail_list_drafts", "List Gmail drafts.", schema!({"max_results":{"type":"integer","default":10}}, [])),
            Tool::new("gmail_get_draft", "Get a specific Gmail draft by ID.", schema!({"draft_id":{"type":"string"}}, ["draft_id"])),
            Tool::new("gmail_update_draft", "Update a Gmail draft's content.", schema!({"draft_id":{"type":"string"},"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"},"cc":{"type":"string"},"bcc":{"type":"string"}}, ["draft_id","to","subject","body"])),
            Tool::new("gmail_send_draft", "Send an existing Gmail draft.", schema!({"draft_id":{"type":"string"}}, ["draft_id"])),
            Tool::new("gmail_delete_draft", "Delete a Gmail draft.", schema!({"draft_id":{"type":"string"}}, ["draft_id"])),


            // Calendar
            // Calendar
            Tool::new("gcal_list_calendars", "List all Google calendars in the user's account. Use this to discover calendar IDs.", schema!({}, [])),
            Tool::new("gcal_list_events", "List Google Calendar events. Use this when asked about the calendar, schedule, agenda, or upcoming events. Supports free-text search via 'query' parameter. Set 'single_events' to false to discover master events of recurring series (required for series deletion). When 'single_events' is true and no 'time_min' is given, listing starts from now. Responses with more events than 'max_results' include a nextPageToken — pass it back as 'page_token' for the next page.", schema!({"max_results":{"type":"integer","default":10,"maximum":2500},"time_min":{"type":"string","description":"Window start. Any common datetime format works: ISO 8601, '2026-07-05 09:00', 'July 5, 2026 3pm', or a Unix timestamp.","displayOptions":{"inlineGroup":"time_window"}},"time_max":{"type":"string","description":"Window end. Accepts the same flexible formats as time_min.","displayOptions":{"inlineGroup":"time_window"}},"query":{"type":"string","description":"Free-text search terms"},"calendar_id":{"type":"string","default":"primary"},"single_events":{"type":"boolean","default":true},"page_token":{"type":"string","description":"nextPageToken from a previous response, to fetch the next page"}}, [])),
            Tool::new("gcal_get_event", "Get a single Google Calendar event by ID.", schema!({"event_id":{"type":"string"},"calendar_id":{"type":"string","default":"primary"}}, ["event_id"])),
            Tool::new("gcal_create_event", "Create a Google Calendar event. Defaults to 'Asia/Manila' timezone. Set 'create_meet_link' to true to generate a Google Meet link. For recurring events, provide RRULE strings in the 'recurrence' array (e.g. ['RRULE:FREQ=WEEKLY;BYDAY=FR']). For an ALL-DAY event pass dates only (e.g. start '2025-06-15', end '2025-06-15').", schema!({
                    "summary":         { "type": "string",  "description": "Event title / name (SUMMARY). What the event is called, e.g. 'Team Standup' or 'Doctor Appointment'." },
                    "start":           { "type": "string",  "description": "Start date and time, e.g. '2025-06-15T09:00:00'. Any common format works: ISO 8601, '2025-06-15 09:00', 'June 15, 2025 9am', '06/15/2025 9:00 AM', or a Unix timestamp. A date alone ('2025-06-15') makes an all-day event.", "displayOptions": { "inlineGroup": "event_time" } },
                    "end":             { "type": "string",  "description": "End date and time, e.g. '2025-06-15T10:00:00'. Accepts the same flexible formats as start. Must be after start. For all-day events use a date; same date as start means a one-day event.", "displayOptions": { "inlineGroup": "event_time" } },
                    "description":     { "type": "string",  "description": "Optional notes or agenda for the event (DESCRIPTION). Supports plain text details about what this event is about." },
                    "location":        { "type": "string",  "description": "Physical or virtual place where the event occurs (LOCATION), e.g. 'Zoom', 'Conference Room A', or a full address." },
                    "attendees":       { "type": "array",   "description": "List of people to invite to this event. Each item is an attendee with their email address.", "items": { "type": "object", "properties": { "email": { "type": "string", "description": "Attendee email address, e.g. john@example.com" } } } },
                    "time_zone":       { "type": "string",  "description": "Timezone for the event times, e.g. 'Asia/Manila', 'America/New_York'.", "default": "Asia/Manila",
                        "enum": ["Asia/Manila","Asia/Singapore","Asia/Tokyo","Asia/Hong_Kong","Asia/Seoul","Asia/Bangkok","Asia/Kolkata","Asia/Dubai","Asia/Karachi","Asia/Jakarta","Asia/Shanghai","Australia/Sydney","Australia/Melbourne","Europe/London","Europe/Paris","Europe/Berlin","Europe/Rome","Europe/Madrid","Europe/Amsterdam","Europe/Moscow","America/New_York","America/Chicago","America/Denver","America/Los_Angeles","America/Toronto","America/Vancouver","America/Sao_Paulo","America/Buenos_Aires","America/Mexico_City","America/Bogota","Africa/Cairo","Africa/Lagos","Africa/Nairobi","Pacific/Auckland","Pacific/Honolulu","UTC"]
                    },
                    "create_meet_link": { "type": "boolean", "description": "Set to true to automatically generate a Google Meet video conference link for this event.", "default": false },
                    "calendar_id":     { "type": "string",  "description": "Which calendar to add this event to. Use 'primary' for your main calendar, or select from your available calendars.", "default": "primary" },
                    "recurrence":      { "type": "array",   "description": "Recurrence rules as RFC 5545 RRULE strings, e.g. 'RRULE:FREQ=WEEKLY;BYDAY=FR' (every Friday), 'RRULE:FREQ=WEEKLY;BYDAY=FR;COUNT=10' (10 occurrences), 'RRULE:FREQ=WEEKLY;BYDAY=FR;UNTIL=20261231T000000Z' (until a date), 'RRULE:FREQ=MONTHLY;BYMONTHDAY=1' (1st of each month).", "items": { "type": "string" } },
                    "send_updates":    { "type": "string",  "description": "Who receives notification emails: 'all' attendees, 'externalOnly' (only attendees outside your Google Workspace), or 'none'.", "enum": ["all","externalOnly","none"], "default": "all" }
                }, ["summary","start","end"])),
            Tool::new("gcal_update_event", "Update a Google Calendar event. Only the provided fields change — blank fields are left untouched. Defaults to 'Asia/Manila' timezone. To edit an entire recurring series, provide the master ID (found via 'gcal_list_events' with single_events=false). You can also update the 'recurrence' rules for a series.", schema!({
                    "event_id":    { "type": "string", "description": "ID of the event to update." },
                    "summary":     { "type": "string", "description": "New event title / name (SUMMARY)." },
                    "start":       { "type": "string", "description": "New start time, e.g. '2025-06-15T09:00:00'. Any common datetime format or a Unix timestamp works. A date alone ('2025-06-15') switches the event to all-day." },
                    "end":         { "type": "string", "description": "New end time, e.g. '2025-06-15T10:00:00'. Accepts the same flexible formats as start. For all-day events use a date." },
                    "description": { "type": "string", "description": "New event notes / agenda (DESCRIPTION)." },
                    "location":    { "type": "string", "description": "New event location (LOCATION)." },
                    "attendees":   { "type": "array",  "description": "Updated attendee list. Each item is an attendee with their email.", "items": { "type": "object", "properties": { "email": { "type": "string", "description": "Attendee email address" } } } },
                    "time_zone":   { "type": "string", "description": "Timezone for the updated event times.", "default": "Asia/Manila",
                        "enum": ["Asia/Manila","Asia/Singapore","Asia/Tokyo","Asia/Hong_Kong","Asia/Seoul","Asia/Bangkok","Asia/Kolkata","Asia/Dubai","Asia/Karachi","Asia/Jakarta","Asia/Shanghai","Australia/Sydney","Australia/Melbourne","Europe/London","Europe/Paris","Europe/Berlin","Europe/Rome","Europe/Madrid","Europe/Amsterdam","Europe/Moscow","America/New_York","America/Chicago","America/Denver","America/Los_Angeles","America/Toronto","America/Vancouver","America/Sao_Paulo","America/Buenos_Aires","America/Mexico_City","America/Bogota","Africa/Cairo","Africa/Lagos","Africa/Nairobi","Pacific/Auckland","Pacific/Honolulu","UTC"]
                    },
                    "calendar_id": { "type": "string", "description": "Calendar containing the event.", "default": "primary" },
                    "recurrence":  { "type": "array",  "description": "Updated recurrence rules as RFC 5545 RRULE strings, e.g. 'RRULE:FREQ=WEEKLY;BYDAY=FR' or 'RRULE:FREQ=WEEKLY;BYDAY=FR;COUNT=10'.", "items": { "type": "string" } },
                    "send_updates": { "type": "string", "description": "Who receives notification emails: 'all' attendees, 'externalOnly' (only attendees outside your Google Workspace), or 'none'.", "enum": ["all","externalOnly","none"], "default": "all" }
                }, ["event_id"])),
            Tool::new("gcal_delete_event", "Delete a Google Calendar event. Attendees are notified unless 'send_updates' says otherwise. Set 'all_events' to true to delete all instances of a recurring event.", schema!({"event_id":{"type":"string"},"calendar_id":{"type":"string","default":"primary"},"all_events":{"type":"boolean","default":false},"send_updates":{"type":"string","description":"Who receives cancellation emails: 'all', 'externalOnly', or 'none'.","enum":["all","externalOnly","none"],"default":"all"}}, ["event_id"])),
            Tool::new("gcal_move_event", "Move an event from one Google calendar to another.", schema!({"event_id":{"type":"string"},"source_calendar_id":{"type":"string","default":"primary"},"destination_calendar_id":{"type":"string"},"send_updates":{"type":"string","description":"Who receives notification emails: 'all', 'externalOnly', or 'none'.","enum":["all","externalOnly","none"],"default":"all"}}, ["event_id","destination_calendar_id"])),
            Tool::new("gcal_quick_add", "Quick-add a calendar event from natural language, e.g. 'Team standup tomorrow 10am'.", schema!({"text":{"type":"string"},"calendar_id":{"type":"string","default":"primary"},"send_updates":{"type":"string","description":"Who receives notification emails: 'all', 'externalOnly', or 'none'.","enum":["all","externalOnly","none"],"default":"all"}}, ["text"])),
            Tool::new("gcal_get_freebusy", "Check free/busy time for a list of calendars.", schema!({"calendar_ids":{"type":"array","items":{"type":"string"}},"time_min":{"type":"string","description":"Window start; any common datetime format or Unix timestamp"},"time_max":{"type":"string","description":"Window end; same flexible formats as time_min"}}, ["calendar_ids","time_min","time_max"])),

            // Drive
            Tool::new("gdrive_list", "List Google Drive files/folders. Use this when asked what files or folders are in Drive.", schema!({"max_results":{"type":"integer","default":10},"folder_id":{"type":"string"},"mime_type":{"type":"string"}}, [])),
            Tool::new("gdrive_search", "Search Google Drive files by name or content.", schema!({"query":{"type":"string"},"max_results":{"type":"integer","default":10}}, ["query"])),
            Tool::new("gdrive_move_file", "Move a Google Drive file to another folder.", schema!({"file_id":{"type":"string"},"new_folder_id":{"type":"string"}}, ["file_id","new_folder_id"])),
            Tool::new("gdrive_share", "Share a Drive file. Also returns the public webViewLink if type=anyone.", schema!({"file_id":{"type":"string"},"role":{"type":"string","default":"reader"},"type":{"type":"string","default":"anyone"},"email":{"type":"string"}}, ["file_id"])),
            Tool::new("gdrive_export", "Export a Google Workspace document (Doc, Sheet, Slide) to a specific format like PDF, XLSX, or DOCX.", schema!({"file_id":{"type":"string"},"mime_type":{"type":"string","enum":["application/pdf","application/vnd.openxmlformats-officedocument.spreadsheetml.sheet","application/vnd.openxmlformats-officedocument.wordprocessingml.document","text/csv","text/plain","application/zip"]}}, ["file_id","mime_type"])),
            Tool::new("gdrive_download_binary", "Download a non-text Google Drive file to a local path so the agent can upload/send it.", schema!({"file_id":{"type":"string"}}, ["file_id"])),
            Tool::new("gdrive_upload_binary", "Upload a binary file from a local path to Google Drive.", schema!({"local_path":{"type":"string","description":"Local file path"},"name":{"type":"string","description":"Target file name in Drive"},"mime_type":{"type":"string","default":"application/octet-stream"},"folder_id":{"type":"string"}}, ["local_path","name"])),
            Tool::new("gdrive_upload_folder", "Upload a local folder recursively to Google Drive, preserving subfolder structure.", schema!({"local_folder_path":{"type":"string","description":"Local folder path"},"folder_name":{"type":"string","description":"Optional name for the new root folder in Drive"},"parent_folder_id":{"type":"string","description":"Optional destination parent folder ID"},"include_hidden":{"type":"boolean","default":false,"description":"Include hidden files/folders (dot-prefixed names)"}}, ["local_folder_path"])),
            Tool::new("gdrive_delete", "Permanently delete a Google Drive file or folder by ID. Supports bulk deletion with file_ids.", schema!({"file_id":{"type":"string","description":"Single file/folder ID. Can also be an expression that resolves to an array."},"file_ids":{"type":"array","items":{"type":"string"},"description":"Optional array of file/folder IDs for bulk deletion"}}, [])),
            Tool::new("gmail_send_with_attachment", "Send a Gmail email with a file attachment from a local path.", schema!({"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"},"local_path":{"type":"string","description":"Local file path to attach"}}, ["to","subject","body","local_path"])),

            // Contacts
            Tool::new("gcon_list_contacts", "List Google contacts (People API).", schema!({"max_results":{"type":"integer","default":50}}, [])),
            Tool::new("gcon_get_contact", "Get a single Google contact by resource name (e.g. 'people/c12345').", schema!({"name":{"type":"string"}}, ["name"])),
            Tool::new("gcon_create_contact", "Create a new Google contact.", schema!({"given_name":{"type":"string"},"family_name":{"type":"string"},"email":{"type":"string"},"phone":{"type":"string"},"notes":{"type":"string"}}, ["given_name"])),
            Tool::new("gcon_update_contact", "Update an existing Google contact.", schema!({"name":{"type":"string"},"given_name":{"type":"string"},"family_name":{"type":"string"},"email":{"type":"string"},"phone":{"type":"string"},"notes":{"type":"string"}}, ["name"])),
            Tool::new("gcon_delete_contact", "Delete a Google contact.", schema!({"name":{"type":"string"}}, ["name"])),
            Tool::new("gcon_search_contacts", "Search Google contacts by name, email, or phone.", schema!({"query":{"type":"string"},"max_results":{"type":"integer","default":10}}, ["query"])),

            // Meet
            Tool::new("gmeet_list_records", "List past Google Meet conference records.", schema!({"max_results":{"type":"integer","default":10},"filter":{"type":"string"}}, [])),
            Tool::new("gmeet_get_full_transcript", "Get the full, chronological transcript text for a Meet call.", schema!({"conference_record_name":{"type":"string","description":"Format: conferenceRecords/XXXXXXXXXXXX"}}, ["conference_record_name"])),

            // Tasks
            Tool::new("gtasks_list_lists", "List all Google Task lists.", schema!({"max_results":{"type":"integer","default":20}}, [])),
            Tool::new("gtasks_list_tasks", "List tasks in a specific task list.", schema!({"tasklist_id":{"type":"string"},"show_completed":{"type":"boolean","default":false}}, ["tasklist_id"])),
            Tool::new("gtasks_create_task", "Create a new Google Task.", schema!({"tasklist_id":{"type":"string"},"title":{"type":"string"},"notes":{"type":"string"},"due":{"type":"string","description":"RFC 3339 timestamp"}}, ["tasklist_id","title"])),
            Tool::new("gtasks_complete_task", "Mark a Google Task as completed.", schema!({"tasklist_id":{"type":"string"},"task_id":{"type":"string"}}, ["tasklist_id","task_id"])),

            // Docs
            Tool::new("gdocs_create", "Create a new Google Doc.", schema!({"title":{"type":"string"}}, ["title"])),
            Tool::new("gdocs_get_text", "Get the plain text content of a Google Doc.", schema!({"document_id":{"type":"string"}}, ["document_id"])),
            Tool::new("gdocs_append_text", "Append text to the end of a Google Doc.", schema!({"document_id":{"type":"string"},"text":{"type":"string"}}, ["document_id","text"])),

            // Sheets — Spreadsheet Management
            Tool::new("gsheets_list", "List Google Spreadsheets in the user's Drive. Returns id, name, modifiedTime for each.", schema!({"max_results":{"type":"integer","default":20}}, [])),
            Tool::new("gsheets_create", "Create a new Google Spreadsheet.", schema!({"title":{"type":"string"},"sheet_names":{"type":"array","items":{"type":"string"}}}, ["title"])),
            Tool::new("gsheets_get", "Get spreadsheet metadata (title, sheet tabs, properties). Use to discover sheet IDs.", schema!({"spreadsheet_id":{"type":"string"}}, ["spreadsheet_id"])),

            // Sheets — Reading & Writing
            Tool::new("gsheets_read_range", "Read cell values from a range (e.g. 'Sheet1!A1:D10').", schema!({"spreadsheet_id":{"type":"string"},"range":{"type":"string"}}, ["spreadsheet_id","range"])),
            Tool::new("gsheets_batch_read", "Read multiple ranges in a single request.", schema!({"spreadsheet_id":{"type":"string"},"ranges":{"type":"array","description":"Ranges to read, one per row.","items":{"type":"object","properties":{"range":{"type":"string","description":"e.g. Sheet1!A1:C10"}}}}}, ["spreadsheet_id","ranges"])),
            Tool::new("gsheets_write_range", "Write/update cell values in a range. 'values' is a 2D array.", schema!({"spreadsheet_id":{"type":"string"},"range":{"type":"string"},"values":{"type":"array","items":{"type":"array"}}}, ["spreadsheet_id","range","values"])),
            Tool::new("gsheets_batch_write", "Write to multiple ranges in one request. 'data' is an array of {range, values} objects.", schema!({"spreadsheet_id":{"type":"string"},"data":{"type":"array","items":{"type":"object","properties":{"range":{"type":"string"},"values":{"type":"array","items":{"type":"array"}}}}}}, ["spreadsheet_id","data"])),
            Tool::new("gsheets_append_rows", "Append rows after the last row with data. 'values' is a 2D array.", schema!({"spreadsheet_id":{"type":"string"},"range":{"type":"string"},"values":{"type":"array","items":{"type":"array"}}}, ["spreadsheet_id","range","values"])),
            Tool::new("gsheets_clear_range", "Clear all cell values in a range (keeps formatting).", schema!({"spreadsheet_id":{"type":"string"},"range":{"type":"string"}}, ["spreadsheet_id","range"])),
            Tool::new("gsheets_find", "Search for a value in a sheet range. Returns matching cell addresses and values.", schema!({"spreadsheet_id":{"type":"string"},"range":{"type":"string","description":"Range to search, e.g. 'Sheet1!A1:Z1000'"},"query":{"type":"string","description":"Text to search for (case-insensitive)"}}, ["spreadsheet_id","range","query"])),

            // Sheets — Tab Management
            Tool::new("gsheets_add_sheet", "Add a new sheet tab to an existing spreadsheet.", schema!({"spreadsheet_id":{"type":"string"},"title":{"type":"string"}}, ["spreadsheet_id","title"])),
            Tool::new("gsheets_delete_sheet", "Delete a sheet tab by its numeric sheet ID. Use gsheets_get to find IDs.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"}}, ["spreadsheet_id","sheet_id"])),
            Tool::new("gsheets_rename_sheet", "Rename a sheet tab.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"new_title":{"type":"string"}}, ["spreadsheet_id","sheet_id","new_title"])),
            Tool::new("gsheets_duplicate_sheet", "Duplicate a sheet tab within the same spreadsheet.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"new_title":{"type":"string"}}, ["spreadsheet_id","sheet_id"])),
            Tool::new("gsheets_copy_sheet_to", "Copy a sheet tab to a different spreadsheet.", schema!({"source_spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"destination_spreadsheet_id":{"type":"string"}}, ["source_spreadsheet_id","sheet_id","destination_spreadsheet_id"])),
            Tool::new("gsheets_export_sheet", "Export a specific sheet tab to PDF, XLSX, or CSV with print options.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"format":{"type":"string","enum":["pdf","xlsx","csv","tsv","ods","zip"],"default":"pdf"},"range":{"type":"string","description":"Optional cell range to export (e.g., 'A1:E20')"},"portrait":{"type":"boolean","default":true,"description":"True for portrait, false for landscape (PDF only)"},"fitw":{"type":"boolean","default":true,"description":"Fit to width (PDF only)"},"gridlines":{"type":"boolean","default":false,"description":"Show gridlines (PDF only)"}}, ["spreadsheet_id","sheet_id"])),

            // Sheets — Row / Column Manipulation
            Tool::new("gsheets_insert_dimension", "Insert empty rows or columns. dimension: 'ROWS' or 'COLUMNS'. start_index is 0-based.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"dimension":{"type":"string","enum":["ROWS","COLUMNS"]},"start_index":{"type":"integer"},"count":{"type":"integer"}}, ["spreadsheet_id","sheet_id","dimension","start_index","count"])),
            Tool::new("gsheets_delete_dimension", "Delete rows or columns. dimension: 'ROWS' or 'COLUMNS'. Indices are 0-based, end is exclusive.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"dimension":{"type":"string","enum":["ROWS","COLUMNS"]},"start_index":{"type":"integer"},"end_index":{"type":"integer"}}, ["spreadsheet_id","sheet_id","dimension","start_index","end_index"])),

            // Sheets — Sort & Filter
            Tool::new("gsheets_sort_range", "Sort a range by a column. All indices are 0-based.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"start_row":{"type":"integer"},"end_row":{"type":"integer"},"start_col":{"type":"integer"},"end_col":{"type":"integer"},"sort_column":{"type":"integer","description":"0-based column index"},"ascending":{"type":"boolean","default":true}}, ["spreadsheet_id","sheet_id","start_row","end_row","start_col","end_col","sort_column"])),
            Tool::new("gsheets_create_filter", "Add an auto-filter to a range. All indices are 0-based.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"start_row":{"type":"integer"},"end_row":{"type":"integer"},"start_col":{"type":"integer"},"end_col":{"type":"integer"}}, ["spreadsheet_id","sheet_id","start_row","end_row","start_col","end_col"])),
            Tool::new("gsheets_clear_filter", "Remove the auto-filter from a sheet.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"}}, ["spreadsheet_id","sheet_id"])),

            // Sheets — Merge / Unmerge
            Tool::new("gsheets_merge_cells", "Merge cells. merge_type: 'MERGE_ALL', 'MERGE_COLUMNS', or 'MERGE_ROWS'. Indices are 0-based.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"start_row":{"type":"integer"},"end_row":{"type":"integer"},"start_col":{"type":"integer"},"end_col":{"type":"integer"},"merge_type":{"type":"string","default":"MERGE_ALL"}}, ["spreadsheet_id","sheet_id","start_row","end_row","start_col","end_col"])),
            Tool::new("gsheets_unmerge_cells", "Unmerge all merged cells in a range. Indices are 0-based.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"start_row":{"type":"integer"},"end_row":{"type":"integer"},"start_col":{"type":"integer"},"end_col":{"type":"integer"}}, ["spreadsheet_id","sheet_id","start_row","end_row","start_col","end_col"])),

            // Sheets — Formatting
            Tool::new("gsheets_bold_row", "Make an entire row bold (e.g. for headers). row_index is 0-based.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"row_index":{"type":"integer"}}, ["spreadsheet_id","sheet_id","row_index"])),
            Tool::new("gsheets_freeze_rows", "Freeze the first N rows of a sheet.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"row_count":{"type":"integer"}}, ["spreadsheet_id","sheet_id","row_count"])),
            Tool::new("gsheets_auto_resize", "Auto-resize all columns in a sheet to fit content.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"}}, ["spreadsheet_id","sheet_id"])),
            Tool::new("gsheets_format_cells", "Apply formatting to a cell range: bold, italic, font_size, bg_color (hex like '#FF0000'), fg_color (hex), h_align ('LEFT','CENTER','RIGHT'). Indices are 0-based.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"start_row":{"type":"integer"},"end_row":{"type":"integer"},"start_col":{"type":"integer"},"end_col":{"type":"integer"},"bold":{"type":"boolean"},"italic":{"type":"boolean"},"font_size":{"type":"integer"},"bg_color":{"type":"string","description":"Hex color like '#4285F4'"},"fg_color":{"type":"string","description":"Hex text color like '#FFFFFF'"},"h_align":{"type":"string","enum":["LEFT","CENTER","RIGHT"]}}, ["spreadsheet_id","sheet_id","start_row","end_row","start_col","end_col"])),
            Tool::new("gsheets_add_conditional_format", "Add conditional formatting with custom formula. bg_color is hex like '#FF0000'. Indices are 0-based.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"},"start_row":{"type":"integer"},"end_row":{"type":"integer"},"start_col":{"type":"integer"},"end_col":{"type":"integer"},"formula":{"type":"string","description":"Custom formula, e.g. '=A1>100'"},"bg_color":{"type":"string","description":"Hex color like '#FF0000'"}}, ["spreadsheet_id","sheet_id","start_row","end_row","start_col","end_col","formula","bg_color"])),
            Tool::new("gsheets_clear_conditional_formats", "Remove all conditional formatting rules from a sheet.", schema!({"spreadsheet_id":{"type":"string"},"sheet_id":{"type":"integer"}}, ["spreadsheet_id","sheet_id"])),
            Tool::new("gsheets_batch_update", "Send arbitrary batchUpdate requests to a spreadsheet. 'requests' is a JSON array of request objects.", schema!({"spreadsheet_id":{"type":"string"},"requests":{"type":"array","items":{"type":"object"}}}, ["spreadsheet_id","requests"])),

            // Slides
            Tool::new("gslides_create", "Create a new Google Slides presentation.", schema!({"title":{"type":"string"}}, ["title"])),
            Tool::new("gslides_replace_text", "Replace all occurrences of text in a presentation.", schema!({"presentation_id":{"type":"string"},"find":{"type":"string"},"replacement":{"type":"string"}}, ["presentation_id","find","replacement"])),

            // Chat
            Tool::new("gchat_list_spaces", "List Google Chat spaces.", schema!({"max_results":{"type":"integer","default":20}}, [])),
            Tool::new("gchat_send_message", "Send a message to a Google Chat space.", schema!({"space_name":{"type":"string","description":"Format: spaces/XXXXXX"},"text":{"type":"string"}}, ["space_name","text"])),
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
                let cc = a.get("cc").and_then(|v| v.as_str());
                let bcc = a.get("bcc").and_then(|v| v.as_str());
                let attach = a
                    .get("attachment_path")
                    .and_then(|v| v.as_str())
                    .filter(|p| !p.is_empty());
                let want_attach = a.get("send_attachment").map(truthy).unwrap_or(false);
                match attach.filter(|_| want_attach) {
                    Some(path) => {
                        gmail::send_with_attachment(
                            &self.0,
                            s("to")?,
                            s("subject")?,
                            s("body")?,
                            path,
                            cc,
                            bcc,
                        )
                        .await
                    }
                    None => {
                        gmail::send(&self.0, s("to")?, s("subject")?, s("body")?, cc, bcc).await
                    }
                }
            }
            "gmail_reply" => {
                let attach = a
                    .get("attachment_path")
                    .and_then(|v| v.as_str())
                    .filter(|p| {
                        !p.is_empty() && a.get("send_attachment").map(truthy).unwrap_or(false)
                    });
                gmail::reply(
                    &self.0,
                    a.get("thread_id").and_then(|v| v.as_str()).unwrap_or(""),
                    s("message_id")?,
                    s("to")?,
                    a.get("subject").and_then(|v| v.as_str()).unwrap_or(""),
                    s("body")?,
                    attach,
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
            "gmail_download_all_attachments" => {
                gmail::download_all_attachments(&self.0, s("message_id")?).await
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

            // Calendar. Optional params go through opt_str/opt_bool so the
            // blank strings workflow nodes send for untouched fields read as
            // "not provided" instead of empty values.
            "gcal_list_calendars" => calendar::list_calendars(&self.0).await,
            "gcal_list_events" => {
                let time_min = opt_dt(a, "time_min");
                let time_max = opt_dt(a, "time_max");
                calendar::list_events(
                    &self.0,
                    n("max_results", 10.0).clamp(1.0, 2500.0) as u32,
                    time_min.as_deref(),
                    time_max.as_deref(),
                    opt_str(a, "query"),
                    opt_str(a, "calendar_id").unwrap_or("primary"),
                    opt_bool(a, "single_events"),
                    opt_str(a, "page_token"),
                )
                .await
            }
            "gcal_create_event" => {
                let start = req_dt(a, "start")?;
                let end = req_dt(a, "end")?;
                calendar::create_event(
                    &self.0,
                    req_str(a, "summary")?,
                    &start,
                    &end,
                    opt_str(a, "description"),
                    opt_str(a, "location"),
                    extract_attendees(a, "attendees"),
                    opt_str(a, "time_zone"),
                    opt_bool(a, "create_meet_link").unwrap_or(false),
                    opt_str(a, "calendar_id").unwrap_or("primary"),
                    json_arr_opt(a, "recurrence"),
                    calendar::send_updates_or_all(opt_str(a, "send_updates")),
                )
                .await
            }
            "gcal_get_event" => {
                calendar::get_event(
                    &self.0,
                    req_str(a, "event_id")?,
                    opt_str(a, "calendar_id").unwrap_or("primary"),
                )
                .await
            }
            "gcal_update_event" => {
                let start = opt_dt(a, "start");
                let end = opt_dt(a, "end");
                calendar::update_event(
                    &self.0,
                    req_str(a, "event_id")?,
                    opt_str(a, "summary"),
                    start.as_deref(),
                    end.as_deref(),
                    opt_str(a, "description"),
                    opt_str(a, "location"),
                    opt_str(a, "time_zone"),
                    opt_str(a, "calendar_id").unwrap_or("primary"),
                    extract_attendees(a, "attendees"),
                    json_arr_opt(a, "recurrence"),
                    calendar::send_updates_or_all(opt_str(a, "send_updates")),
                )
                .await
            }
            "gcal_delete_event" => {
                calendar::delete_event(
                    &self.0,
                    req_str(a, "event_id")?,
                    opt_str(a, "calendar_id").unwrap_or("primary"),
                    opt_bool(a, "all_events").unwrap_or(false),
                    calendar::send_updates_or_all(opt_str(a, "send_updates")),
                )
                .await
            }
            "gcal_move_event" => {
                calendar::move_event(
                    &self.0,
                    req_str(a, "event_id")?,
                    opt_str(a, "source_calendar_id").unwrap_or("primary"),
                    req_str(a, "destination_calendar_id")?,
                    calendar::send_updates_or_all(opt_str(a, "send_updates")),
                )
                .await
            }
            "gcal_quick_add" => {
                calendar::quick_add(
                    &self.0,
                    req_str(a, "text")?,
                    opt_str(a, "calendar_id").unwrap_or("primary"),
                    calendar::send_updates_or_all(opt_str(a, "send_updates")),
                )
                .await
            }
            "gcal_get_freebusy" => {
                let time_min = req_dt(a, "time_min")?;
                let time_max = req_dt(a, "time_max")?;
                calendar::get_freebusy(&self.0, json_arr(a, "calendar_ids")?, &time_min, &time_max)
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
                let ranges = parse_batch_read_ranges(a.get("ranges"));
                if ranges.is_empty() {
                    anyhow::bail!(
                        "gsheets_batch_read: no valid ranges to read. Add at least one range \
                         like 'Sheet1!A1:C10' (a range that resolves to null/blank is skipped)."
                    );
                }
                sheets::batch_read(&self.0, s("spreadsheet_id")?, ranges).await
            }
            "gsheets_write_range" => {
                let values = parse_2d_values(a);
                sheets::write_range(&self.0, s("spreadsheet_id")?, s("range")?, values).await
            }
            "gsheets_batch_write" => {
                let data = parse_batch_write_data(a.get("data"));
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

/// Coerce a JSON value into a list of strings, tolerating the shapes the UI and
/// agents actually send: a real array (`["a","b"]`), a JSON-encoded array string
/// from the UI's "(JSON array)" text box (`"[\"a\",\"b\"]"`), or a single bare
/// value (`"abc"` → `["abc"]`). Commas are NOT treated as separators — values
/// like calendar RRULEs legitimately contain them.
fn coerce_str_vec(v: &Value) -> Vec<String> {
    match v {
        Value::Array(arr) => arr.iter().filter_map(scalar_to_string).collect(),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Vec::new();
            }
            if t.starts_with('[') {
                if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(t) {
                    return arr.iter().filter_map(scalar_to_string).collect();
                }
            }
            vec![t.to_string()]
        }
        Value::Number(_) | Value::Bool(_) => scalar_to_string(v).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Interpret a config toggle that the UI/LLM may send as a real bool, or as a
/// string/number ("true"/"1"/"yes"/"on") — workflow nodes and serializers are
/// inconsistent about boolean encoding.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_i64() == Some(1),
        Value::String(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
        _ => false,
    }
}

/// Optional string param. Workflow nodes send `""` for every untouched field
/// (the UI initializes all config keys), so blank must read as "not provided" —
/// treating it as a value made gcal_update_event blank out summaries and PATCH
/// `start.dateTime: ""` into a 400.
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

fn json_arr(args: &Map<String, Value>, key: &str) -> Result<Vec<String>> {
    let items = args.get(key).map(coerce_str_vec).unwrap_or_default();
    if items.is_empty() {
        return Err(anyhow::anyhow!("missing required param '{key}'"));
    }
    Ok(items)
}

fn json_arr_opt(args: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    let items = args.get(key).map(coerce_str_vec).unwrap_or_default();
    (!items.is_empty()).then_some(items)
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

/// Parse the `ranges` argument for `gsheets_batch_read` into range strings.
/// Like `parse_batch_write_data` below, this reaches us in several shapes:
///   • a JSON array of range strings (LLM tool call, legacy saved nodes)
///   • an array of `{range}` objects or a `{"parameters": [...]}`
///     fixedCollection wrapper (workflow UI node)
///   • a JSON *string* of any of the above, or a single bare range string
/// Blank/unresolvable entries are skipped; the caller errors if none survive.
fn parse_batch_read_ranges(v: Option<&Value>) -> Vec<String> {
    let Some(v) = v else {
        return vec![];
    };

    let entries: Vec<Value> = match v {
        Value::Array(arr) => arr.clone(),
        Value::Object(obj) => obj
            .get("parameters")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default(),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                vec![]
            } else if t.starts_with('[') || t.starts_with('{') {
                match serde_json::from_str::<Value>(t) {
                    Ok(Value::Array(arr)) => arr,
                    Ok(Value::Object(obj)) => obj
                        .get("parameters")
                        .and_then(|p| p.as_array())
                        .cloned()
                        .unwrap_or_default(),
                    _ => vec![Value::String(t.to_string())],
                }
            } else {
                vec![Value::String(t.to_string())]
            }
        }
        _ => vec![],
    };

    entries
        .iter()
        .filter_map(|entry| {
            let raw = match entry {
                Value::Object(obj) => obj.get("range")?,
                other => other,
            };
            let s = match raw {
                Value::String(s) => s.trim().to_string(),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            (!s.is_empty()).then_some(s)
        })
        .collect()
}

/// Parse the `data` argument for `gsheets_batch_write` into `(range, values)`
/// pairs. This is deliberately permissive because `data` reaches us in several
/// shapes depending on the caller:
///   • a JSON array of `{range, values}` objects (LLM tool call)
///   • a `{"parameters": [...]}` fixedCollection wrapper (workflow UI node)
///   • a JSON *string* of either of the above (some LLM/tool serializers)
///   • entries that are themselves JSON-encoded object strings
/// Rows whose `range` is missing/blank/non-stringifiable are skipped (this is
/// what stops a leftover empty UI row, or an expression that resolved to null,
/// from poisoning or silently emptying the whole batch). `batch_write` then
/// errors if *nothing* survives, so the failure is visible instead of a no-op.
fn parse_batch_write_data(data_v: Option<&Value>) -> Vec<(String, Vec<Vec<Value>>)> {
    let Some(v) = data_v else {
        return vec![];
    };

    // Resolve the top-level value to a list of entry Values.
    let entries: Vec<Value> = match v {
        Value::Array(arr) => arr.clone(),
        Value::Object(obj) => obj
            .get("parameters")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default(),
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(arr)) => arr,
            Ok(Value::Object(obj)) => obj
                .get("parameters")
                .and_then(|p| p.as_array())
                .cloned()
                .unwrap_or_default(),
            _ => vec![],
        },
        _ => vec![],
    };

    entries
        .into_iter()
        .filter_map(|entry| {
            // An entry may itself be a JSON-encoded object string.
            let entry = match &entry {
                Value::String(s) => {
                    serde_json::from_str::<Value>(s).unwrap_or_else(|_| entry.clone())
                }
                _ => entry,
            };
            let obj = entry.as_object()?;

            // Accept a string range, or a number we can stringify; trim it.
            let range = match obj.get("range") {
                Some(Value::String(s)) => s.trim().to_string(),
                Some(Value::Number(n)) => n.to_string(),
                _ => return None,
            };
            if range.is_empty() {
                return None;
            }

            // A row with no values writes nothing; skip it rather than emit an
            // empty ValueRange that contributes a silent no-op.
            let values = parse_2d_values(obj);
            if values.is_empty() {
                return None;
            }
            Some((range, values))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn batch_read_ranges_accepts_all_shapes() {
        // LLM tool call: plain string array
        let v = json!(["Sheet1!A1:B2", "Sheet2!C1:D2"]);
        assert_eq!(
            parse_batch_read_ranges(Some(&v)),
            vec!["Sheet1!A1:B2", "Sheet2!C1:D2"]
        );

        // Workflow UI fixedCollection envelope with {range} objects
        let v =
            json!({"parameters": [{"range": " Sheet1!A1:B2 "}, {"range": ""}, {"range": "C1:D2"}]});
        assert_eq!(
            parse_batch_read_ranges(Some(&v)),
            vec!["Sheet1!A1:B2", "C1:D2"]
        );

        // Bare array of {range} objects
        let v = json!([{"range": "A1:B2"}]);
        assert_eq!(parse_batch_read_ranges(Some(&v)), vec!["A1:B2"]);

        // JSON-encoded string forms (legacy textarea / serializers)
        let v = json!("[\"A1:B2\",\"C3:D4\"]");
        assert_eq!(parse_batch_read_ranges(Some(&v)), vec!["A1:B2", "C3:D4"]);
        let v = json!("{\"parameters\":[{\"range\":\"A1:B2\"}]}");
        assert_eq!(parse_batch_read_ranges(Some(&v)), vec!["A1:B2"]);

        // Single bare range string
        let v = json!("Sheet1!A1:C10");
        assert_eq!(parse_batch_read_ranges(Some(&v)), vec!["Sheet1!A1:C10"]);

        // Nothing valid
        assert!(parse_batch_read_ranges(None).is_empty());
        assert!(parse_batch_read_ranges(Some(&json!(""))).is_empty());
        assert!(parse_batch_read_ranges(Some(&json!({"parameters": [{"range": ""}]}))).is_empty());
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
