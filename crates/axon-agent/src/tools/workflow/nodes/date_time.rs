//! Date & Time — Task 2.1 (*Chronon*). A thin config layer over
//! `axon_core::flexidate` (which already reconciles every datetime shape the
//! calendar integrations feed it) plus `chrono`/`chrono-tz` for the arithmetic,
//! formatting, and zone conversion. It removes the "drop to a JavaScript node
//! just to reformat a date" tax.
//!
//! Five operations, one per `operation` config key:
//!   - `getCurrentDate` — emit "now" in a timezone (optionally date-only).
//!   - `format`         — parse a value and render it (presets or a custom
//!                        strftime string), optionally converting timezone.
//!   - `addSubtract`    — shift a value by a duration (calendar-aware for
//!                        months/quarters/years).
//!   - `diff`           — the amount of time between two dates, in one unit.
//!   - `extract`        — pull one component (year, month, weekday, …) out.
//!
//! Input values arrive already expression-resolved by `interpolate_config`, so
//! a bare `$node[...]` reference keeps its JSON type — a Unix timestamp is a
//! number, a formatted string is a string; `flexidate::parse_flexible_value`
//! handles both. Output mirrors Soma: the result lands under `outputField`, and
//! `includeInputFields` decides whether the incoming item's other fields ride
//! along.

use crate::tools::workflow::{val_to_number, val_to_string};
use axon_core::flexidate::{self, FlexiDateTime};
use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Datelike, Duration, Months, SecondsFormat, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::{json, Map, Value};

/// Resolve the timezone config key to an IANA zone, defaulting to Axon's
/// configured default (Asia/Manila unless overridden).
fn resolve_tz(config: &Value, key: &str) -> Result<Tz, String> {
    let name = config
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(flexidate::default_tz);
    name.parse::<Tz>()
        .map_err(|_| format!("Unknown timezone: {name}"))
}

/// Anchor a parsed [`FlexiDateTime`] into a concrete instant in `tz`. A zoned
/// value converts (same instant, new wall clock); a naive or date-only value is
/// interpreted *as* `tz` wall clock. DST gaps fall back to treating the wall
/// clock as UTC so parsing never silently drops the value.
fn anchor(fd: FlexiDateTime, tz: &Tz) -> DateTime<Tz> {
    let from_naive = |ndt: chrono::NaiveDateTime| {
        tz.from_local_datetime(&ndt)
            .earliest()
            .unwrap_or_else(|| Utc.from_utc_datetime(&ndt).with_timezone(tz))
    };
    match fd {
        FlexiDateTime::Zoned(dt) => dt.with_timezone(tz),
        FlexiDateTime::Naive(ndt) => from_naive(ndt),
        FlexiDateTime::DateOnly(d) => from_naive(d.and_hms_opt(0, 0, 0).unwrap()),
    }
}

/// Parse a config value into an instant in `tz`, with messages that name the
/// offending value so a misconfigured field is obvious on the canvas.
fn parse_in_tz(config: &Value, key: &str, tz: &Tz) -> Result<DateTime<Tz>, String> {
    let v = config.get(key).cloned().unwrap_or(Value::Null);
    if v.is_null() || matches!(&v, Value::String(s) if s.trim().is_empty()) {
        return Err(format!("`{key}` is empty — provide a date"));
    }
    let fd = flexidate::parse_flexible_value(&v)
        .ok_or_else(|| format!("Could not parse `{key}` as a date: {}", val_to_string(&v)))?;
    Ok(anchor(fd, tz))
}

/// Add `m` calendar months (negative subtracts), preserving day-of-month where
/// the target month allows it (chrono clamps 31 → 30/28).
fn shift_months(dt: &DateTime<Tz>, m: i64) -> Option<DateTime<Tz>> {
    if m >= 0 {
        dt.checked_add_months(Months::new(m as u32))
    } else {
        dt.checked_sub_months(Months::new(m.unsigned_abs() as u32))
    }
}

