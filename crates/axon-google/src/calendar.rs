use crate::auth::access_token;
use anyhow::Result;
use axon_core::flexidate::{
    date_only, default_tz, fix_all_day_end, normalize_rfc3339, parse_flexible, FlexiDateTime,
};
use axon_core::{AppState, EnsureOk};
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

const BASE: &str = "https://www.googleapis.com/calendar/v3";

// ── Time handling ─────────────────────────────────────────────────────────────
// default_tz / normalize_rfc3339 / date_only / fix_all_day_end live in
// axon_core::flexidate, shared with the Microsoft calendar adapter.

/// Build an event start/end object from any [`parse_flexible`] shape. A
/// date-only value ("2026-07-05", "July 5, 2026") produces an all-day
/// `{date}`; naive datetimes become `{dateTime, timeZone}` — Google's
/// preferred wall-clock form; offset-aware values (including Unix timestamps)
/// keep their absolute instant. Unparseable values pass through so Google
/// reports them in its own words.
fn event_time(value: &str, tz: &str) -> Value {
    let v = value.trim();
    match parse_flexible(v) {
        Some(FlexiDateTime::DateOnly(d)) => json!({ "date": d.format("%Y-%m-%d").to_string() }),
        Some(FlexiDateTime::Naive(dt)) => {
            json!({ "dateTime": dt.format("%Y-%m-%dT%H:%M:%S").to_string(), "timeZone": tz })
        }
        Some(FlexiDateTime::Zoned(dt)) => {
            json!({ "dateTime": dt.to_rfc3339_opts(SecondsFormat::Secs, true), "timeZone": tz })
        }
        None => json!({ "dateTime": v, "timeZone": tz }),
    }
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
/// Date-only start/end values switch the event to all-day, mirroring
/// [`create_event`].
#[allow(clippy::too_many_arguments)]
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
    send_updates: &str,
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
    let default_tz = default_tz();
    let tz = time_zone.unwrap_or(&default_tz);
    let end = match (start, end) {
        // Both given as dates: apply the same exclusive-end bump as create.
        (Some(st), Some(en)) => Some(fix_all_day_end(st, en).unwrap_or_else(|| en.to_owned())),
        (_, en) => en.map(str::to_owned),
    };
    if let Some(st) = start {
        patch["start"] = event_time(st, tz);
    }
    if let Some(en) = end {
        patch["end"] = event_time(&en, tz);
    }

    let resp: Value = state
        .client
        .patch(format!("{BASE}/calendars/{cal}/events/{enc_event}"))
        .bearer_auth(&tok)
        .query(&[("sendUpdates", send_updates)])
        .json(&patch)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Delete an event. `send_updates` controls attendee notification emails.
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
    send_updates: &str,
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
        .delete(format!("{BASE}/calendars/{cal}/events/{enc_target}"))
        .bearer_auth(&tok)
        .query(&[("sendUpdates", send_updates)])
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
    send_updates: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(source_calendar_id);
    let enc_event = urlenc(event_id);
    let resp: Value = state
        .client
        .post(format!("{BASE}/calendars/{cal}/events/{enc_event}/move"))
        .bearer_auth(&tok)
        // destination goes through .query() raw — reqwest percent-encodes it;
        // pre-encoding here double-encodes the "@" every calendar ID contains.
        .query(&[("destination", destination_calendar_id), ("sendUpdates", send_updates)])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Create an event from a natural-language string (e.g. "Lunch with John tomorrow at noon").
pub async fn quick_add(
    state: &AppState,
    text: &str,
    calendar_id: &str,
    send_updates: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    let resp: Value = state
        .client
        .post(format!("{BASE}/calendars/{cal}/events/quickAdd"))
        .bearer_auth(&tok)
        .query(&[("text", text), ("sendUpdates", send_updates)])
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
        "timeMin": normalize_rfc3339(time_min),
        "timeMax": normalize_rfc3339(time_max),
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

/// Percent-encode a URL *path* segment. form_urlencoded emits "+" for spaces,
/// which is only a space in query strings — in a path it's a literal plus, so
/// rewrite it to %20.
fn urlenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

#[cfg(test)]
mod tests {
    use super::*;

    // The offset helpers read env vars; tests assume the defaults (+08:00).

    #[test]
    fn offset_aware_times_pass_through() {
        assert_eq!(normalize_rfc3339("2026-07-05T09:00:00Z"), "2026-07-05T09:00:00Z");
        assert_eq!(
            normalize_rfc3339("2026-07-05T09:00:00+08:00"),
            "2026-07-05T09:00:00+08:00"
        );
        assert_eq!(
            normalize_rfc3339("2026-07-05T09:00:00-05:00"),
            "2026-07-05T09:00:00-05:00"
        );
    }

    #[test]
    fn naive_times_get_local_offset_not_utc() {
        assert_eq!(
            normalize_rfc3339("2026-07-05T09:00:00"),
            "2026-07-05T09:00:00+08:00"
        );
        // datetime-local without seconds
        assert_eq!(normalize_rfc3339("2026-07-05T09:00"), "2026-07-05T09:00:00+08:00");
    }

    #[test]
    fn date_only_expands_to_local_midnight() {
        assert_eq!(normalize_rfc3339("2026-07-05"), "2026-07-05T00:00:00+08:00");
    }

    #[test]
    fn unrecognized_shapes_pass_through_for_google_to_report() {
        assert_eq!(normalize_rfc3339("not a date"), "not a date");
        assert_eq!(normalize_rfc3339(""), "");
    }

    #[test]
    fn foreign_formats_normalize_for_time_windows() {
        assert_eq!(normalize_rfc3339("2026-07-05 09:00:00"), "2026-07-05T09:00:00+08:00");
        assert_eq!(normalize_rfc3339("07/05/2026 3:00 PM"), "2026-07-05T15:00:00+08:00");
        assert_eq!(normalize_rfc3339("July 5, 2026"), "2026-07-05T00:00:00+08:00");
        // Unix seconds resolve to an absolute UTC instant
        assert_eq!(normalize_rfc3339("1783213200"), "2026-07-05T01:00:00Z");
    }

    #[test]
    fn date_only_values_become_all_day_events() {
        assert_eq!(event_time("2026-07-05", "Asia/Manila"), json!({"date": "2026-07-05"}));
        assert_eq!(event_time("July 5, 2026", "Asia/Manila"), json!({"date": "2026-07-05"}));
        assert_eq!(event_time("07/05/2026", "Asia/Manila"), json!({"date": "2026-07-05"}));
        assert_eq!(
            event_time("2026-07-05T09:00:00", "Asia/Manila"),
            json!({"dateTime": "2026-07-05T09:00:00", "timeZone": "Asia/Manila"})
        );
    }

    #[test]
    fn foreign_formats_become_wall_clock_event_times() {
        assert_eq!(
            event_time("2026-07-05 09:00", "Asia/Manila"),
            json!({"dateTime": "2026-07-05T09:00:00", "timeZone": "Asia/Manila"})
        );
        assert_eq!(
            event_time("July 5, 2026 at 3pm", "Asia/Manila"),
            json!({"dateTime": "2026-07-05T15:00:00", "timeZone": "Asia/Manila"})
        );
        // Offset-aware inputs keep their absolute instant
        assert_eq!(
            event_time("1783213200", "Asia/Manila"),
            json!({"dateTime": "2026-07-05T01:00:00Z", "timeZone": "Asia/Manila"})
        );
        // Garbage still passes through for Google to report
        assert_eq!(
            event_time("banana", "Asia/Manila"),
            json!({"dateTime": "banana", "timeZone": "Asia/Manila"})
        );
    }

    #[test]
    fn all_day_end_bumps_to_exclusive_next_day() {
        // start == end → one-day event needs end = next day
        assert_eq!(
            fix_all_day_end("2026-07-05", "2026-07-05"),
            Some("2026-07-06".into())
        );
        // valid exclusive end left alone
        assert_eq!(fix_all_day_end("2026-07-05", "2026-07-06"), None);
        // timed events are untouched
        assert_eq!(fix_all_day_end("2026-07-05T09:00:00", "2026-07-05T09:00:00"), None);
        // date-only in a foreign format still gets the bump
        assert_eq!(
            fix_all_day_end("July 5, 2026", "July 5, 2026"),
            Some("2026-07-06".into())
        );
    }

    #[test]
    fn send_updates_validates_with_all_fallback() {
        assert_eq!(send_updates_or_all(Some("none")), "none");
        assert_eq!(send_updates_or_all(Some("externalOnly")), "externalOnly");
        assert_eq!(send_updates_or_all(Some("bogus")), "all");
        assert_eq!(send_updates_or_all(None), "all");
    }

    #[test]
    fn path_encoding_handles_calendar_ids() {
        assert_eq!(urlenc("user@gmail.com"), "user%40gmail.com");
        assert_eq!(
            urlenc("en.philippines#holiday@group.v.calendar.google.com"),
            "en.philippines%23holiday%40group.v.calendar.google.com"
        );
        assert_eq!(urlenc("has space"), "has%20space");
    }
}
