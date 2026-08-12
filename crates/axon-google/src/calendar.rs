use crate::auth::access_token;
use anyhow::Result;
use axon_core::flexidate::{
    annotate_slot_weekday, default_tz, default_tz_offset, fix_all_day_end, normalize_rfc3339,
    parse_flexible, retain_events_on_day, single_day_window_for, stamp_day_window, FlexiDateTime,
};
use axon_core::{AppState, EnsureOk};
use chrono::{DateTime, Datelike, FixedOffset, NaiveTime, SecondsFormat, TimeZone, Utc, Weekday};
use serde_json::{json, Value};
use uuid::Uuid;

const BASE: &str = "https://www.googleapis.com/calendar/v3";

/// Google's `calendarExpansionMax` ceiling for a single free/busy query.
/// Calendars past this point are dropped from the response without an error.
const FREEBUSY_MAX_CALENDARS: usize = 50;

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

/// The event fields nobody sets on most events — colour, reminders, how the
/// time shows to others, what guests are allowed to do.
///
/// They live in a struct rather than as parameters because `create_event` and
/// `update_event` would otherwise take twenty-odd positional arguments, where a
/// single transposed `Option<&str>` compiles fine and silently writes the
/// location into the visibility field. Every field is `Option`: `None` means
/// "let Google decide" on create and "leave untouched" on update, so the same
/// struct serves both.
#[derive(Default)]
pub struct EventExtras<'a> {
    /// Google's palette index, "1".."11" (see `EVENT_COLORS`).
    pub color_id: Option<&'a str>,
    /// Minutes before the event to alert. `Some(0)` means "at start time".
    pub reminder_minutes: Option<i64>,
    /// "popup" (default) or "email".
    pub reminder_method: Option<&'a str>,
    /// True restores the calendar's own default reminders and drops any
    /// per-event override.
    pub use_default_reminders: Option<bool>,
    /// "default", "public" or "private".
    pub visibility: Option<&'a str>,
    /// Google's `transparency`: "opaque" shows the time as busy,
    /// "transparent" leaves it bookable.
    pub transparency: Option<&'a str>,
    pub guests_can_invite_others: Option<bool>,
    pub guests_can_modify: Option<bool>,
    pub guests_can_see_other_guests: Option<bool>,
}

/// Google's fixed event palette. Exposed so the node can offer colour names
/// instead of asking a non-technical user for the number "6".
pub const EVENT_COLORS: &[(&str, &str)] = &[
    ("1", "Lavender"),
    ("2", "Sage"),
    ("3", "Grape"),
    ("4", "Flamingo"),
    ("5", "Banana"),
    ("6", "Tangerine"),
    ("7", "Peacock"),
    ("8", "Graphite"),
    ("9", "Blueberry"),
    ("10", "Basil"),
    ("11", "Tomato"),
];

impl EventExtras<'_> {
    /// Write the set fields onto an event body (a create payload or a PATCH).
    fn apply(&self, body: &mut Value) {
        if let Some(c) = self.color_id.filter(|c| !c.is_empty()) {
            body["colorId"] = json!(c);
        }
        if let Some(v) = self.visibility.filter(|v| !v.is_empty()) {
            body["visibility"] = json!(v);
        }
        if let Some(t) = self.transparency.filter(|t| !t.is_empty()) {
            body["transparency"] = json!(t);
        }
        if let Some(b) = self.guests_can_invite_others {
            body["guestsCanInviteOthers"] = json!(b);
        }
        if let Some(b) = self.guests_can_modify {
            body["guestsCanModify"] = json!(b);
        }
        if let Some(b) = self.guests_can_see_other_guests {
            body["guestsCanSeeOtherGuests"] = json!(b);
        }

        // `reminders` is a single object, so an explicit override and "use the
        // calendar's defaults" are mutually exclusive — a payload carrying
        // overrides *and* useDefault:true is rejected by Google.
        match (self.use_default_reminders, self.reminder_minutes) {
            (Some(true), _) => body["reminders"] = json!({ "useDefault": true }),
            (_, Some(mins)) => {
                let method = match self.reminder_method {
                    Some("email") => "email",
                    _ => "popup",
                };
                body["reminders"] = json!({
                    "useDefault": false,
                    "overrides": [{ "method": method, "minutes": mins.clamp(0, 40_320) }],
                });
            }
            (Some(false), None) => body["reminders"] = json!({ "useDefault": false }),
            (None, None) => {}
        }
    }
}

// ── Calendars ─────────────────────────────────────────────────────────────────

/// List all calendars in the user's calendar list.
///
/// `showHidden=true` matters: calendars subscribed to under "Other calendars"
/// arrive with `hidden: true` until they're ticked in Google's own sidebar, and
/// the API's default (`false`) would silently drop them from the picker. Pages
/// are followed to the end so the result is the whole list rather than Google's
/// first 100 entries.
pub async fn list_calendars(state: &AppState) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut items: Vec<Value> = Vec::new();
    let mut page_token: Option<String> = None;

    loop {
        let mut params = vec![
            ("maxResults", "250".to_string()),
            ("showHidden", "true".to_string()),
        ];
        if let Some(pt) = &page_token {
            params.push(("pageToken", pt.clone()));
        }

        let resp: Value = state
            .client
            .get(format!("{BASE}/users/me/calendarList"))
            .bearer_auth(&tok)
            .query(&params)
            .send()
            .await?
            .ensure_ok()
            .await?
            .json()
            .await?;

        if let Some(page) = resp.get("items").and_then(|v| v.as_array()) {
            items.extend(page.iter().cloned());
        }

        match resp.get("nextPageToken").and_then(|v| v.as_str()) {
            Some(pt) => page_token = Some(pt.to_string()),
            None => break,
        }
    }

    Ok(json!({ "kind": "calendar#calendarList", "items": items }))
}

/// Create a new secondary calendar and return its `calendarList` entry.
///
/// `calendars.insert` creates the calendar but answers with the bare
/// `Calendar` resource, which carries no `accessRole`/`selected`/colour — the
/// fields the picker groups and renders by. Re-reading the `calendarList` entry
/// means a freshly created calendar looks exactly like every other one there,
/// so the UI needs no special case for "created a moment ago".
pub async fn create_calendar(
    state: &AppState,
    summary: &str,
    description: Option<&str>,
    time_zone: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let default_tz = default_tz();
    let mut body = json!({ "summary": summary, "timeZone": time_zone.unwrap_or(&default_tz) });
    if let Some(d) = description {
        body["description"] = json!(d);
    }

    let created: Value = state
        .client
        .post(format!("{BASE}/calendars"))
        .bearer_auth(&tok)
        .json(&body)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;

    let id = created["id"].as_str().unwrap_or_default().to_owned();
    // Best-effort: the calendar exists either way, so a hiccup reading the list
    // entry must not read back as "creation failed".
    match get_calendar_list_entry(state, &tok, &id).await {
        Ok(entry) => Ok(entry),
        Err(_) => Ok(created),
    }
}