/// Full calendar months from `start` to `end` (signed). "Full" means a partial
/// trailing month doesn't count: Jan 31 → Feb 28 is 0 months, Jan 15 → Feb 15
/// is 1. Used by `diff` for the months/years units where a duration-based count
/// would be misleading.
fn full_months_between(start: &DateTime<Tz>, end: &DateTime<Tz>) -> i64 {
    if end < start {
        return -full_months_between(end, start);
    }
    let mut m = (end.year() as i64 - start.year() as i64) * 12
        + (end.month() as i64 - start.month() as i64);
    // The crude month delta can overshoot by one when end's day/time hasn't yet
    // reached start's within the final month; back off until start+m <= end.
    while m > 0 {
        match shift_months(start, m) {
            Some(shifted) if &shifted > end => m -= 1,
            _ => break,
        }
    }
    m
}

/// Seconds in one of the duration-based units; None for the calendar units
/// (months/quarters/years) which can't be a fixed number of seconds.
fn unit_seconds(unit: &str) -> Option<f64> {
    Some(match unit {
        "weeks" => 604_800.0,
        "days" => 86_400.0,
        "hours" => 3_600.0,
        "minutes" => 60.0,
        "seconds" => 1.0,
        _ => return None,
    })
}

/// A strftime string chrono can actually render — rejects bad specifiers up
/// front so `.format().to_string()` can't panic on user input.
fn valid_strftime(fmt: &str) -> bool {
    !fmt.is_empty() && !StrftimeItems::new(fmt).any(|it| matches!(it, Item::Error))
}

/// Render an instant per a format preset (or a custom strftime string). Numeric
/// presets return a JSON number; the rest return strings.
fn render(dt: &DateTime<Tz>, format: &str, custom: &str) -> Result<Value, String> {
    let s = |v: String| Ok(Value::String(v));
    match format {
        "iso" => s(dt.to_rfc3339_opts(SecondsFormat::Secs, false)),
        "rfc2822" => s(dt.to_rfc2822()),
        "date" => s(dt.format("%Y-%m-%d").to_string()),
        "time" => s(dt.format("%H:%M:%S").to_string()),
        "datetime" => s(dt.format("%Y-%m-%d %H:%M:%S").to_string()),
        "human" => s(dt.format("%A, %B %-d, %Y at %-I:%M %p").to_string()),
        "unix" => Ok(json!(dt.timestamp())),
        "unixMs" => Ok(json!(dt.timestamp_millis())),
        "custom" => {
            if !valid_strftime(custom) {
                return Err(format!("Invalid custom format string: {custom:?}"));
            }
            s(dt.format(custom).to_string())
        }
        _ => s(dt.to_rfc3339_opts(SecondsFormat::Secs, false)),
    }
}

/// Extract one calendar/clock component as a number. Weekday is ISO (Mon=1..
/// Sun=7); quarter is 1..4.
fn extract_part(dt: &DateTime<Tz>, part: &str) -> Result<Value, String> {
    Ok(match part {
        "year" => json!(dt.year()),
        "month" => json!(dt.month()),
        "day" => json!(dt.day()),
        "hour" => json!(dt.hour()),
        "minute" => json!(dt.minute()),
        "second" => json!(dt.second()),
        "weekday" => json!(dt.weekday().number_from_monday()),
        "dayOfYear" => json!(dt.ordinal()),
        "weekOfYear" => json!(dt.iso_week().week()),
        "quarter" => json!((dt.month() - 1) / 3 + 1),
        other => return Err(format!("Unknown part: {other}")),
    })
}

