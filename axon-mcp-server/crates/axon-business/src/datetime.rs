use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, FixedOffset, Months, NaiveDateTime, Utc};
use serde_json::{json, Value};

/// Parse an ISO 8601 string into a UTC DateTime.
/// If no offset is provided, assumes Asia/Manila (+8:00).
fn parse_dt(s: &str) -> Result<DateTime<Utc>> {
    // Try full RFC3339 first
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    // Try naive datetime (assume Asia/Manila +8:00)
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        let local =
            DateTime::<FixedOffset>::from_naive_utc_and_offset(ndt - Duration::hours(8), offset);
        return Ok(local.with_timezone(&Utc));
    }
    // Try date only (midnight Manila +8:00)
    if let Ok(nd) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let ndt = nd.and_hms_opt(0, 0, 0).unwrap();
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        let local =
            DateTime::<FixedOffset>::from_naive_utc_and_offset(ndt - Duration::hours(8), offset);
        return Ok(local.with_timezone(&Utc));
    }
    Err(anyhow!(
        "Cannot parse datetime: '{s}'. Expected ISO 8601 or YYYY-MM-DD."
    ))
}

/// Minimal tz offset lookup for common timezones (avoids a heavy tz crate).
fn tz_offset(tz: &str) -> Result<FixedOffset> {
    let offset_hours: i32 = match tz.to_uppercase().as_str() {
        "UTC" | "GMT"                       =>  0,
        "EST" | "US/EASTERN"                => -5,
        "EDT"                               => -4,
        "CST" | "US/CENTRAL"                => -6,
        "CDT"                               => -5,
        "MST" | "US/MOUNTAIN"               => -7,
        "MDT"                               => -6,
        "PST" | "US/PACIFIC"                => -8,
        "PDT"                               => -7,
        "AEST" | "AUSTRALIA/SYDNEY"         => 10,
        "AEDT"                              => 11,
        "JST" | "ASIA/TOKYO"                =>  9,
        "CST+8" | "ASIA/SHANGHAI" | "ASIA/MANILA" | "ASIA/SINGAPORE" => 8,
        "IST" | "ASIA/KOLKATA"              =>  0, // handled below as +5:30
        "CET" | "EUROPE/PARIS"              =>  1,
        "CEST"                              =>  2,
        "EET" | "EUROPE/ATHENS"             =>  2,
        "MSK" | "EUROPE/MOSCOW"             =>  3,
        "WIB" | "ASIA/JAKARTA"              =>  7,
        "WITA"| "ASIA/MAKASSAR"             =>  8,
        "WIT" | "ASIA/JAYAPURA"             =>  9,
        "NZST"| "PACIFIC/AUCKLAND"          => 12,
        "BRT" | "AMERICA/SAO_PAULO"         => -3,
        "ART" | "AMERICA/ARGENTINA/BUENOS_AIRES" => -3,
        "WAT" | "AFRICA/LAGOS"              =>  1,
        "EAT" | "AFRICA/NAIROBI"            =>  3,
        "SAST"| "AFRICA/JOHANNESBURG"       =>  2,
        // +5:30 special case
        _ if tz.to_uppercase() == "IST" || tz.contains("KOLKATA") => {
            return FixedOffset::east_opt(19800).ok_or_else(|| anyhow!("bad offset"));
        }
        _ => return Err(anyhow!(
            "Unknown timezone '{tz}'. Use UTC offsets like 'UTC' or common codes like 'PST', 'JST', 'ASIA/MANILA'."
        )),
    };
    FixedOffset::east_opt(offset_hours * 3600).ok_or_else(|| anyhow!("invalid offset for {tz}"))
}

pub fn now(timezone: &str) -> Result<Value> {
    let utc_now = Utc::now();
    let offset = tz_offset(timezone)?;
    let local = utc_now.with_timezone(&offset);
    Ok(json!({
        "utc":      utc_now.to_rfc3339(),
        "local":    local.to_rfc3339(),
        "timezone": timezone,
        "human":    local.format("%A, %B %d %Y at %H:%M:%S").to_string(),
        "date":     local.format("%Y-%m-%d").to_string(),
        "time":     local.format("%H:%M:%S").to_string(),
        "unix":     utc_now.timestamp(),
    }))
}

pub fn convert(datetime: &str, from_tz: &str, to_tz: &str) -> Result<Value> {
    let from_offset = tz_offset(from_tz)?;
    let to_offset = tz_offset(to_tz)?;

    // Parse in from_tz context, then shift to to_tz
    let dt_utc = parse_dt(datetime)?;
    let dt_from = dt_utc.with_timezone(&from_offset);
    let dt_to = dt_utc.with_timezone(&to_offset);

    Ok(json!({
        "input":     datetime,
        "from_tz":   from_tz,
        "to_tz":     to_tz,
        "converted": dt_to.to_rfc3339(),
        "human":     dt_to.format("%A, %B %d %Y at %H:%M:%S %z").to_string(),
        "original":  dt_from.to_rfc3339(),
    }))
}

pub fn diff(start: &str, end: &str) -> Result<Value> {
    let start_dt = parse_dt(start)?;
    let end_dt = parse_dt(end)?;
    let dur = end_dt.signed_duration_since(start_dt);

    let total_secs = dur.num_seconds().abs();
    let total_mins = dur.num_minutes();
    let total_hours = dur.num_hours();
    let total_days = dur.num_days();
    let weeks = total_days / 7;
    let days_rem = total_days % 7;
    let hours_rem = total_hours - total_days * 24;
    let mins_rem = total_mins - total_hours * 60;

    Ok(json!({
        "start":        start,
        "end":          end,
        "is_past":      dur.num_seconds() < 0,
        "total_seconds": total_secs,
        "total_minutes": total_mins.abs(),
        "total_hours":   total_hours.abs(),
        "total_days":    total_days.abs(),
        "breakdown": {
            "weeks": weeks.abs(),
            "days":  days_rem.abs(),
            "hours": hours_rem.abs(),
            "minutes": mins_rem.abs(),
        },
        "human": format!(
            "{} weeks, {} days, {} hours, {} minutes",
            weeks.abs(), days_rem.abs(), hours_rem.abs(), mins_rem.abs()
        ),
    }))
}

pub fn add(datetime: &str, amount: i64, unit: &str) -> Result<Value> {
    let mut dt = parse_dt(datetime)?;
    dt = match unit {
        "minutes" => dt + Duration::minutes(amount),
        "hours" => dt + Duration::hours(amount),
        "days" => dt + Duration::days(amount),
        "weeks" => dt + Duration::weeks(amount),
        "months" => {
            if amount >= 0 {
                dt + Months::new(amount as u32)
            } else {
                dt - Months::new((-amount) as u32)
            }
        }
        other => {
            return Err(anyhow!(
                "Unknown unit '{other}'. Use: minutes|hours|days|weeks|months"
            ))
        }
    };
    Ok(json!({
        "original": datetime,
        "amount":   amount,
        "unit":     unit,
        "result":   dt.to_rfc3339(),
        "date":     dt.format("%Y-%m-%d").to_string(),
        "human":    dt.format("%A, %B %d %Y at %H:%M:%S").to_string(),
    }))
}

pub fn format_dt(datetime: &str, format: &str) -> Result<Value> {
    let dt = parse_dt(datetime)?;
    let formatted = if format == "human" {
        dt.format("%A, %B %d %Y at %H:%M").to_string()
    } else {
        dt.format(format).to_string()
    };
    Ok(json!({ "input": datetime, "format": format, "result": formatted }))
}