/// Rename a calendar or change its description, timezone, colour or
/// show-in-list flag.
///
/// Google splits these across two resources: `summary`/`description`/`timeZone`
/// belong to the calendar itself and are shared with everyone it's shared with,
/// while the nickname, colour and visibility are per-subscriber and live on the
/// `calendarList` entry. A user renaming a calendar doesn't care which is which,
/// so this patches whichever resources the supplied fields touch.
#[allow(clippy::too_many_arguments)]
pub async fn update_calendar(
    state: &AppState,
    calendar_id: &str,
    summary: Option<&str>,
    description: Option<&str>,
    time_zone: Option<&str>,
    nickname: Option<&str>,
    color: Option<&str>,
    show_in_list: Option<bool>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);

    let mut shared = json!({});
    for (key, val) in [
        ("summary", summary),
        ("description", description),
        ("timeZone", time_zone),
    ] {
        if let Some(v) = val.filter(|v| !v.is_empty()) {
            shared[key] = json!(v);
        }
    }
    if shared.as_object().is_some_and(|o| !o.is_empty()) {
        state
            .client
            .patch(format!("{BASE}/calendars/{cal}"))
            .bearer_auth(&tok)
            .json(&shared)
            .send()
            .await?
            .ensure_ok()
            .await?;
    }

    let mut personal = json!({});
    if let Some(n) = nickname.filter(|n| !n.is_empty()) {
        personal["summaryOverride"] = json!(n);
    }
    if let Some(b) = show_in_list {
        // "Show in list" is the single switch a user means; Google splits it
        // into `selected` (ticked in the sidebar) and `hidden` (present in the
        // list at all), and leaving `hidden` set would keep the calendar out of
        // the picker no matter what `selected` says.
        personal["selected"] = json!(b);
        personal["hidden"] = json!(!b);
    }
    let rgb = color.filter(|c| c.starts_with('#'));
    if let Some(hex) = rgb {
        personal["backgroundColor"] = json!(hex);
        personal["foregroundColor"] = json!("#000000");
    } else if let Some(c) = color.filter(|c| !c.is_empty()) {
        personal["colorId"] = json!(c);
    }

    if personal.as_object().is_some_and(|o| !o.is_empty()) {
        let mut req = state
            .client
            .patch(format!("{BASE}/users/me/calendarList/{cal}"))
            .bearer_auth(&tok);
        // Hex colours are rejected unless the request opts into RGB format.
        if rgb.is_some() {
            req = req.query(&[("colorRgbFormat", "true")]);
        }
        req.json(&personal).send().await?.ensure_ok().await?;
    }

    get_calendar_list_entry(state, &tok, calendar_id).await
}