/// Wrap a computed result: place it under `outputField` (falling back to
/// `default_field`), optionally merged onto the incoming item's other fields.
fn wrap(config: &Value, input: &Value, default_field: &str, result: Value) -> Value {
    let field = config
        .get("outputField")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_field)
        .to_string();
    let include = config
        .get("includeInputFields")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut out: Map<String, Value> = match (include, input) {
        (true, Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    out.insert(field, result);
    Value::Object(out)
}

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("getCurrentDate");
    let tz = resolve_tz(config, "timezone")?;

    match operation {
        "getCurrentDate" => {
            let now = Utc::now().with_timezone(&tz);
            let include_time = config
                .get("includeTime")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let out = if include_time {
                Value::String(now.to_rfc3339_opts(SecondsFormat::Secs, false))
            } else {
                Value::String(now.format("%Y-%m-%d").to_string())
            };
            Ok(wrap(config, input, "currentDate", out))
        }

        "format" => {
            let dt = parse_in_tz(config, "value", &tz)?;
            let format = config
                .get("format")
                .and_then(|v| v.as_str())
                .unwrap_or("iso");
            let custom = config
                .get("customFormat")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let out = render(&dt, format, custom)?;
            Ok(wrap(config, input, "formatted", out))
        }

        "addSubtract" => {
            let dt = parse_in_tz(config, "value", &tz)?;
            let unit = config.get("unit").and_then(|v| v.as_str()).unwrap_or("days");
            let amount = config
                .get("duration")
                .map(|v| val_to_number(v).unwrap_or(0.0))
                .unwrap_or(0.0);
            let subtract = config.get("direction").and_then(|v| v.as_str()) == Some("subtract");
            let signed = if subtract { -amount } else { amount };

            let shifted = match unit {
                "years" => shift_months(&dt, (signed * 12.0).round() as i64),
                "quarters" => shift_months(&dt, (signed * 3.0).round() as i64),
                "months" => shift_months(&dt, signed.round() as i64),
                _ => {
                    let secs = unit_seconds(unit)
                        .ok_or_else(|| format!("Unknown unit: {unit}"))?;
                    dt.checked_add_signed(Duration::milliseconds((signed * secs * 1000.0).round() as i64))
                }
            }
            .ok_or_else(|| "Date arithmetic overflowed".to_string())?;

            let out = Value::String(shifted.to_rfc3339_opts(SecondsFormat::Secs, false));
            Ok(wrap(config, input, "result", out))
        }

        "diff" => {
            let start = parse_in_tz(config, "startDate", &tz)?;
            let end = parse_in_tz(config, "endDate", &tz)?;
            let unit = config.get("unit").and_then(|v| v.as_str()).unwrap_or("days");
            let value = match unit {
                "months" => json!(full_months_between(&start, &end)),
                "quarters" => json!(full_months_between(&start, &end) / 3),
                "years" => json!(full_months_between(&start, &end) / 12),
                _ => {
                    let secs = unit_seconds(unit)
                        .ok_or_else(|| format!("Unknown unit: {unit}"))?;
                    let ms = end.signed_duration_since(start).num_milliseconds() as f64;
                    json!(ms / 1000.0 / secs)
                }
            };
            Ok(wrap(config, input, "diff", value))
        }

        "extract" => {
            let dt = parse_in_tz(config, "value", &tz)?;
            let part = config.get("part").and_then(|v| v.as_str()).unwrap_or("year");
            let out = extract_part(&dt, part)?;
            Ok(wrap(config, input, part, out))
        }

        other => Err(format!("Unknown Date & Time operation: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Format an ISO instant into a plain date, converting into a named zone.
    #[test]
    fn format_date_in_timezone() {
        let cfg = json!({
            "operation": "format",
            "value": "2026-07-05T23:30:00+08:00",
            "format": "date",
            "timezone": "Asia/Manila",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "formatted": "2026-07-05" }));
    }

    // A zoned value converts to a different target zone before formatting.
    #[test]
    fn format_converts_across_zones() {
        let cfg = json!({
            "operation": "format",
            "value": "2026-07-05T09:00:00+08:00",
            "format": "datetime",
            "timezone": "UTC",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "formatted": "2026-07-05 01:00:00" }));
    }

    // Unix presets return JSON numbers, not strings.
    #[test]
    fn format_unix_is_number() {
        let cfg = json!({
            "operation": "format",
            "value": "2026-07-05T01:00:00Z",
            "format": "unix",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "formatted": 1_783_213_200 }));
    }

    // A custom strftime string renders; an invalid one is rejected (not panicked).
    #[test]
    fn format_custom_and_invalid() {
        let ok = execute(
            &json!({
                "operation": "format",
                "value": "2026-07-05T09:00:00+08:00",
                "format": "custom",
                "customFormat": "%Y/%m/%d",
                "timezone": "Asia/Manila",
            }),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(ok, json!({ "formatted": "2026/07/05" }));

        let err = execute(
            &json!({
                "operation": "format",
                "value": "2026-07-05",
                "format": "custom",
                "customFormat": "%Q",
            }),
            &Value::Null,
        );
        assert!(err.is_err(), "invalid strftime should error");
    }

    // Add days lands on the next calendar day, in the value's zone.
    #[test]
    fn add_days() {
        let cfg = json!({
            "operation": "addSubtract",
            "value": "2026-07-05T09:00:00+08:00",
            "direction": "add",
            "duration": 3,
            "unit": "days",
            "timezone": "Asia/Manila",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "result": "2026-07-08T09:00:00+08:00" }));
    }

    // Subtract months is calendar-aware and clamps the day (Mar 31 → Feb 28).
    #[test]
    fn subtract_months_clamps_day() {
        let cfg = json!({
            "operation": "addSubtract",
            "value": "2026-03-31T12:00:00+08:00",
            "direction": "subtract",
            "duration": 1,
            "unit": "months",
            "timezone": "Asia/Manila",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "result": "2026-02-28T12:00:00+08:00" }));
    }

    // Adding a year via the years unit shifts 12 months.
    #[test]
    fn add_years() {
        let cfg = json!({
            "operation": "addSubtract",
            "value": "2026-07-05T00:00:00+08:00",
            "direction": "add",
            "duration": 1,
            "unit": "years",
            "timezone": "Asia/Manila",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "result": "2027-07-05T00:00:00+08:00" }));
    }

    // Diff in hours between two instants (fractional).
    #[test]
    fn diff_hours() {
        let cfg = json!({
            "operation": "diff",
            "startDate": "2026-07-05T09:00:00Z",
            "endDate": "2026-07-05T10:30:00Z",
            "unit": "hours",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "diff": 1.5 }));
    }

    // Diff in full calendar months ignores the partial trailing days.
    #[test]
    fn diff_full_months() {
        let cfg = json!({
            "operation": "diff",
            "startDate": "2026-01-15T00:00:00+08:00",
            "endDate": "2026-04-10T00:00:00+08:00", // not yet the 15th → 2 full months
            "unit": "months",
            "timezone": "Asia/Manila",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "diff": 2 }));
    }

    // Diff in quarters (7 full months → 2 quarters).
    #[test]
    fn diff_quarters() {
        let cfg = json!({
            "operation": "diff",
            "startDate": "2026-01-15T00:00:00+08:00",
            "endDate": "2026-08-20T00:00:00+08:00", // 7 full months
            "unit": "quarters",
            "timezone": "Asia/Manila",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "diff": 2 }));
    }

    // Extract the ISO weekday number (2026-07-05 is a Sunday → 7).
    #[test]
    fn extract_weekday() {
        let cfg = json!({
            "operation": "extract",
            "value": "2026-07-05T12:00:00+08:00",
            "part": "weekday",
            "timezone": "Asia/Manila",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "weekday": 7 }));
    }

    // Extract the quarter (July → Q3).
    #[test]
    fn extract_quarter() {
        let cfg = json!({
            "operation": "extract",
            "value": "2026-07-05",
            "part": "quarter",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "quarter": 3 }));
    }

    // getCurrentDate honors includeTime=false → a bare date string.
    #[test]
    fn current_date_only() {
        let cfg = json!({
            "operation": "getCurrentDate",
            "includeTime": false,
            "timezone": "UTC",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        let date = out["currentDate"].as_str().unwrap();
        // YYYY-MM-DD shape, no time component.
        assert_eq!(date.len(), 10, "expected bare date, got {date}");
        assert!(date.matches('-').count() == 2 && !date.contains('T'));
    }

    // includeInputFields merges the result onto the incoming item.
    #[test]
    fn include_input_fields_merges() {
        let cfg = json!({
            "operation": "extract",
            "value": "2026-07-05",
            "part": "year",
            "includeInputFields": true,
            "outputField": "yr",
        });
        let input = json!({ "keep": "me" });
        let out = execute(&cfg, &input).unwrap();
        assert_eq!(out, json!({ "keep": "me", "yr": 2026 }));
    }

    // A Unix timestamp number parses (flexidate preserves the JSON type).
    #[test]
    fn parses_unix_number_input() {
        let cfg = json!({
            "operation": "format",
            "value": 1_783_213_200,
            "format": "datetime",
            "timezone": "UTC",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out, json!({ "formatted": "2026-07-05 01:00:00" }));
    }

    // An empty value field is a clear error, not a silent default.
    #[test]
    fn empty_value_errors() {
        let cfg = json!({ "operation": "format", "value": "", "format": "iso" });
        assert!(execute(&cfg, &Value::Null).is_err());
    }

    // An unknown timezone is rejected.
    #[test]
    fn unknown_timezone_errors() {
        let cfg = json!({
            "operation": "getCurrentDate",
            "timezone": "Mars/Olympus",
        });
        assert!(execute(&cfg, &Value::Null).is_err());
    }
}
