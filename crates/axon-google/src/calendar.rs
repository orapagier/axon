use crate::auth::access_token;
use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use chrono::{NaiveDate, NaiveDateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

const BASE: &str = "https://www.googleapis.com/calendar/v3";

// ── Time handling ─────────────────────────────────────────────────────────────

/// IANA timezone applied when a caller doesn't specify one.
/// Override with AXON_DEFAULT_TZ (keep AXON_DEFAULT_TZ_OFFSET in sync).
pub(crate) fn default_tz() -> String {
    std::env::var("AXON_DEFAULT_TZ").unwrap_or_else(|_| "Asia/Manila".into())
}

/// Fixed UTC offset matching [`default_tz`], used to make naive datetimes
/// unambiguous where the API demands an offset (timeMin/timeMax, freeBusy).
/// Override with AXON_DEFAULT_TZ_OFFSET, e.g. "+02:00".
fn default_tz_offset() -> String {
    std::env::var("AXON_DEFAULT_TZ_OFFSET").unwrap_or_else(|_| "+08:00".into())
}

/// Normalize a user/expression-supplied time into the RFC 3339 form Google
/// requires for timeMin/timeMax:
///   - offset-aware strings ("...Z", "...+08:00") pass through untouched
///   - date-only "YYYY-MM-DD" expands to local midnight with the default offset
///   - naive datetimes get the default offset appended — NOT "Z", because a
///     naive time means operator-local wall clock, not UTC
/// Unrecognized shapes pass through so Google reports them in its own words.
fn normalize_rfc3339(t: &str) -> String {
    let t = t.trim();
    if chrono::DateTime::parse_from_rfc3339(t).is_ok() {
        return t.to_owned();
    }
    if NaiveDate::parse_from_str(t, "%Y-%m-%d").is_ok() {
        return format!("{t}T00:00:00{}", default_tz_offset());
    }
    if NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%.f").is_ok() {
        return format!("{t}{}", default_tz_offset());
    }
    // datetime-local without seconds ("2026-07-05T09:00")
    if NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M").is_ok() {
        return format!("{t}:00{}", default_tz_offset());
    }
    t.to_owned()
}

/// Build an event start/end object. A date-only value ("2026-07-05") produces
/// an all-day `{date}`; anything else a timed `{dateTime, timeZone}`. A naive
/// dateTime plus timeZone is Google's preferred wall-clock form, so timed
/// values are passed through as given.
fn event_time(value: &str, tz: &str) -> Value {
    let v = value.trim();
    if NaiveDate::parse_from_str(v, "%Y-%m-%d").is_ok() {
        json!({ "date": v })
    } else {
        json!({ "dateTime": v, "timeZone": tz })
    }
}

/// Google's all-day `end.date` is exclusive: a one-day event on the 5th needs
/// end = the 6th, and end == start is rejected as an empty range. Callers
/// naturally pass start == end for a single day, so bump the end forward when
/// both are dates and end doesn't already clear start.
fn fix_all_day_end(start: &str, end: &str) -> Option<String> {
    let s = NaiveDate::parse_from_str(start.trim(), "%Y-%m-%d").ok()?;
    let e = NaiveDate::parse_from_str(end.trim(), "%Y-%m-%d").ok()?;
    if e > s {
        return None; // already a valid exclusive end
    }
    Some(s.succ_opt()?.to_string())
}

/// Validated sendUpdates value; anything unrecognized falls back to "all",
/// which matches the node's historical behavior.
pub(crate) fn send_updates_or_all(v: Option<&str>) -> &'static str {
    match v {
        Some("none") => "none",
        Some("externalOnly") => "externalOnly",
        _ => "all",
    }
}

// ── Calendars ─────────────────────────────────────────────────────────────────

/// List all calendars in the user's calendar list.
pub async fn list_calendars(state: &AppState) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/users/me/calendarList"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

// ── Events ────────────────────────────────────────────────────────────────────