/// Permanently delete a secondary calendar, along with every event on it.
///
/// Refuses the primary calendar: Google answers that request with a bare 403,
/// and "you cannot delete your main calendar" is the thing the user needs to
/// read. Deleting a calendar you don't own isn't possible either — use
/// [`unsubscribe_calendar`] to remove it from your list instead.
pub async fn delete_calendar(state: &AppState, calendar_id: &str) -> Result<Value> {
    if calendar_id.trim().eq_ignore_ascii_case("primary") {
        anyhow::bail!(
            "Your primary calendar cannot be deleted. To remove a calendar you subscribed to, \
             use 'Remove calendar from my list' instead."
        );
    }
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    state
        .client
        .delete(format!("{BASE}/calendars/{cal}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?;
    Ok(json!({ "success": true, "deletedCalendarId": calendar_id }))
}

/// Subscribe to an existing calendar by ID, adding it under "Other calendars".
///
/// This is the API half of Google's "Subscribe to calendar" box, and the
/// counterpart to [`unsubscribe_calendar`]. The calendar has to already exist
/// and be readable by this account.
pub async fn subscribe_calendar(
    state: &AppState,
    calendar_id: &str,
    nickname: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut body = json!({ "id": calendar_id, "selected": true });
    if let Some(n) = nickname.filter(|n| !n.is_empty()) {
        body["summaryOverride"] = json!(n);
    }
    let resp: Value = state
        .client
        .post(format!("{BASE}/users/me/calendarList"))
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

/// Remove a calendar from this account's list without deleting the calendar
/// itself. The inverse of [`subscribe_calendar`].
pub async fn unsubscribe_calendar(state: &AppState, calendar_id: &str) -> Result<Value> {
    if calendar_id.trim().eq_ignore_ascii_case("primary") {
        anyhow::bail!("Your primary calendar cannot be removed from your calendar list.");
    }
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    state
        .client
        .delete(format!("{BASE}/users/me/calendarList/{cal}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?;
    Ok(json!({ "success": true, "removedCalendarId": calendar_id }))
}

// ── Sharing (ACL) ─────────────────────────────────────────────────────────────
//
// Subscribing and sharing are different things and are easy to confuse.
// `subscribe_calendar` adds a calendar to *this* account's sidebar, and only
// works if access was already granted. The rules below are what grants it: they
// decide who else in the world can see or edit a calendar.

/// The access levels Google's ACL accepts, paired with what each one actually
/// permits. Exposed so the node can offer "See when I'm busy (no details)"
/// rather than the API's `freeBusyReader`.
pub const ACL_ROLES: &[(&str, &str)] = &[
    (
        "freeBusyReader",
        "Can only see when I am busy, not what the events are",
    ),
    (
        "reader",
        "Can see every event and its details, but not change anything",
    ),
    ("writer", "Can add, edit and delete events"),
    (
        "owner",
        "Full control, including sharing the calendar with others",
    ),
];

/// Google's rule id for granting access to one person: `user:<email>`. Groups
/// and domains use their own prefixes, and `default` means the whole public.
fn acl_rule_id(email: &str) -> String {
    let email = email.trim();
    if email.eq_ignore_ascii_case("default") || email.eq_ignore_ascii_case("public") {
        return "default".to_string();
    }
    // Already a fully-formed scope ("group:team@x.com", "domain:x.com"): leave
    // it alone rather than producing "user:group:team@x.com".
    if let Some((prefix, _)) = email.split_once(':') {
        if matches!(prefix, "user" | "group" | "domain" | "default") {
            return email.to_string();
        }
    }
    format!("user:{email}")
}

/// List who has access to a calendar, and at what level.
///
/// Requires owner access on the calendar — Google returns 403 otherwise, which
/// is worth knowing before reading the error: an empty-looking failure here
/// usually means "this calendar isn't yours", not "nobody has access".
pub async fn list_acl(state: &AppState, calendar_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    let resp: Value = state
        .client
        .get(format!("{BASE}/calendars/{cal}/acl"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;

    // The raw rules read as `{scope: {type, value}, role}`, which takes a moment
    // to parse into "who". Flatten it, keeping the original alongside.
    let people: Vec<Value> = resp
        .get("items")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .map(|rule| {
            let scope = rule.get("scope");
            let scope_type = scope
                .and_then(|s| s.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("user");
            let who = scope
                .and_then(|s| s.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or(if scope_type == "default" {
                    "Anyone with the link"
                } else {
                    "unknown"
                });
            let role = rule.get("role").and_then(|v| v.as_str()).unwrap_or("");
            json!({
                "who": who,
                "scopeType": scope_type,
                "role": role,
                "access": ACL_ROLES
                    .iter()
                    .find(|(r, _)| *r == role)
                    .map(|(_, label)| *label)
                    .unwrap_or(role),
                "ruleId": rule.get("id").and_then(|v| v.as_str()).unwrap_or(""),
            })
        })
        .collect();

    Ok(json!({
        "calendarId": calendar_id,
        "sharedWithCount": people.len(),
        "sharedWith": people,
        "rules": resp.get("items").cloned().unwrap_or_else(|| json!([])),
    }))
}

/// Share a calendar with someone, or change the access they already have.
///
/// `acl.insert` is an upsert on the rule id, so re-sharing with a different
/// role updates it rather than erroring — which is what "change their access"
/// needs, and why there's no separate update tool.
pub async fn share_calendar(
    state: &AppState,
    calendar_id: &str,
    email: &str,
    role: &str,
    send_notifications: bool,
) -> Result<Value> {
    let email = email.trim();
    if email.is_empty() {
        anyhow::bail!("Give an email address to share the calendar with.");
    }
    if !ACL_ROLES.iter().any(|(r, _)| *r == role) {
        let names = ACL_ROLES
            .iter()
            .map(|(r, _)| *r)
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!("'{role}' is not an access level. Use one of: {names}.");
    }
    // "owner" on the public scope would hand the calendar to the entire
    // internet; Google rejects it, but late and unhelpfully.
    let rule_id = acl_rule_id(email);
    if rule_id == "default" && role != "freeBusyReader" && role != "reader" {
        anyhow::bail!(
            "Sharing publicly is limited to read access. Use 'reader' or 'freeBusyReader', or \
             name a specific person instead."
        );
    }

    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    let (scope_type, scope_value) = match rule_id.split_once(':') {
        Some((t, v)) => (t, Some(v)),
        None => ("default", None),
    };
    let mut scope = json!({ "type": scope_type });
    if let Some(v) = scope_value {
        scope["value"] = json!(v);
    }

    let resp: Value = state
        .client
        .post(format!("{BASE}/calendars/{cal}/acl"))
        .bearer_auth(&tok)
        .query(&[("sendNotifications", send_notifications.to_string())])
        .json(&json!({ "scope": scope, "role": role }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;

    Ok(json!({
        "success": true,
        "calendarId": calendar_id,
        "sharedWith": scope_value.unwrap_or("Anyone with the link"),
        "role": role,
        "access": ACL_ROLES
            .iter()
            .find(|(r, _)| *r == role)
            .map(|(_, label)| *label)
            .unwrap_or(role),
        "notified": send_notifications,
        "rule": resp,
    }))
}

/// Withdraw someone's access to a calendar.
///
/// Deleting the rule doesn't remove the calendar from their sidebar — Google
/// leaves the stale entry there until they dismiss it — but they lose the
/// ability to read or change anything immediately.
pub async fn unshare_calendar(state: &AppState, calendar_id: &str, email: &str) -> Result<Value> {
    let email = email.trim();
    if email.is_empty() {
        anyhow::bail!("Give the email address whose access should be withdrawn.");
    }
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    let rule = urlenc(&acl_rule_id(email));
    state
        .client
        .delete(format!("{BASE}/calendars/{cal}/acl/{rule}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?;
    Ok(json!({
        "success": true,
        "calendarId": calendar_id,
        "removedAccessFor": email,
    }))
}

/// One `calendarList` entry, which is the shape the calendar picker reads.
async fn get_calendar_list_entry(state: &AppState, tok: &str, calendar_id: &str) -> Result<Value> {
    let cal = urlenc(calendar_id);
    let resp: Value = state
        .client
        .get(format!("{BASE}/users/me/calendarList/{cal}"))
        .bearer_auth(tok)
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
///
/// **Single-day hard guard:** when `time_min` is a bare date (all-day semantics),
/// no `time_max` is given, and instances are being expanded (`single_events`
/// unset or true), the query is scoped to exactly that one calendar day and
/// returned events are post-filtered to those that actually occur on that day
/// (multi-day and overnight spans included). This prevents a weak model from
/// reporting the wrong day's recurring event when the requested day is empty.
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
    let expand = single_events.unwrap_or(true);

    // Detect single-day scope: date-only time_min with no time_max. Only the
    // expanded-instances path applies the one-day bounds, so the guard must
    // not engage for a singleEvents=false (series-master) listing.
    let day_window = (expand && time_max.is_none())
        .then_some(time_min)
        .flatten()
        .and_then(single_day_window_for);

    let mut params = vec![
        ("maxResults", max_results.to_string()),
        ("singleEvents", expand.to_string()),
    ];

    if expand {
        params.push(("orderBy", "startTime".into()));
        if let Some(dw) = &day_window {
            // Single-day window: bound to that exact day.
            params.push(("timeMin", dw.start_rfc3339.clone()));
            params.push(("timeMax", dw.end_rfc3339.clone()));
        } else {
            // Default to "upcoming events" when no window start is given.
            let tmin = time_min.map(normalize_rfc3339).unwrap_or(now);
            let tmax = time_max.map(normalize_rfc3339).unwrap_or_else(|| {
                // Window end defaults to 30 days past the later of now and
                // timeMin — anchoring on now alone would invert the range
                // (a 400) whenever timeMin is more than 30 days out.
                let anchor = match DateTime::parse_from_rfc3339(&tmin) {
                    Ok(t) => t.with_timezone(&Utc).max(Utc::now()),
                    Err(_) => Utc::now(),
                };
                (anchor + chrono::Duration::days(30)).to_rfc3339()
            });
            params.push(("timeMin", tmin));
            params.push(("timeMax", tmax));
        }
    } else {
        // Series-master listing: pass along whatever bounds the caller gave.
        if let Some(tmin) = time_min {
            params.push(("timeMin", normalize_rfc3339(tmin)));
        }
        if let Some(tmax) = time_max {
            params.push(("timeMax", normalize_rfc3339(tmax)));
        }
    }
    if let Some(q) = query {
        params.push(("q", q.to_owned()));
    }
    if let Some(pt) = page_token {
        params.push(("pageToken", pt.to_owned()));
    }

    let mut resp: Value = state
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
    let mut kept = 0;
    if let Some(items) = resp.get_mut("items").and_then(Value::as_array_mut) {
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
    // missing items array just means an empty day, and kept = 0 is right.)
    if let Some(dw) = &day_window {
        stamp_day_window(&mut resp, dw, kept);
    }
    Ok(resp)
}

/// Which way an event changed, worked out from the event body rather than left
/// to the caller to infer.
///
/// Google reports a change as "this event now looks like *this*"; nothing in the
/// payload says "created" or "cancelled" outright. `status: cancelled` marks a
/// deletion, and an event whose `created` stamp is itself after the cursor has
/// to be new — everything else is an edit to something that already existed.
fn change_type(event: &Value, since: DateTime<Utc>) -> &'static str {
    if event.get("status").and_then(|v| v.as_str()) == Some("cancelled") {
        return "cancelled";
    }
    let created = event
        .get("created")
        .and_then(|v| v.as_str())
        .and_then(|s| parse_instant(s).ok());
    match created {
        Some(c) if c >= since => "created",
        _ => "updated",
    }
}

/// Events on a calendar that were created, edited or cancelled since `since`.
///
/// The counterpart to `crm_changes_since`, and the feed the Google Calendar
/// trigger polls. `updatedMin` is the only way to ask Google "what moved?" —
/// listing by start time and diffing client-side would miss an edit to an event
/// whose start didn't change, which for a booking calendar is most of them.
///
/// `showDeleted` is on because a cancellation is the change people most want to
/// react to, and it is invisible without it.
pub async fn changes_since(
    state: &AppState,
    calendar_id: &str,
    since: &str,
    query: Option<&str>,
    max_results: u32,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    let since_rfc = normalize_rfc3339(since);
    let since_instant = parse_instant(&since_rfc)?;

    let mut params = vec![
        ("updatedMin", since_rfc.clone()),
        ("showDeleted", "true".to_string()),
        ("singleEvents", "true".to_string()),
        ("orderBy", "updated".to_string()),
        ("maxResults", max_results.clamp(1, 2500).to_string()),
    ];
    if let Some(q) = query.filter(|q| !q.is_empty()) {
        params.push(("q", q.to_owned()));
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

    let mut changes: Vec<Value> = Vec::new();
    let mut newest = since_instant;
    for ev in resp
        .get("items")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let mut ev = ev.clone();
        annotate_event_weekdays(&mut ev);
        let kind = change_type(&ev, since_instant);
        if let Some(u) = ev
            .get("updated")
            .and_then(|v| v.as_str())
            .and_then(|s| parse_instant(s).ok())
        {
            newest = newest.max(u);
        }
        ev["changeType"] = json!(kind);
        changes.push(ev);
    }

    Ok(json!({
        "calendarId": calendar_id,
        "since": since_rfc,
        // Advance past the newest change seen. `updatedMin` is inclusive, so
        // reusing the raw stamp would replay that last event on every poll.
        "cursor": (newest + chrono::Duration::milliseconds(1))
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        "changeCount": changes.len(),
        "changes": changes,
    }))
}

/// Fetch a single event by ID.
pub async fn get_event(state: &AppState, event_id: &str, calendar_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    let enc_event = urlenc(event_id);
    let mut resp: Value = state
        .client
        .get(format!("{BASE}/calendars/{cal}/events/{enc_event}"))
        .bearer_auth(&tok)
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
    extras: &EventExtras<'_>,
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
    extras.apply(&mut body);
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
    extras: &EventExtras<'_>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    let enc_event = urlenc(event_id);

    let mut patch = json!({});
    extras.apply(&mut patch);
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
        .query(&[
            ("destination", destination_calendar_id),
            ("sendUpdates", send_updates),
        ])
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

/// List the individual occurrences of a recurring event.
///
/// `gcal_list_events` with `single_events=false` finds the series master; this
/// expands that master back into its dated instances, which is what you need to
/// cancel or move one occurrence without touching the rest of the series.
pub async fn list_event_instances(
    state: &AppState,
    event_id: &str,
    calendar_id: &str,
    max_results: u32,
    time_min: Option<&str>,
    time_max: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
    let enc_event = urlenc(event_id);

    let mut params = vec![("maxResults", max_results.to_string())];
    if let Some(t) = time_min {
        params.push(("timeMin", normalize_rfc3339(t)));
    }
    if let Some(t) = time_max {
        params.push(("timeMax", normalize_rfc3339(t)));
    }

    let mut resp: Value = state
        .client
        .get(format!(
            "{BASE}/calendars/{cal}/events/{enc_event}/instances"
        ))
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    if let Some(items) = resp.get_mut("items").and_then(Value::as_array_mut) {
        for ev in items.iter_mut() {
            annotate_event_weekdays(ev);
        }
    }
    Ok(resp)
}

// ── Attendees & RSVP ──────────────────────────────────────────────────────────

/// Read an event, hand its attendee list to `edit`, and PATCH the result back.
///
/// Google's events API has no add/remove-one-attendee call: `attendees` is
/// replaced wholesale by whatever a PATCH carries, so an "add Bob" that sends
/// only Bob silently uninvites everyone else. Every attendee-shaped operation
/// therefore has to read-modify-write, and doing it in one place keeps the RSVP
/// path from drifting away from the add/remove paths.
async fn edit_attendees<F>(
    state: &AppState,
    event_id: &str,
    calendar_id: &str,
    send_updates: &str,
    edit: F,
) -> Result<Value>
where
    F: FnOnce(Vec<Value>) -> Result<Vec<Value>>,
{
    let tok = access_token(state).await?;
    let cal = urlenc(calendar_id);
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

    let current = event
        .get("attendees")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let updated = edit(current)?;

    let mut resp: Value = state
        .client
        .patch(format!("{BASE}/calendars/{cal}/events/{enc_event}"))
        .bearer_auth(&tok)
        .query(&[("sendUpdates", send_updates)])
        .json(&json!({ "attendees": updated }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    annotate_event_weekdays(&mut resp);
    Ok(resp)
}

/// Invite more people to an existing event, keeping the current guest list.
/// Emails already invited are left as they are, so their RSVP isn't reset.
pub async fn add_attendees(
    state: &AppState,
    event_id: &str,
    calendar_id: &str,
    emails: Vec<String>,
    send_updates: &str,
) -> Result<Value> {
    edit_attendees(
        state,
        event_id,
        calendar_id,
        send_updates,
        move |mut list| {
            for email in emails {
                let email = email.trim();
                if email.is_empty() {
                    continue;
                }
                let already = list.iter().any(|a| {
                    a.get("email")
                        .and_then(|v| v.as_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case(email))
                });
                if !already {
                    list.push(json!({ "email": email }));
                }
            }
            Ok(list)
        },
    )
    .await
}

/// Uninvite people from an event, leaving the rest of the guest list intact.
pub async fn remove_attendees(
    state: &AppState,
    event_id: &str,
    calendar_id: &str,
    emails: Vec<String>,
    send_updates: &str,
) -> Result<Value> {
    edit_attendees(state, event_id, calendar_id, send_updates, move |list| {
        let drop: Vec<String> = emails
            .iter()
            .map(|e| e.trim().to_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        Ok(list
            .into_iter()
            .filter(|a| {
                let email = a
                    .get("email")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_lowercase();
                !drop.contains(&email)
            })
            .collect())
    })
    .await
}

/// RSVP to an invitation as this account.
///
/// The attendee to update is the one Google flags `self: true` — matching on the
/// signed-in email would miss invitations sent to an alias or a group the
/// account belongs to, which are exactly the invitations people forget to answer.
pub async fn respond_to_event(
    state: &AppState,
    event_id: &str,
    calendar_id: &str,
    response: &str,
    comment: Option<&str>,
    send_updates: &str,
) -> Result<Value> {
    let status = rsvp_status(response)?;
    let comment = comment.map(str::to_owned);

    edit_attendees(
        state,
        event_id,
        calendar_id,
        send_updates,
        move |mut list| {
            let me = list
                .iter_mut()
                .find(|a| a.get("self").and_then(|v| v.as_bool()).unwrap_or(false));
            let Some(me) = me else {
                anyhow::bail!(
                    "You are not on this event's guest list, so there is nothing to RSVP to. \
                 Events you created yourself don't need a response."
                );
            };
            me["responseStatus"] = json!(status);
            if let Some(c) = comment.filter(|c| !c.is_empty()) {
                me["comment"] = json!(c);
            }
            Ok(list)
        },
    )
    .await
}

/// Google's `responseStatus` value for an RSVP.
///
/// The everyday words are accepted alongside the API's own, because "yes" and
/// "no" are what a workflow author types into the field and what a model reaches
/// for. An unrecognised answer is an error rather than a default: silently
/// accepting a meeting the user meant to decline is not a recoverable mistake.
fn rsvp_status(response: &str) -> Result<&'static str> {
    match response.trim().to_lowercase().as_str() {
        "accepted" | "accept" | "yes" => Ok("accepted"),
        "declined" | "decline" | "no" => Ok("declined"),
        "tentative" | "maybe" => Ok("tentative"),
        other => {
            anyhow::bail!("Unknown RSVP '{other}'. Use 'accepted', 'declined' or 'tentative'.")
        }
    }
}

// ── Free/Busy ─────────────────────────────────────────────────────────────────

/// Query free/busy blocks for one or more calendars over a time range.
pub async fn get_freebusy(
    state: &AppState,
    calendar_ids: Vec<String>,
    time_min: &str,
    time_max: &str,
) -> Result<Value> {
    // Google expands at most 50 calendars per query and silently drops the rest,
    // which would read as "those calendars are wide open". Refuse instead: a
    // wrong free/busy answer is worse than no answer.
    if calendar_ids.len() > FREEBUSY_MAX_CALENDARS {
        anyhow::bail!(
            "Google checks at most {FREEBUSY_MAX_CALENDARS} calendars at once, and {} were given. \
             Split them across separate steps — beyond the limit the extra calendars are dropped \
             and would look completely free.",
            calendar_ids.len()
        );
    }
    let tok = access_token(state).await?;
    let body = json!({
        "timeMin": normalize_rfc3339(time_min),
        "timeMax": normalize_rfc3339(time_max),
        // Explicit rather than relying on the API default, so the cap the guard
        // above enforces and the cap Google applies can't drift apart.
        "calendarExpansionMax": FREEBUSY_MAX_CALENDARS,
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

/// Find open time slots of at least `duration_minutes` across one or more
/// calendars — "when could we all meet for an hour next week?".
///
/// [`get_freebusy`] answers the opposite question (when is everyone *busy*), and
/// leaves the caller to invert a merged list of overlapping intervals across a
/// timezone boundary. That inversion is exactly the arithmetic a language model
/// gets subtly wrong — an off-by-one day, a slot that starts in the past, a
/// meeting proposed at 3am — so it happens here in code and the model only ever
/// sees a list of concrete, bookable slots.
///
/// `day_start`/`day_end` ("09:00", "17:00") clip every day to working hours in
/// the default timezone; omitting them searches around the clock.
#[allow(clippy::too_many_arguments)]
pub async fn find_free_slots(
    state: &AppState,
    calendar_ids: Vec<String>,
    time_min: Option<&str>,
    time_max: Option<&str>,
    duration_minutes: i64,
    day_start: Option<&str>,
    day_end: Option<&str>,
    skip_weekends: bool,
    max_slots: usize,
) -> Result<Value> {
    let offset = parse_offset(&default_tz_offset());
    let duration = chrono::Duration::minutes(duration_minutes.max(1));

    // Never propose a slot that has already passed: a search window starting
    // "today" means the rest of today, not 00:00 this morning.
    let now = Utc::now();
    let search_start = time_min
        .map(parse_instant)
        .transpose()?
        .unwrap_or(now)
        .max(now);
    let search_end = time_max
        .map(parse_instant)
        .transpose()?
        .unwrap_or_else(|| search_start + chrono::Duration::days(7));
    if search_end <= search_start {
        anyhow::bail!("The search window ends before it starts. Check 'time_min' and 'time_max'.");
    }

    let ids = if calendar_ids.is_empty() {
        vec!["primary".to_string()]
    } else {
        calendar_ids
    };
    let freebusy = get_freebusy(
        state,
        ids.clone(),
        &search_start.to_rfc3339(),
        &search_end.to_rfc3339(),
    )
    .await?;

    // Google reports per-calendar problems (no access, unknown id) inside a 200
    // response, so an unreadable calendar would otherwise read as "wide open".
    let mut unavailable: Vec<Value> = Vec::new();
    let mut busy: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
    if let Some(cals) = freebusy.get("calendars").and_then(|v| v.as_object()) {
        for (id, entry) in cals {
            if let Some(errs) = entry.get("errors").and_then(|v| v.as_array()) {
                let reason = errs
                    .first()
                    .and_then(|e| e.get("reason"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                unavailable.push(json!({ "calendarId": id, "reason": reason }));
            }
            for slot in entry
                .get("busy")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                let (Some(s), Some(e)) = (
                    slot.get("start").and_then(|v| v.as_str()),
                    slot.get("end").and_then(|v| v.as_str()),
                ) else {
                    continue;
                };
                if let (Ok(s), Ok(e)) = (parse_instant(s), parse_instant(e)) {
                    busy.push((s, e));
                }
            }
        }
    }

    // Merge overlapping busy blocks so a gap between two calendars' meetings
    // isn't mistaken for free time.
    busy.sort_by_key(|(s, _)| *s);
    let mut merged: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
    for (start, end) in busy {
        match merged.last_mut() {
            Some((_, prev_end)) if start <= *prev_end => *prev_end = (*prev_end).max(end),
            _ => merged.push((start, end)),
        }
    }

    let (open, close) = working_hours(day_start, day_end)?;

    let window = SlotWindow {
        from: search_start,
        to: search_end,
        duration,
        offset,
        open,
        close,
        skip_weekends,
        max_slots,
    };
    let slots = window.carve(&merged);

    Ok(json!({
        "slots": slots,
        "slotCount": slots.len(),
        "durationMinutes": duration.num_minutes(),
        "searchedCalendars": ids,
        "searchedFrom": search_start.to_rfc3339_opts(SecondsFormat::Secs, true),
        "searchedTo": search_end.to_rfc3339_opts(SecondsFormat::Secs, true),
        "timeZone": default_tz(),
        "unavailableCalendars": unavailable,
    }))
}

/// The shape of the free-slot search: a window, a minimum length, and the local
/// working hours to clip each day to.
///
/// Split out from [`find_free_slots`] so the interval arithmetic can be tested
/// against fixed busy blocks — the network half has nothing to do with whether
/// a gap that straddles midnight, or one that ends exactly when a meeting
/// starts, comes out right.
struct SlotWindow {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    duration: chrono::Duration,
    offset: FixedOffset,
    open: Option<NaiveTime>,
    close: Option<NaiveTime>,
    skip_weekends: bool,
    max_slots: usize,
}

impl SlotWindow {
    /// Subtract `busy` (sorted and already merged) from the window, one local
    /// day at a time, keeping the gaps long enough to hold the meeting.
    fn carve(&self, busy: &[(DateTime<Utc>, DateTime<Utc>)]) -> Vec<Value> {
        let mut slots: Vec<Value> = Vec::new();
        let mut day = self.from.with_timezone(&self.offset).date_naive();
        let last_day = self.to.with_timezone(&self.offset).date_naive();
        let (search_start, search_end, duration, offset, max_slots) = (
            self.from,
            self.to,
            self.duration,
            self.offset,
            self.max_slots,
        );
        let (open, close, skip_weekends) = (self.open, self.close, self.skip_weekends);

        'days: while day <= last_day {
            if skip_weekends && matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
                day = match day.succ_opt() {
                    Some(d) => d,
                    None => break,
                };
                continue;
            }

            // The bookable part of this day, in local time, clipped to the
            // search window. `local_instant` only returns None for a wall-clock
            // time that doesn't exist, which a fixed offset never produces.
            let day_open = match local_instant(&offset, day, open.unwrap_or(NaiveTime::MIN)) {
                Some(t) => t.max(search_start),
                None => break,
            };
            let day_close = match close {
                Some(c) => local_instant(&offset, day, c),
                // No closing time means "until midnight" — the next day's 00:00,
                // not 23:59, so an evening slot isn't cut a minute short.
                None => day
                    .succ_opt()
                    .and_then(|d| local_instant(&offset, d, NaiveTime::MIN)),
            };
            let day_close = match day_close {
                Some(t) => t.min(search_end),
                None => break,
            };

            let mut cursor = day_open;
            for (busy_start, busy_end) in busy {
                if *busy_end <= cursor {
                    continue;
                }
                if *busy_start >= day_close {
                    break;
                }
                let gap_end = (*busy_start).min(day_close);
                if gap_end - cursor >= duration {
                    slots.push(free_slot(cursor, gap_end, &offset));
                    if slots.len() >= max_slots {
                        break 'days;
                    }
                }
                cursor = cursor.max(*busy_end);
                if cursor >= day_close {
                    break;
                }
            }
            if day_close - cursor >= duration {
                slots.push(free_slot(cursor, day_close, &offset));
                if slots.len() >= max_slots {
                    break;
                }
            }

            day = match day.succ_opt() {
                Some(d) => d,
                None => break,
            };
        }

        slots
    }
}

/// One free window, reported in local time with its weekday spelled out so the
/// answer can be read back to the user without further date arithmetic.
fn free_slot(start: DateTime<Utc>, end: DateTime<Utc>, offset: &FixedOffset) -> Value {
    let local_start = start.with_timezone(offset);
    let local_end = end.with_timezone(offset);
    json!({
        "start": local_start.to_rfc3339_opts(SecondsFormat::Secs, false),
        "end": local_end.to_rfc3339_opts(SecondsFormat::Secs, false),
        "weekday": local_start.format("%A").to_string(),
        "date": local_start.format("%Y-%m-%d").to_string(),
        "availableMinutes": (end - start).num_minutes(),
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Absolute instant for any [`normalize_rfc3339`]-shaped value.
fn parse_instant(v: &str) -> Result<DateTime<Utc>> {
    let normalized = normalize_rfc3339(v);
    DateTime::parse_from_rfc3339(&normalized)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| anyhow::anyhow!("Could not read '{v}' as a date and time."))
}

/// The daily open/close pair for a free-slot search.
///
/// [`SlotWindow::carve`] walks one local day at a time, so an overnight range
/// ("22:00" to "06:00") describes a day that shuts before it opens and matches
/// nothing at all. Left to itself that returns an empty slot list, which reads
/// as "every court is booked" rather than "those hours can't be searched".
fn working_hours(
    day_start: Option<&str>,
    day_end: Option<&str>,
) -> Result<(Option<NaiveTime>, Option<NaiveTime>)> {
    let open = day_start.map(parse_clock).transpose()?;
    let close = day_end.map(parse_clock).transpose()?;
    if let (Some(o), Some(c)) = (open, close) {
        if o >= c {
            anyhow::bail!(
                "The earliest time of day ({o}) is not before the latest ({c}), so no day has any \
                 bookable hours. An overnight range isn't supported — search a single daytime \
                 range, or leave both blank to search around the clock."
            );
        }
    }
    Ok((open, close))
}

/// A wall-clock time of day: "09:00", "9:00", "17:30:00".
fn parse_clock(v: &str) -> Result<NaiveTime> {
    let v = v.trim();
    NaiveTime::parse_from_str(v, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(v, "%H:%M"))
        .or_else(|_| NaiveTime::parse_from_str(v, "%l:%M %p"))
        .or_else(|_| NaiveTime::parse_from_str(v, "%l%p"))
        .map_err(|_| anyhow::anyhow!("Could not read '{v}' as a time of day (try '09:00')."))
}

/// The configured local offset, falling back to UTC if it's malformed.
fn parse_offset(v: &str) -> FixedOffset {
    DateTime::parse_from_rfc3339(&format!("1970-01-01T00:00:00{v}"))
        .map(|t| *t.offset())
        .unwrap_or_else(|_| FixedOffset::east_opt(0).expect("UTC is a valid offset"))
}

fn local_instant(
    offset: &FixedOffset,
    date: chrono::NaiveDate,
    time: NaiveTime,
) -> Option<DateTime<Utc>> {
    offset
        .from_local_datetime(&date.and_time(time))
        .single()
        .map(|t| t.with_timezone(&Utc))
}

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
        assert_eq!(
            normalize_rfc3339("2026-07-05T09:00:00Z"),
            "2026-07-05T09:00:00Z"
        );
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
        assert_eq!(
            normalize_rfc3339("2026-07-05T09:00"),
            "2026-07-05T09:00:00+08:00"
        );
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
        assert_eq!(
            normalize_rfc3339("2026-07-05 09:00:00"),
            "2026-07-05T09:00:00+08:00"
        );
        assert_eq!(
            normalize_rfc3339("07/05/2026 3:00 PM"),
            "2026-07-05T15:00:00+08:00"
        );
        assert_eq!(
            normalize_rfc3339("July 5, 2026"),
            "2026-07-05T00:00:00+08:00"
        );
        // Unix seconds resolve to an absolute UTC instant
        assert_eq!(normalize_rfc3339("1783213200"), "2026-07-05T01:00:00Z");
    }

    #[test]
    fn date_only_values_become_all_day_events() {
        assert_eq!(
            event_time("2026-07-05", "Asia/Manila"),
            json!({"date": "2026-07-05"})
        );
        assert_eq!(
            event_time("July 5, 2026", "Asia/Manila"),
            json!({"date": "2026-07-05"})
        );
        assert_eq!(
            event_time("07/05/2026", "Asia/Manila"),
            json!({"date": "2026-07-05"})
        );
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
        assert_eq!(
            fix_all_day_end("2026-07-05T09:00:00", "2026-07-05T09:00:00"),
            None
        );
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
    fn rsvp_accepts_everyday_words_and_rejects_guesses() {
        assert_eq!(rsvp_status("yes").unwrap(), "accepted");
        assert_eq!(rsvp_status(" Accepted ").unwrap(), "accepted");
        assert_eq!(rsvp_status("no").unwrap(), "declined");
        assert_eq!(rsvp_status("maybe").unwrap(), "tentative");
        // An ambiguous answer must fail rather than default to "accepted".
        assert!(rsvp_status("probably").is_err());
        assert!(rsvp_status("").is_err());
    }

    #[test]
    fn extras_write_only_the_fields_that_were_set() {
        let mut body = json!({ "summary": "Standup" });
        EventExtras::default().apply(&mut body);
        assert_eq!(body, json!({ "summary": "Standup" }));

        let mut body = json!({});
        EventExtras {
            // Blank strings are what a workflow node sends for an untouched
            // dropdown; writing them through would blank the real value.
            color_id: Some(""),
            transparency: Some("transparent"),
            guests_can_modify: Some(false),
            ..Default::default()
        }
        .apply(&mut body);
        assert_eq!(
            body,
            json!({ "transparency": "transparent", "guestsCanModify": false })
        );
    }

    #[test]
    fn reminder_override_wins_over_calendar_defaults() {
        let mut body = json!({});
        EventExtras {
            reminder_minutes: Some(15),
            reminder_method: Some("email"),
            ..Default::default()
        }
        .apply(&mut body);
        assert_eq!(
            body["reminders"],
            json!({ "useDefault": false, "overrides": [{ "method": "email", "minutes": 15 }] })
        );

        // Google rejects a payload carrying overrides *and* useDefault:true, so
        // asking for the defaults must drop the override entirely.
        let mut body = json!({});
        EventExtras {
            reminder_minutes: Some(15),
            use_default_reminders: Some(true),
            ..Default::default()
        }
        .apply(&mut body);
        assert_eq!(body["reminders"], json!({ "useDefault": true }));

        // 0 means "alert at the start time", not "no reminder".
        let mut body = json!({});
        EventExtras {
            reminder_minutes: Some(0),
            ..Default::default()
        }
        .apply(&mut body);
        assert_eq!(body["reminders"]["overrides"][0]["minutes"], json!(0));
        assert_eq!(body["reminders"]["overrides"][0]["method"], json!("popup"));
    }

    #[test]
    fn times_of_day_parse_in_the_shapes_people_type() {
        assert_eq!(
            parse_clock("09:00").unwrap(),
            NaiveTime::from_hms_opt(9, 0, 0).unwrap()
        );
        assert_eq!(
            parse_clock(" 9:30 ").unwrap(),
            NaiveTime::from_hms_opt(9, 30, 0).unwrap()
        );
        assert_eq!(
            parse_clock("17:15:30").unwrap(),
            NaiveTime::from_hms_opt(17, 15, 30).unwrap()
        );
        assert!(parse_clock("lunchtime").is_err());
    }

    #[test]
    fn offsets_fall_back_to_utc_when_malformed() {
        assert_eq!(parse_offset("+08:00").local_minus_utc(), 8 * 3600);
        assert_eq!(parse_offset("-05:00").local_minus_utc(), -5 * 3600);
        assert_eq!(parse_offset("nonsense").local_minus_utc(), 0);
    }

    /// A slot search over one +08:00 day, 09:00–17:00, wanting 60 minutes.
    fn carve_one_day(busy: &[(&str, &str)], duration_mins: i64) -> Vec<Value> {
        let offset = parse_offset("+08:00");
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let window = SlotWindow {
            from: at("2026-08-17T00:00:00+08:00"),
            to: at("2026-08-18T00:00:00+08:00"),
            duration: chrono::Duration::minutes(duration_mins),
            offset,
            open: Some(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
            close: Some(NaiveTime::from_hms_opt(17, 0, 0).unwrap()),
            skip_weekends: false,
            max_slots: 10,
        };
        let merged: Vec<_> = busy.iter().map(|(s, e)| (at(s), at(e))).collect();
        window.carve(&merged)
    }

    #[test]
    fn free_slots_are_the_gaps_between_meetings() {
        let slots = carve_one_day(
            &[
                ("2026-08-17T10:00:00+08:00", "2026-08-17T11:00:00+08:00"),
                ("2026-08-17T13:00:00+08:00", "2026-08-17T14:30:00+08:00"),
            ],
            60,
        );
        let times: Vec<&str> = slots.iter().map(|s| s["start"].as_str().unwrap()).collect();
        assert_eq!(
            times,
            vec![
                "2026-08-17T09:00:00+08:00",
                "2026-08-17T11:00:00+08:00",
                "2026-08-17T14:30:00+08:00",
            ]
        );
        // 11:00–13:00 is the longest gap; the day closes at 17:00.
        assert_eq!(slots[1]["availableMinutes"], json!(120));
        assert_eq!(slots[2]["availableMinutes"], json!(150));
        assert_eq!(slots[0]["weekday"], json!("Monday"));
    }

    #[test]
    fn a_change_is_classified_from_the_event_itself() {
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let since = at("2026-08-12T09:00:00+08:00");

        // Cancelled wins outright: a deleted event still carries its old
        // created stamp, so checking `created` first would call it "updated".
        let cancelled = json!({ "status": "cancelled", "created": "2026-08-01T10:00:00Z" });
        assert_eq!(change_type(&cancelled, since), "cancelled");

        // Created after the cursor → genuinely new.
        let fresh = json!({ "status": "confirmed", "created": "2026-08-12T10:30:00+08:00" });
        assert_eq!(change_type(&fresh, since), "created");

        // Created before the cursor but modified since → an edit.
        let edited = json!({ "status": "confirmed", "created": "2026-07-30T08:00:00+08:00" });
        assert_eq!(change_type(&edited, since), "updated");

        // No created stamp at all is an edit, not a spurious "created".
        assert_eq!(change_type(&json!({}), since), "updated");
    }

    #[test]
    fn sharing_scopes_are_built_from_plain_email_addresses() {
        assert_eq!(acl_rule_id("sam@example.com"), "user:sam@example.com");
        assert_eq!(acl_rule_id("  sam@example.com  "), "user:sam@example.com");
        // "Everyone" has its own scope with no value attached.
        assert_eq!(acl_rule_id("default"), "default");
        assert_eq!(acl_rule_id("public"), "default");
        // An already-qualified scope is left alone rather than double-prefixed.
        assert_eq!(
            acl_rule_id("group:team@example.com"),
            "group:team@example.com"
        );
        assert_eq!(acl_rule_id("domain:example.com"), "domain:example.com");
        // A colon that isn't a scope prefix is still just an address.
        assert_eq!(acl_rule_id("weird:name@x.com"), "user:weird:name@x.com");
    }

    #[test]
    fn every_offered_access_level_is_one_google_accepts() {
        // The node's dropdown is built from ACL_ROLES, so a typo here would ship
        // a choice the API rejects only once someone picks it.
        let names: Vec<&str> = ACL_ROLES.iter().map(|(r, _)| *r).collect();
        assert_eq!(names, vec!["freeBusyReader", "reader", "writer", "owner"]);
        assert!(ACL_ROLES.iter().all(|(_, label)| !label.is_empty()));
    }

    #[test]
    fn an_overnight_working_day_is_refused_rather_than_returning_nothing() {
        let err = working_hours(Some("22:00"), Some("06:00"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("overnight") || err.contains("earliest"),
            "got: {err}"
        );
        // Equal times leave a zero-length day — same silent-empty trap.
        assert!(working_hours(Some("09:00"), Some("09:00")).is_err());
        // The ordinary cases still pass through, including one-sided limits.
        assert!(working_hours(Some("09:00"), Some("17:00")).is_ok());
        assert!(working_hours(None, Some("17:00")).is_ok());
        assert!(working_hours(None, None).unwrap() == (None, None));
    }

    #[test]
    fn gaps_shorter_than_the_meeting_are_not_offered() {
        // 30 free minutes between the two meetings, but an hour was asked for.
        let slots = carve_one_day(
            &[
                ("2026-08-17T09:00:00+08:00", "2026-08-17T11:00:00+08:00"),
                ("2026-08-17T11:30:00+08:00", "2026-08-17T17:00:00+08:00"),
            ],
            60,
        );
        assert!(slots.is_empty(), "expected no slots, got {slots:?}");

        // The same day does offer that gap to a 30-minute meeting.
        let slots = carve_one_day(
            &[
                ("2026-08-17T09:00:00+08:00", "2026-08-17T11:00:00+08:00"),
                ("2026-08-17T11:30:00+08:00", "2026-08-17T17:00:00+08:00"),
            ],
            30,
        );
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0]["start"], json!("2026-08-17T11:00:00+08:00"));
    }

    #[test]
    fn busy_blocks_outside_working_hours_do_not_shrink_the_day() {
        // An overnight block ending at 08:00 and an evening one starting at
        // 19:00 both sit outside 09:00-17:00 and must leave the day whole.
        let slots = carve_one_day(
            &[
                ("2026-08-16T22:00:00+08:00", "2026-08-17T08:00:00+08:00"),
                ("2026-08-17T19:00:00+08:00", "2026-08-17T21:00:00+08:00"),
            ],
            60,
        );
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0]["start"], json!("2026-08-17T09:00:00+08:00"));
        assert_eq!(slots[0]["availableMinutes"], json!(480));
    }

    #[test]
    fn weekends_are_skipped_and_each_day_is_searched() {
        let offset = parse_offset("+08:00");
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let window = SlotWindow {
            // Friday 2026-08-14 through Tuesday 2026-08-18.
            from: at("2026-08-14T00:00:00+08:00"),
            to: at("2026-08-19T00:00:00+08:00"),
            duration: chrono::Duration::minutes(60),
            offset,
            open: Some(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
            close: Some(NaiveTime::from_hms_opt(17, 0, 0).unwrap()),
            skip_weekends: true,
            max_slots: 10,
        };
        let slots = window.carve(&[]);
        let days: Vec<&str> = slots
            .iter()
            .map(|s| s["weekday"].as_str().unwrap())
            .collect();
        assert_eq!(days, vec!["Friday", "Monday", "Tuesday"]);
    }

    #[test]
    fn max_slots_caps_the_answer() {
        let offset = parse_offset("+08:00");
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let window = SlotWindow {
            from: at("2026-08-17T00:00:00+08:00"),
            to: at("2026-08-31T00:00:00+08:00"),
            duration: chrono::Duration::minutes(30),
            offset,
            open: None,
            close: None,
            skip_weekends: false,
            max_slots: 3,
        };
        assert_eq!(window.carve(&[]).len(), 3);
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
