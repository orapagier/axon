// calendar.rs
use crate::auth::access_token;
use anyhow::{bail, Result};
use axon_core::flexidate::{
    annotate_slot_weekday, date_only, default_tz, fix_all_day_end, normalize_rfc3339,
    parse_flexible, retain_events_on_day, single_day_window_for, stamp_day_window, FlexiDateTime,
};
use axon_core::{AppState, EnsureOk};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

const BASE: &str = "https://graph.microsoft.com/v1.0";

// ── Time handling ─────────────────────────────────────────────────────────────
// Mirrors the Google Calendar adapter: every user/expression-supplied time
// goes through axon_core::flexidate so any common datetime shape works.

/// Build a Graph `dateTimeTimeZone` from any [`parse_flexible`] shape. Graph's
/// `dateTime` must be a naive local time — the zone lives in the separate
/// `timeZone` field — so:
///   - date-only values ("2026-07-05", "July 5, 2026") become that day's
///     midnight; the caller marks the event `isAllDay`
///   - naive datetimes pass through as wall clock in `tz` — the operator-local
///     reading, matching the Google adapter
///   - offset-aware values (including Unix timestamps) are converted to UTC
///     and tagged "UTC", preserving their absolute instant
/// Unparseable values pass through so Graph reports them in its own words.
fn graph_time(value: &str, tz: &str) -> Value {
    let v = value.trim();
    match parse_flexible(v) {
        Some(FlexiDateTime::DateOnly(d)) => {
            json!({ "dateTime": format!("{}T00:00:00", d.format("%Y-%m-%d")), "timeZone": tz })
        }
        Some(FlexiDateTime::Naive(dt)) => {
            json!({ "dateTime": dt.format("%Y-%m-%dT%H:%M:%S").to_string(), "timeZone": tz })
        }
        Some(FlexiDateTime::Zoned(dt)) => json!({
            "dateTime": dt.with_timezone(&Utc).format("%Y-%m-%dT%H:%M:%S").to_string(),
            "timeZone": "UTC"
        }),
        None => json!({ "dateTime": v, "timeZone": tz }),
    }
}

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
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

// ── Events ────────────────────────────────────────────────────────────────────

