//! Run-scoped plan state for plan-then-execute.
//!
//! On multi-step tasks the model is told to call the `update_plan` internal
//! tool first with a numbered checklist, then to re-call it (full list,
//! statuses updated) as steps complete. The rendered plan is returned in every
//! `update_plan` tool result, so the current state rides along in the
//! conversation history with no per-iteration system-prompt mutation (which
//! would invalidate provider prompt caches). The loop reminds the model ONCE
//! if it tries to give a final answer while steps are still open.
//!
//! State is keyed by run id and cleared in `finalize` on every exit path.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanStep {
    pub step: String,
    /// "done" checks a step off; anything else (or absent) means open.
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Default)]
struct RunPlan {
    steps: Vec<PlanStep>,
    reminded: bool,
}

static PLANS: Lazy<Mutex<HashMap<String, RunPlan>>> = Lazy::new(|| Mutex::new(HashMap::new()));

pub fn set_steps(run_id: &str, steps: Vec<PlanStep>) {
    let mut g = PLANS.lock().unwrap();
    g.entry(run_id.to_string()).or_default().steps = steps;
}

/// Numbered checklist rendering, e.g. `1. [x] fetch emails`.
pub fn render(run_id: &str) -> Option<String> {
    let g = PLANS.lock().unwrap();
    let p = g.get(run_id)?;
    if p.steps.is_empty() {
        return None;
    }
    let lines: Vec<String> = p
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mark = if s.status.eq_ignore_ascii_case("done") {
                "x"
            } else {
                " "
            };
            format!("{}. [{}] {}", i + 1, mark, s.step)
        })
        .collect();
    Some(format!("[PLAN]\n{}", lines.join("\n")))
}

/// Steps not yet marked done. None when the run never created a plan.
pub fn open_steps(run_id: &str) -> Option<Vec<String>> {
    let g = PLANS.lock().unwrap();
    let p = g.get(run_id)?;
    Some(
        p.steps
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.status.eq_ignore_ascii_case("done"))
            .map(|(i, s)| format!("{}. {}", i + 1, s.step))
            .collect(),
    )
}

/// One-shot latch: true the first time it is called for a run with a plan.
/// Bounds the open-steps reminder to a single correction per run.
pub fn mark_reminded(run_id: &str) -> bool {
    let mut g = PLANS.lock().unwrap();
    match g.get_mut(run_id) {
        Some(p) if !p.reminded => {
            p.reminded = true;
            true
        }
        _ => false,
    }
}

pub fn clear(run_id: &str) {
    PLANS.lock().unwrap().remove(run_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_lifecycle() {
        let rid = "test-run-plan-lifecycle";
        assert!(render(rid).is_none());
        set_steps(
            rid,
            vec![
                PlanStep {
                    step: "fetch emails".into(),
                    status: "done".into(),
                },
                PlanStep {
                    step: "summarize".into(),
                    status: "pending".into(),
                },
            ],
        );
        let rendered = render(rid).unwrap();
        assert!(rendered.contains("1. [x] fetch emails"));
        assert!(rendered.contains("2. [ ] summarize"));
        assert_eq!(open_steps(rid).unwrap(), vec!["2. summarize".to_string()]);
        assert!(mark_reminded(rid));
        assert!(!mark_reminded(rid), "reminder must be one-shot");
        clear(rid);
        assert!(render(rid).is_none());
    }
}
