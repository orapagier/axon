use crate::auth::access_token;
use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use serde_json::{json, Value};

const BASE: &str = "https://meet.googleapis.com/v2";

// ── Conference Records ────────────────────────────────────────────────────────

/// List past conference records (completed Meet calls) for the authenticated user.
/// `filter` supports `start_time` and `end_time` fields, e.g.:
///   `"startTime>\"2024-01-01T00:00:00Z\" AND endTime<\"2024-06-01T00:00:00Z\""`
pub async fn list_conference_records(
    state: &AppState,
    max_results: u32,
    filter: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut params = vec![("pageSize", max_results.to_string())];
    if let Some(f) = filter {
        params.push(("filter", f.to_owned()));
    }

    let resp: Value = state
        .client
        .get(format!("{BASE}/conferenceRecords"))
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Get a single conference record.
/// `conference_record_name` format: "conferenceRecords/XXXXXXXXXXXX"
pub async fn get_conference_record(
    state: &AppState,
    conference_record_name: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{conference_record_name}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

// ── Participants ──────────────────────────────────────────────────────────────

/// List all participants in a conference record.
pub async fn list_participants(
    state: &AppState,
    conference_record_name: &str,
    max_results: u32,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{conference_record_name}/participants"))
        .bearer_auth(&tok)
        .query(&[("pageSize", max_results.to_string())])
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Get a specific participant's session details.
/// `participant_name` format: "conferenceRecords/XXX/participants/YYY"
pub async fn get_participant(state: &AppState, participant_name: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{participant_name}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// List all sessions for a participant (a single participant can have multiple
/// sessions if they joined/left/rejoined during the call).
pub async fn list_participant_sessions(
    state: &AppState,
    participant_name: &str,
    max_results: u32,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{participant_name}/participantSessions"))
        .bearer_auth(&tok)
        .query(&[("pageSize", max_results.to_string())])
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

// ── Recordings ────────────────────────────────────────────────────────────────

/// List all recordings for a conference record.
/// Each recording contains a `driveDestination` with the Google Drive file ID
/// that you can download using the Drive module.
pub async fn list_recordings(
    state: &AppState,
    conference_record_name: &str,
    max_results: u32,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{conference_record_name}/recordings"))
        .bearer_auth(&tok)
        .query(&[("pageSize", max_results.to_string())])
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Get a specific recording.
/// `recording_name` format: "conferenceRecords/XXX/recordings/YYY"
pub async fn get_recording(state: &AppState, recording_name: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{recording_name}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

// ── Transcripts ───────────────────────────────────────────────────────────────

/// List all transcripts for a conference record.
/// Each transcript contains a `docsDestination` with the Google Doc ID
/// that you can read using the Docs module.
pub async fn list_transcripts(
    state: &AppState,
    conference_record_name: &str,
    max_results: u32,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{conference_record_name}/transcripts"))
        .bearer_auth(&tok)
        .query(&[("pageSize", max_results.to_string())])
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Get a specific transcript.
/// `transcript_name` format: "conferenceRecords/XXX/transcripts/YYY"
pub async fn get_transcript(state: &AppState, transcript_name: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{transcript_name}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// List individual transcript entries (utterances) for a transcript.
/// Returns time-stamped text snippets per speaker.
pub async fn list_transcript_entries(
    state: &AppState,
    transcript_name: &str,
    max_results: u32,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{transcript_name}/entries"))
        .bearer_auth(&tok)
        .query(&[("pageSize", max_results.to_string())])
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Convenience: given a conference record, fetch all transcript entries across
/// all transcripts and return them as a flat list ordered by start time.
/// Useful for getting a clean, readable summary of what was said in a meeting.
pub async fn get_full_transcript_text(
    state: &AppState,
    conference_record_name: &str,
) -> Result<Value> {
    let transcripts = list_transcripts(state, conference_record_name, 10).await?;
    let mut all_entries: Vec<Value> = Vec::new();

    if let Some(ts) = transcripts["transcripts"].as_array() {
        for transcript in ts {
            if let Some(name) = transcript["name"].as_str() {
                let entries = list_transcript_entries(state, name, 1000).await?;
                if let Some(arr) = entries["transcriptEntries"].as_array() {
                    all_entries.extend(arr.iter().cloned());
                }
            }
        }
    }

    // Sort by startTime so entries are in chronological order
    all_entries.sort_by(|a, b| {
        let ta = a["startTime"].as_str().unwrap_or("");
        let tb = b["startTime"].as_str().unwrap_or("");
        ta.cmp(tb)
    });

    let count = all_entries.len();
    Ok(json!({
        "conferenceRecord": conference_record_name,
        "entries": all_entries,
        "count": count,
    }))
}