/// List events. Supports free-text search via `query`.
/// Note: `calendar_id` and `query` cannot be combined — the Graph API does not
/// support `$search` on a calendarView endpoint.
///
/// **Single-day hard guard:** when `start_dt` is a bare date (all-day semantics),
/// no `end_dt` is given, and there is no free-text `query` (the `$search` path
/// ignores time bounds entirely), the query is scoped to exactly that one
/// calendar day and returned events are post-filtered to those that actually
/// occur on that day (multi-day and overnight spans included). This prevents a
/// weak model from reporting the wrong day's recurring event when the requested
/// day is empty.
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

    // Detect single-day scope: date-only start_dt with no end_dt. The $search
    // path applies no time bounds, so the guard must not engage there.
    let day_window = (query.is_none() && end_dt.is_none())
        .then_some(start_dt)
        .flatten()
        .and_then(single_day_window_for);

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
        // Window bounds accept any flexible format; normalized to RFC 3339
        // with the default offset, matching the Google adapter's timeMin/Max.
        let (start, end) = if let Some(dw) = &day_window {
            (dw.start_rfc3339.clone(), dw.end_rfc3339.clone())
        } else {
            let start = start_dt.map(normalize_rfc3339).unwrap_or(now);
            let end = end_dt.map(normalize_rfc3339).unwrap_or_else(|| {
                // Window end defaults to 30 days past the later of now and
                // the start — anchoring on now alone would invert the range
                // (a 400) whenever the start is more than 30 days out.
                let anchor = match DateTime::parse_from_rfc3339(&start) {
                    Ok(t) => t.with_timezone(&Utc).max(Utc::now()),
                    Err(_) => Utc::now(),
                };
                (anchor + chrono::Duration::days(30))
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string()
            });
            (start, end)
        };
        let base = match calendar_id {
            Some(c) => format!("{BASE}/me/calendars/{}/calendarView", urlenc(c)),
            None => format!("{BASE}/me/calendarView"),
        };
        (
            base,
            vec![
                ("startDateTime", start),
                ("endDateTime", end),
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

    let mut resp: Value = state
        .client
        .get(&base)
        .bearer_auth(&tok)
        // Without this Graph renders start/end in UTC, so the weekday
        // annotation below would name the UTC day — off by one for any event
        // before local 8am in a UTC+8 default_tz. Graph accepts IANA names.
        .header("Prefer", format!("outlook.timezone=\"{}\"", default_tz()))
        .query(&params)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    let mut kept = 0;
    if let Some(items) = resp.get_mut("value").and_then(Value::as_array_mut) {
        for ev in items.iter_mut() {
            annotate_event_weekdays(ev);
        }
        // Single-day post-filter: keep only events occurring on that day.
        if let Some(dw) = &day_window {
            kept = retain_events_on_day(items, dw.date);
        }
    }
    // Stamp code-computed metadata so the model can't misreport the day.
    // (Only 2xx payloads reach here — ensure_ok bails on errors — so a
    // missing value array just means an empty day, and kept = 0 is right.)
    if let Some(dw) = &day_window {
        stamp_day_window(&mut resp, dw, kept);
    }
    Ok(resp)
}

/// Fetch a single event by ID, including the full body and online meeting details.
pub async fn get_event(state: &AppState, event_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut resp: Value = state
        .client
        .get(format!("{BASE}/me/events/{}", urlenc(event_id)))
        .bearer_auth(&tok)
        // Same as list_events: render times in the operator's tz so the
        // weekday annotation names the local day, not the UTC day.
        .header("Prefer", format!("outlook.timezone=\"{}\"", default_tz()))
        .query(&[(
            "$select",
            "id,subject,start,end,location,organizer,isAllDay,isCancelled,\
             body,attendees,isOnlineMeeting,onlineMeeting,recurrence,importance",
        )])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    annotate_event_weekdays(&mut resp);
    Ok(resp)
}

/// Tag an event's `start` and `end` with a code-computed weekday name so the
/// agent reports the day-of-week instead of deriving it (wrongly) from the date.
fn annotate_event_weekdays(ev: &mut Value) {
    if let Some(start) = ev.get_mut("start") {
        annotate_slot_weekday(start);
    }
    if let Some(end) = ev.get_mut("end") {
        annotate_slot_weekday(end);
    }
}

/// Create a new event.
///
/// Date-only start/end values ("2026-07-05") create an all-day event; Graph's
/// exclusive all-day end is bumped forward automatically when start == end,
/// mirroring the Google adapter.
pub async fn create_event(
    state: &AppState,
    subject: &str,
    start: &str,
    end: &str,
    time_zone: Option<&str>,
    body: Option<&str>,
    location: Option<&str>,
    attendees: Option<Vec<&str>>,
    is_online: bool,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let default_tz = default_tz();
    let tz = time_zone.unwrap_or(&default_tz);
    let end = fix_all_day_end(start, end).unwrap_or_else(|| end.to_owned());
    // Graph's all-day flag is event-level and demands midnight bounds, which
    // graph_time produces for date-only values.
    let all_day = date_only(start).is_some() && date_only(&end).is_some();
    let mut ev = json!({
        "subject":          subject,
        "isOnlineMeeting":  is_online,
        "isAllDay":         all_day,
        "start":  graph_time(start, tz),
        "end":    graph_time(&end, tz),
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
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Update an event using PATCH (only the provided fields are changed).
/// Date-only start/end values switch the event to all-day and timed values
/// switch it back, mirroring [`create_event`] and the Google adapter.
#[allow(clippy::too_many_arguments)]
pub async fn update_event(
    state: &AppState,
    event_id: &str,
    subject: Option<&str>,
    start: Option<&str>,
    end: Option<&str>,
    body: Option<&str>,
    location: Option<&str>,
    time_zone: Option<&str>,
    attendees: Option<Vec<&str>>,
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
    if let Some(att) = attendees {
        patch["attendees"] = json!(att
            .iter()
            .map(|a| json!({"emailAddress": {"address": a}, "type": "required"}))
            .collect::<Vec<_>>());
    }
    let default_tz = default_tz();
    let tz = time_zone.unwrap_or(&default_tz);
    let end = match (start, end) {
        // Both given as dates: apply the same exclusive-end bump as create.
        (Some(st), Some(en)) => Some(fix_all_day_end(st, en).unwrap_or_else(|| en.to_owned())),
        (_, en) => en.map(str::to_owned),
    };
    if let Some(st) = start {
        patch["start"] = graph_time(st, tz);
    }
    if let Some(en) = &end {
        patch["end"] = graph_time(en, tz);
    }
    // Graph validates isAllDay against midnight bounds, so flip it whenever
    // the event's times change; untouched-time patches leave it alone.
    if start.is_some() || end.is_some() {
        let all_day = start.map(|s| date_only(s).is_some()).unwrap_or(true)
            && end
                .as_deref()
                .map(|e| date_only(e).is_some())
                .unwrap_or(true);
        patch["isAllDay"] = json!(all_day);
    }
    let resp: Value = state
        .client
        .patch(format!("{BASE}/me/events/{}", urlenc(event_id)))
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

/// Delete an event from the user's calendar (no attendee notification).
/// Use `cancel_event` instead if you are the organizer and want to notify attendees.
pub async fn delete_event(state: &AppState, event_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/me/events/{}", urlenc(event_id)))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?;
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
        .post(format!("{BASE}/me/events/{}/cancel", urlenc(event_id)))
        .bearer_auth(&tok)
        .json(&json!({"comment": comment.unwrap_or("")}))
        .send()
        .await?
        .ensure_ok()
        .await?;
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
        .post(format!("{BASE}/me/events/{}/{action}", urlenc(event_id)))
        .bearer_auth(&tok)
        .json(&json!({"comment": comment.unwrap_or(""), "sendResponse": true}))
        .send()
        .await?
        .ensure_ok()
        .await?;
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
    let default_tz = default_tz();
    let tz = time_zone.unwrap_or(&default_tz);
    let body = json!({
        "schedules":                  emails,
        "startTime": graph_time(start, tz),
        "endTime":   graph_time(end, tz),
        "availabilityViewInterval":   30,
    });
    let resp: Value = state
        .client
        .post(format!("{BASE}/me/calendar/getSchedule"))
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
    let default_tz = default_tz();
    let tz = time_zone.unwrap_or(&default_tz);
    let body = json!({
        "attendees": attendees
            .iter()
            .map(|a| json!({"emailAddress": {"address": a}, "type": "required"}))
            .collect::<Vec<_>>(),
        "timeConstraint": {
            "activityDomain": "work",
            "timeslots": [{
                "start": graph_time(time_min, tz),
                "end":   graph_time(time_max, tz),
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
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Percent-encode a URL *path* segment. form_urlencoded emits "+" for spaces,
/// which is only a space in query strings — in a path it's a literal plus, so
/// rewrite it to %20. Graph event IDs can carry "=" and "+".
fn urlenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

#[cfg(test)]
mod tests {
    use super::*;

    // graph_time's default-offset path isn't exercised here (the tz argument
    // is explicit), but normalize_rfc3339 reads AXON_DEFAULT_TZ_OFFSET; tests
    // assume the default (+08:00).

    #[test]
    fn naive_and_foreign_formats_become_wall_clock_graph_times() {
        for input in [
            "2026-07-05T09:00:00",
            "2026-07-05T09:00",
            "2026-07-05 09:00:00",
            "2026-07-05 09:00",
        ] {
            assert_eq!(
                graph_time(input, "Asia/Manila"),
                json!({"dateTime": "2026-07-05T09:00:00", "timeZone": "Asia/Manila"}),
                "failed: {input}"
            );
        }
        assert_eq!(
            graph_time("July 5, 2026 at 3pm", "Asia/Manila"),
            json!({"dateTime": "2026-07-05T15:00:00", "timeZone": "Asia/Manila"})
        );
        assert_eq!(
            graph_time("07/05/2026 3:00 PM", "Asia/Manila"),
            json!({"dateTime": "2026-07-05T15:00:00", "timeZone": "Asia/Manila"})
        );
    }

    #[test]
    fn offset_aware_values_keep_their_instant_as_utc() {
        assert_eq!(
            graph_time("2026-07-05T09:00:00+08:00", "Asia/Manila"),
            json!({"dateTime": "2026-07-05T01:00:00", "timeZone": "UTC"})
        );
        assert_eq!(
            graph_time("2026-07-05T09:00:00Z", "Asia/Manila"),
            json!({"dateTime": "2026-07-05T09:00:00", "timeZone": "UTC"})
        );
        // Unix seconds resolve to an absolute UTC instant
        assert_eq!(
            graph_time("1783213200", "Asia/Manila"),
            json!({"dateTime": "2026-07-05T01:00:00", "timeZone": "UTC"})
        );
    }

    #[test]
    fn date_only_values_become_midnight_for_all_day() {
        for input in ["2026-07-05", "July 5, 2026", "07/05/2026"] {
            assert_eq!(
                graph_time(input, "Asia/Manila"),
                json!({"dateTime": "2026-07-05T00:00:00", "timeZone": "Asia/Manila"}),
                "failed: {input}"
            );
        }
    }

    #[test]
    fn garbage_passes_through_for_graph_to_report() {
        assert_eq!(
            graph_time("banana", "Asia/Manila"),
            json!({"dateTime": "banana", "timeZone": "Asia/Manila"})
        );
    }

    #[test]
    fn all_day_end_bumps_to_exclusive_next_day() {
        // start == end → one-day event needs end = next day (same rule as Google)
        assert_eq!(
            fix_all_day_end("2026-07-05", "2026-07-05"),
            Some("2026-07-06".into())
        );
        assert_eq!(fix_all_day_end("2026-07-05", "2026-07-06"), None);
        assert_eq!(
            fix_all_day_end("2026-07-05T09:00:00", "2026-07-05T09:00:00"),
            None
        );
        assert_eq!(
            fix_all_day_end("July 5, 2026", "July 5, 2026"),
            Some("2026-07-06".into())
        );
    }

    #[test]
    fn path_encoding_handles_graph_event_ids() {
        assert_eq!(urlenc("AAMkADg1OWUwLTk4M2E="), "AAMkADg1OWUwLTk4M2E%3D");
        assert_eq!(urlenc("abc+def/ghi"), "abc%2Bdef%2Fghi");
        assert_eq!(urlenc("has space"), "has%20space");
    }
}
