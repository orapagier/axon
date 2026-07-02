use crate::config::RuntimeSettings;
use crate::providers::types::Message;
use crate::router::{call_llm, SharedRouter};

pub async fn parse_schedule(
    human: &str,
    router: SharedRouter,
    settings: &RuntimeSettings,
) -> anyhow::Result<String> {
    let parts: Vec<&str> = human.split_whitespace().collect();
    if parts.len() == 6 && human.contains('*') {
        let is_cron_like = parts.iter().all(|p| {
            p.chars().all(|c| {
                c.is_ascii_digit()
                    || c == '*'
                    || c == '/'
                    || c == '-'
                    || c == ','
                    || c.is_ascii_alphabetic()
            })
        });
        if is_cron_like {
            return Ok(human.to_string());
        }
    }

    if let Some(s) = quick_parse(human) {
        return Ok(s.to_string());
    }
    let offset = settings.agent_utc_offset();
    let now_local = chrono::Utc::now().with_timezone(&offset);
    let prompt = format!(
        "[CURRENT TIME: {} (UTC{})]\nConvert to a 6-field cron (sec min hour dom month dow). Reply ONLY with the cron string, nothing else.\nExamples:\n'every minute'->'0 * * * * *', 'every 5 minutes'->'0 */5 * * * *',\n'every hour'->'0 0 * * * *', 'daily at 9am'->'0 0 9 * * *',\n'every Monday at 9am'->'0 0 9 * * MON'\n\nSchedule: {}",
        now_local.format("%A, %Y-%m-%d %H:%M:%S"),
        offset,
        human
    );
    let (resp, _, _tier) = call_llm(
        &[Message::user(&prompt)],
        "Convert schedules to 6-field cron expressions. Reply only with the cron string.",
        &[],
        Some(20),
        "router",
        router,
        settings,
        None,
    )
    .await?;
    let text = resp.text_content();
    let text = if let Some(idx) = text.rfind("</think>") {
        text[idx + "</think>".len()..].to_string()
    } else {
        text.to_string()
    };

    for line in text.lines() {
        let line = line
            .trim()
            .trim_matches('`')
            .trim_matches('"')
            .trim_matches('\'');
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 6 {
            let is_cron_like = parts.iter().all(|p| {
                p.chars().all(|c| {
                    c.is_ascii_digit()
                        || c == '*'
                        || c == '/'
                        || c == '-'
                        || c == ','
                        || c.is_ascii_alphabetic()
                })
            });
            if is_cron_like && (line.contains('*') || line.chars().any(|c| c.is_ascii_digit())) {
                return Ok(line.to_string());
            }
        }
    }

    anyhow::bail!("Invalid cron from LLM: '{}'", text.trim())
}

fn quick_parse(s: &str) -> Option<&'static str> {
    let s = s.to_lowercase();
    let s = s.trim();

    // Check for explicit time indicators to avoid wrong hardcoding
    let has_time = s.contains(" at ")
        || s.contains(" am")
        || s.contains(" pm")
        || s.contains(':')
        || s.contains(" clock");

    if s.contains("every minute") {
        return Some("0 * * * * *");
    }
    if s.contains("every 5 min") {
        return Some("0 */5 * * * *");
    }
    if s.contains("every 10 min") {
        return Some("0 */10 * * * *");
    }
    if s.contains("every 15 min") {
        return Some("0 */15 * * * *");
    }
    if s.contains("every 30 min") {
        return Some("0 */30 * * * *");
    }
    if s.contains("every hour") || s == "hourly" {
        return Some("0 0 * * * *");
    }
    if s.contains("every 2 hour") {
        return Some("0 0 */2 * * *");
    }
    if s.contains("every 3 hour") {
        return Some("0 0 */3 * * *");
    }
    if s.contains("every 4 hour") {
        return Some("0 0 */4 * * *");
    }
    if s.contains("every 5 hour") {
        return Some("0 0 */5 * * *");
    }
    if s.contains("every 6 hour") {
        return Some("0 0 */6 * * *");
    }
    if s.contains("every 7 hour") {
        return Some("0 0 */7 * * *");
    }
    if s.contains("every 8 hour") {
        return Some("0 0 */8 * * *");
    }
    if s.contains("every 9 hour") {
        return Some("0 0 */9 * * *");
    }
    if s.contains("every 10 hour") {
        return Some("0 0 */10 * * *");
    }
    if s.contains("every 11 hour") {
        return Some("0 0 */11 * * *");
    }
    if s.contains("every 12 hour") {
        return Some("0 0 */12 * * *");
    }

    // Only use hardcoded 9am defaults if NO specific time was mentioned
    if !has_time {
        if s.contains("daily") || s.contains("every day") {
            return Some("0 0 9 * * *");
        }
        if s.contains("every monday") {
            return Some("0 0 9 * * MON");
        }
        if s.contains("every tuesday") {
            return Some("0 0 9 * * TUE");
        }
        if s.contains("every wednesday") {
            return Some("0 0 9 * * WED");
        }
        if s.contains("every thursday") {
            return Some("0 0 9 * * THU");
        }
        if s.contains("every friday") {
            return Some("0 0 9 * * FRI");
        }
        if s.contains("weekday") {
            return Some("0 0 9 * * MON-FRI");
        }
        if s.contains("weekend") {
            return Some("0 0 9 * * SAT,SUN");
        }
        if s.contains("weekly") {
            return Some("0 0 9 * * MON");
        }
        if s.contains("monthly") {
            return Some("0 0 9 1 * *");
        }
    }
    None
}