/// List events in a calendar. The response's `nextPageToken` (when present)
/// can be fed back via `page_token` to fetch the following page.
#[allow(clippy::too_many_arguments)]
pub async fn list_events(
    state: &AppState,
    max_results: u32,
    time_min: Option<&str>,
    time_max: Option<&str>,
    query: Option<&str>,
    calendar_id: &str,
    single_events: Option<bool>,
    page_token: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let now = Utc::now().to_rfc3339();
    let cal = urlenc(calendar_id);

    let mut params = vec![
        ("maxResults", max_results.to_string()),
        ("singleEvents", single_events.unwrap_or(true).to_string()),
    ];

    if single_events.unwrap_or(true) {
        params.push(("orderBy", "startTime".into()));
        // Default to "upcoming events" when no window is given.
        let tmin = time_min.map(normalize_rfc3339).unwrap_or(now.clone());
        params.push(("timeMin", tmin));
    } else if let Some(tmin) = time_min {
        params.push(("timeMin", normalize_rfc3339(tmin)));
    }
    if let Some(q) = query {
        params.push(("q", q.to_owned()));
    }
    if let Some(tmax) = time_max {
        params.push(("timeMax", normalize_rfc3339(tmax)));
    }
    if let Some(pt) = page_token {
        params.push(("pageToken", pt.to_owned()));
    }

    let resp: Value = state
        .client
        .get(format!("{BASE}/calendars/{cal}/events"))
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Fetch a single event by ID.
pub async fn get_event(state: &AppState, event_id: &str, calendar_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    let enc_event = urlenc(event_id);
    let resp: Value = state
        .client
        .get(format!("{BASE}/calendars/{cal}/events/{enc_event}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Create a new event. `send_updates` controls attendee notification emails.
///
/// Date-only start/end values ("2026-07-05") create an all-day event; the
/// exclusive all-day end is bumped forward automatically when start == end.
///
/// The `recurrence` parameter accepts a list of RRULE/EXRULE/RDATE/EXDATE strings
/// as defined in RFC 5545. Common examples:
///   - Every Friday:                  `["RRULE:FREQ=WEEKLY;BYDAY=FR"]`
///   - Every weekday:                 `["RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"]`
///   - Every Friday, 10 times:        `["RRULE:FREQ=WEEKLY;BYDAY=FR;COUNT=10"]`
///   - Every Friday until Dec 31:     `["RRULE:FREQ=WEEKLY;BYDAY=FR;UNTIL=20261231T000000Z"]`
///   - Every month on the 1st:        `["RRULE:FREQ=MONTHLY;BYMONTHDAY=1"]`
#[allow(clippy::too_many_arguments)]
pub async fn create_event(
    state: &AppState,
    summary: &str,
    start: &str,
    end: &str,
    description: Option<&str>,
    location: Option<&str>,
    attendees: Option<Vec<&str>>,
    time_zone: Option<&str>,
    create_meet_link: bool,
    calendar_id: &str,
    recurrence: Option<Vec<String>>,
    send_updates: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);

    let default_tz = default_tz();
    let tz = time_zone.unwrap_or(&default_tz);
    let end = fix_all_day_end(start, end).unwrap_or_else(|| end.to_owned());
    let mut body = json!({
        "summary": summary,
        "start":   event_time(start, tz),
        "end":     event_time(&end, tz),
    });
    if let Some(d) = description {
        body["description"] = json!(d);
    }
    if let Some(l) = location {
        body["location"] = json!(l);
    }
    if let Some(att) = attendees {
        body["attendees"] = json!(att.iter().map(|e| json!({"email": e})).collect::<Vec<_>>());
    }
    if let Some(rules) = recurrence {
        body["recurrence"] = json!(rules);
    }
    if create_meet_link {
        body["conferenceData"] = json!({
            "createRequest": {
                "requestId": Uuid::new_v4().to_string(),
                "conferenceSolutionKey": { "type": "hangoutsMeet" }
            }
        });
    }

    // conferenceDataVersion=1 is required for Meet links.
    let mut params = vec![("sendUpdates", send_updates)];
    if create_meet_link {
        params.push(("conferenceDataVersion", "1"));
    }

    let resp: Value = state
        .client
        .post(format!("{BASE}/calendars/{cal}/events"))
        .bearer_auth(&tok)
        .query(&params)
        .json(&body)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Update an event using PATCH (only the provided fields are changed).
pub async fn update_event(
    state: &AppState,
    event_id: &str,
    summary: Option<&str>,
    start: Option<&str>,
    end: Option<&str>,
    description: Option<&str>,
    location: Option<&str>,
    time_zone: Option<&str>,
    calendar_id: &str,
    attendees: Option<Vec<&str>>,
    recurrence: Option<Vec<String>>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    let enc_event = urlenc(event_id);

    let mut patch = json!({});
    if let Some(s) = summary {
        patch["summary"] = json!(s);
    }
    if let Some(d) = description {
        patch["description"] = json!(d);
    }
    if let Some(l) = location {
        patch["location"] = json!(l);
    }
    if let Some(att) = attendees {
        patch["attendees"] = json!(att.iter().map(|e| json!({"email": e})).collect::<Vec<_>>());
    }
    if let Some(rules) = recurrence {
        patch["recurrence"] = json!(rules);
    }
    let tz = time_zone.unwrap_or("Asia/Manila");
    if let Some(st) = start {
        patch["start"] = json!({"dateTime": st, "timeZone": tz});
    }
    if let Some(en) = end {
        patch["end"] = json!({"dateTime": en, "timeZone": tz});
    }

    let resp: Value = state
        .client
        .patch(format!(
            "{BASE}/calendars/{cal}/events/{enc_event}?sendUpdates=all"
        ))
        .bearer_auth(&tok)
        .json(&patch)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Delete an event. Attendees are notified.
/// Set `all_events` to true when deleting a recurring event to remove ALL instances at once.
///
/// How it works: the Google Calendar API does not accept any special query param to bulk-delete
/// a series. The correct approach is to delete the *series master* event — the root recurring
/// event whose ID is stored in the `recurringEventId` field of every individual instance.
/// Deleting the master removes every past and future instance in one single API call.
///
/// This function handles both cases automatically:
///   - If `event_id` is already the series master (no `recurringEventId` on the fetched event),
///     it is deleted directly.
///   - If `event_id` is a single instance, we first fetch it, read its `recurringEventId`,
///     and delete that master instead.
pub async fn delete_event(
    state: &AppState,
    event_id: &str,
    calendar_id: &str,
    all_events: bool,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);

    // Resolve the ID we will actually delete.
    let target_id: String = if all_events {
        // Fetch the event to discover the series master ID.
        let enc_event = urlenc(event_id);
        let event: Value = state
            .client
            .get(format!("{BASE}/calendars/{cal}/events/{enc_event}"))
            .bearer_auth(&tok)
            .send()
            .await?
            .ensure_ok()
            .await?
            .json()
            .await?;

        // If this event is itself an instance, `recurringEventId` points to the master.
        // If it is already the master (or a standalone event), use its own id.
        match event["recurringEventId"].as_str() {
            Some(master_id) => master_id.to_owned(),
            None => event_id.to_owned(),
        }
    } else {
        event_id.to_owned()
    };

    let enc_target = urlenc(&target_id);
    state
        .client
        .delete(format!(
            "{BASE}/calendars/{cal}/events/{enc_target}?sendUpdates=all"
        ))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?;

    Ok(json!({
        "success": true,
        "deletedEventId": target_id,
        "allInstances": all_events,
    }))
}

/// Move an event from one calendar to another.
pub async fn move_event(
    state: &AppState,
    event_id: &str,
    source_calendar_id: &str,
    destination_calendar_id: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(source_calendar_id);
    let enc_event = urlenc(event_id);
    let dest = urlenc(destination_calendar_id);
    let resp: Value = state
        .client
        .post(format!("{BASE}/calendars/{cal}/events/{enc_event}/move"))
        .bearer_auth(&tok)
        .query(&[("destination", &dest), ("sendUpdates", &"all".to_string())])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Create an event from a natural-language string (e.g. "Lunch with John tomorrow at noon").
pub async fn quick_add(state: &AppState, text: &str, calendar_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    let resp: Value = state
        .client
        .post(format!("{BASE}/calendars/{cal}/events/quickAdd"))
        .bearer_auth(&tok)
        .query(&[("text", text), ("sendUpdates", "all")])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

// ── Free/Busy ─────────────────────────────────────────────────────────────────

/// Query free/busy blocks for one or more calendars over a time range.
pub async fn get_freebusy(
    state: &AppState,
    calendar_ids: Vec<String>,
    time_min: &str,
    time_max: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let body = json!({
        "timeMin": time_min,
        "timeMax": time_max,
        "items":   calendar_ids.iter().map(|id| json!({"id": id})).collect::<Vec<_>>(),
    });
    let resp: Value = state
        .client
        .post(format!("{BASE}/freeBusy"))
        .bearer_auth(&tok)
        .json(&body)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn urlenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
