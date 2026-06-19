// calendar.rs
use crate::auth::access_token;
use anyhow::{bail, Result};
use axon_core::AppState;
use chrono::Utc;
use serde_json::{json, Value};

const BASE: &str = "https://graph.microsoft.com/v1.0";

// ── Calendars ─────────────────────────────────────────────────────────────────

/// List all calendars in the user's account.
pub async fn list_calendars(state: &AppState) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/me/calendars"))
        .bearer_auth(&tok)
        .query(&[("$select", "id,name,color,isDefaultCalendar,canEdit,owner")])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

// ── Events ────────────────────────────────────────────────────────────────────

/// List events. Supports free-text search via `query`.
/// Note: `calendar_id` and `query` cannot be combined — the Graph API does not
/// support `$search` on a calendarView endpoint.
pub async fn list_events(
    state: &AppState,
    max_count: u32,
    start_dt: Option<&str>,
    end_dt: Option<&str>,
    query: Option<&str>,
    calendar_id: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let default_end = (Utc::now() + chrono::Duration::days(30))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();

    if query.is_some() && calendar_id.is_some() {
        bail!(
            "The Microsoft Graph API does not support filtering by calendar_id \
             and free-text query at the same time. Please omit calendar_id when \
             using the query parameter."
        );
    }

    let (base, params) = if let Some(q) = query {
        let escaped = q.replace('"', "\\\"");
        (
            format!("{BASE}/me/events"),
            vec![
                ("$search", format!("\"{}\"", escaped)),
                ("$top", max_count.to_string()),
                (
                    "$select",
                    "id,subject,start,end,location,organizer,isAllDay,isCancelled,bodyPreview,attendees"
                        .to_owned(),
                ),
            ],
        )
    } else {
        let start = start_dt.unwrap_or(&now);
        let end = end_dt.unwrap_or(&default_end);
        let base = match calendar_id {
            Some(c) => format!("{BASE}/me/calendars/{c}/calendarView"),
            None => format!("{BASE}/me/calendarView"),
        };
        (
            base,
            vec![
                ("startDateTime", start.to_string()),
                ("endDateTime", end.to_string()),
                ("$top", max_count.to_string()),
                ("$orderby", "start/dateTime asc".to_owned()),
                (
                    "$select",
                    "id,subject,start,end,location,organizer,isAllDay,isCancelled,bodyPreview,attendees"
                        .to_owned(),
                ),
            ],
        )
    };

    let resp: Value = state
        .client
        .get(&base)
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

/// Fetch a single event by ID, including the full body and online meeting details.
pub async fn get_event(state: &AppState, event_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/me/events/{event_id}"))
        .bearer_auth(&tok)
        .query(&[(
            "$select",
            "id,subject,start,end,location,organizer,isAllDay,isCancelled,\
             body,attendees,isOnlineMeeting,onlineMeeting,recurrence,importance",
        )])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

/// Create a new event.
pub async fn create_event(
    state: &AppState,
    subject: &str,
    start: &str,
    end: &str,
    time_zone: &str,
    body: Option<&str>,
    location: Option<&str>,
    attendees: Option<Vec<&str>>,
    is_online: bool,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut ev = json!({
        "subject":          subject,
        "isOnlineMeeting":  is_online,
        "start":  { "dateTime": start, "timeZone": time_zone },
        "end":    { "dateTime": end,   "timeZone": time_zone },
    });
    if let Some(b) = body {
        ev["body"] = json!({"contentType": "Text", "content": b});
    }
    if let Some(l) = location {
        ev["location"] = json!({"displayName": l});
    }
    if let Some(att) = attendees {
        ev["attendees"] = json!(att
            .iter()
            .map(|a| json!({"emailAddress": {"address": a}, "type": "required"}))
            .collect::<Vec<_>>());
    }
    let resp: Value = state
        .client
        .post(format!("{BASE}/me/events"))
        .bearer_auth(&tok)
        .json(&ev)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

/// Update an event using PATCH (only the provided fields are changed).
pub async fn update_event(
    state: &AppState,
    event_id: &str,
    subject: Option<&str>,
    start: Option<&str>,
    end: Option<&str>,
    body: Option<&str>,
    location: Option<&str>,
    time_zone: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut patch = json!({});
    if let Some(s) = subject {
        patch["subject"] = json!(s);
    }
    if let Some(b) = body {
        patch["body"] = json!({"contentType": "Text", "content": b});
    }
    if let Some(l) = location {
        patch["location"] = json!({"displayName": l});
    }
    let tz = time_zone.unwrap_or("Asia/Manila");
    if let Some(s) = start {
        patch["start"] = json!({"dateTime": s, "timeZone": tz});
    }
    if let Some(e) = end {
        patch["end"] = json!({"dateTime": e, "timeZone": tz});
    }
    let resp: Value = state
        .client
        .patch(format!("{BASE}/me/events/{event_id}"))
        .bearer_auth(&tok)
        .json(&patch)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

/// Delete an event from the user's calendar (no attendee notification).
/// Use `cancel_event` instead if you are the organizer and want to notify attendees.
pub async fn delete_event(state: &AppState, event_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/me/events/{event_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .error_for_status()?;
    Ok(json!({ "success": true }))
}

/// Cancel an event as organizer — sends a cancellation notice to all attendees.
/// This is distinct from delete_event, which removes the event silently.
pub async fn cancel_event(
    state: &AppState,
    event_id: &str,
    comment: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .post(format!("{BASE}/me/events/{event_id}/cancel"))
        .bearer_auth(&tok)
        .json(&json!({"comment": comment.unwrap_or("")}))
        .send()
        .await?
        .error_for_status()?;
    Ok(json!({ "success": true, "cancelledEventId": event_id }))
}

/// Accept, decline, or tentatively accept a meeting invitation.
pub async fn respond_event(
    state: &AppState,
    event_id: &str,
    action: &str,
    comment: Option<&str>,
) -> Result<Value> {
    match action {
        "accept" | "decline" | "tentativelyAccept" => {}
        other => bail!(
            "Invalid respond action '{other}'. Must be accept, decline, or tentativelyAccept."
        ),
    }
    let tok = access_token(state).await?;
    state
        .client
        .post(format!("{BASE}/me/events/{event_id}/{action}"))
        .bearer_auth(&tok)
        .json(&json!({"comment": comment.unwrap_or(""), "sendResponse": true}))
        .send()
        .await?
        .error_for_status()?;
    Ok(json!({ "success": true, "action": action }))
}

// ── Scheduling ────────────────────────────────────────────────────────────────

/// Check free/busy availability for one or more users or calendars.
/// Returns busy blocks and a 30-minute-slot availability view.
pub async fn get_schedule(
    state: &AppState,
    emails: Vec<&str>,
    start: &str,
    end: &str,
    time_zone: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let tz = time_zone.unwrap_or("Asia/Manila");
    let body = json!({
        "schedules":                  emails,
        "startTime": { "dateTime": start, "timeZone": tz },
        "endTime":   { "dateTime": end,   "timeZone": tz },
        "availabilityViewInterval":   30,
    });
    let resp: Value = state
        .client
        .post(format!("{BASE}/me/calendar/getSchedule"))
        .bearer_auth(&tok)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

/// Find available meeting times for a set of attendees within a time window.
/// Returns ranked suggestions where all required attendees are free.
pub async fn find_meeting_times(
    state: &AppState,
    attendees: Vec<&str>,
    duration_minutes: u32,
    time_min: &str,
    time_max: &str,
    time_zone: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let tz = time_zone.unwrap_or("Asia/Manila");
    let body = json!({
        "attendees": attendees
            .iter()
            .map(|a| json!({"emailAddress": {"address": a}, "type": "required"}))
            .collect::<Vec<_>>(),
        "timeConstraint": {
            "activityDomain": "work",
            "timeslots": [{
                "start": { "dateTime": time_min, "timeZone": tz },
                "end":   { "dateTime": time_max, "timeZone": tz },
            }],
        },
        "meetingDuration":             format!("PT{}M", duration_minutes),
        "returnSuggestionReasons":     true,
        "minimumAttendeePercentage":   100,
    });
    let resp: Value = state
        .client
        .post(format!("{BASE}/me/findMeetingTimes"))
        .bearer_auth(&tok)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}
