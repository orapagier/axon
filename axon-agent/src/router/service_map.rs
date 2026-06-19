//! Single source of truth for confusable paired services (Google vs Microsoft).
//!
//! Previously the same "don't mix `gcal_*` with `mscal_*`" knowledge was
//! hand-copied into three places — the pre-execution corrector and the quality
//! gate (both in the agent loop) and the tool-router prompt. Editing one and
//! forgetting another caused silent drift. All three now derive from
//! [`SERVICE_PAIRS`]; add a pair once and every site stays in sync.

/// One confusable pair: side A (e.g. Microsoft) vs side B (e.g. Google).
pub struct ServicePair {
    /// Keywords that mean the user wants side A.
    pub a_keywords: &'static [&'static str],
    /// Tool-name prefix for side A (e.g. `"mscal_"`).
    pub a_prefix: &'static str,
    /// Human label for side A (e.g. `"Microsoft Calendar"`).
    pub a_label: &'static str,
    pub b_keywords: &'static [&'static str],
    pub b_prefix: &'static str,
    pub b_label: &'static str,
}

/// The canonical list. Every disambiguation site reads from this.
pub static SERVICE_PAIRS: &[ServicePair] = &[
    ServicePair {
        a_keywords: &["mscal", "microsoft calendar", "outlook calendar", "ms cal"],
        a_prefix: "mscal_",
        a_label: "Microsoft Calendar",
        b_keywords: &["gcal", "google calendar"],
        b_prefix: "gcal_",
        b_label: "Google Calendar",
    },
    ServicePair {
        a_keywords: &["outlook", "outlook email", "microsoft email", "ms email"],
        a_prefix: "outlook_",
        a_label: "Outlook",
        b_keywords: &["gmail", "google email", "google mail"],
        b_prefix: "gmail_",
        b_label: "Gmail",
    },
    ServicePair {
        a_keywords: &["onedrive", "one drive", "microsoft drive", "ms drive"],
        a_prefix: "onedrive_",
        a_label: "OneDrive",
        b_keywords: &["gdrive", "google drive"],
        b_prefix: "gdrive_",
        b_label: "Google Drive",
    },
    ServicePair {
        a_keywords: &["mscontacts", "microsoft contacts", "outlook contacts"],
        a_prefix: "mscontacts_",
        a_label: "Microsoft Contacts",
        b_keywords: &["gcon", "google contacts", "google people"],
        b_prefix: "gcon_",
        b_label: "Google Contacts",
    },
];

/// Directional mismatch rules for the post-hoc quality gate: each entry is
/// (keywords the user used, the *wrong* prefix to watch for, correction text).
/// Both directions of every pair are emitted.
pub fn mismatch_rules() -> Vec<(&'static [&'static str], &'static str, String)> {
    let mut rules = Vec::with_capacity(SERVICE_PAIRS.len() * 2);
    for p in SERVICE_PAIRS {
        rules.push((
            p.a_keywords,
            p.b_prefix,
            format!(
                "{} ({}*) not {} ({}*)",
                p.a_label, p.a_prefix, p.b_label, p.b_prefix
            ),
        ));
        rules.push((
            p.b_keywords,
            p.a_prefix,
            format!(
                "{} ({}*) not {} ({}*)",
                p.b_label, p.b_prefix, p.a_label, p.a_prefix
            ),
        ));
    }
    rules
}

/// The SERVICE DISAMBIGUATION block injected into the tool-router prompt.
pub fn router_disambiguation_block() -> String {
    let mut out = String::from("SERVICE DISAMBIGUATION (CRITICAL):\n");
    for p in SERVICE_PAIRS {
        out.push_str(&format!(
            "- If user says {} → select ONLY {}* tools, NEVER {}*\n",
            quote_list(p.a_keywords),
            p.a_prefix,
            p.b_prefix
        ));
        out.push_str(&format!(
            "- If user says {} → select ONLY {}* tools, NEVER {}*\n",
            quote_list(p.b_keywords),
            p.b_prefix,
            p.a_prefix
        ));
    }
    out.push_str(
        "- If user says 'todo' or 'tasks' → decide based on context: if Google mentioned use gtasks_*, else list BOTH options if ambiguous.\n",
    );
    out
}

fn quote_list(items: &[&str]) -> String {
    items
        .iter()
        .map(|s| format!("'{}'", s))
        .collect::<Vec<_>>()
        .join(" or ")
}
