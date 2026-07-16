use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// No action taken yet this session.
    #[default]
    Idle,
    /// Blocked once with a /record prompt; next Stop will check condukt.
    RecordRequested,
    /// In the condukt loop (auto ≤4 times, ask user ≥5 times).
    Continuing,
    /// All condukt tasks are done; no more blocking this session.
    Done,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionState {
    pub phase: Phase,
    /// How many times we have prompted to run /condukt this session.
    #[serde(default)]
    pub condukt_prompts: u32,
    /// How many times we have prompted to run /backlog this session.
    #[serde(default)]
    pub backlog_prompts: u32,
    /// Whether the Tier 2 delegation-record advisory has already fired this
    /// session (dedup — fires at most once). `#[serde(default)]` keeps older
    /// on-disk state files (without this field) deserializable.
    #[serde(default)]
    pub delegation_audit_warned: bool,
}

fn state_path(state_dir: &Path, session_id: &str) -> PathBuf {
    // `session_id` originates from hook input; sanitise it so it stays a single
    // component under `state_dir` and cannot traverse out via `../`.
    state_dir.join(format!(
        "{}.json",
        harness_core::store::safe_session(session_id)
    ))
}

pub fn load(state_dir: &Path, session_id: &str) -> SessionState {
    harness_core::store::load_json(&state_path(state_dir, session_id))
}

pub fn save(state_dir: &Path, session_id: &str, s: &SessionState) {
    harness_core::store::save_json(&state_path(state_dir, session_id), s);
}

/// Path of the "resume /flow after /compact" marker for a session, keyed on the
/// (sanitised) session id under `state_dir`. Mirrors ctxrot's
/// `<state_dir>/<safe>.distilled` idiom: PreCompact drops it, the next
/// UserPromptSubmit consumes it. `safe_session` guarantees the id stays a single
/// component (no `../` traversal).
pub fn resume_marker_path(state_dir: &Path, session_id: &str) -> PathBuf {
    state_dir.join(format!(
        "{}.resume-flow",
        harness_core::store::safe_session(session_id)
    ))
}

/// Drop the resume-flow marker for this session (best-effort; a failed write just
/// means no auto-resume next turn — never breaks the compaction).
pub fn write_resume_marker(state_dir: &Path, session_id: &str) {
    let _ = std::fs::create_dir_all(state_dir);
    let _ = std::fs::write(resume_marker_path(state_dir, session_id), b"1");
}

/// Consume (delete) this session's resume-flow marker, returning `true` iff it
/// existed. Deleting on read makes re-injection fire exactly once per `/compact`
/// (idempotent): a non-existent marker returns `false` with no side effect.
pub fn consume_resume_marker(state_dir: &Path, session_id: &str) -> bool {
    // remove_file → Ok only when a file was actually removed; NotFound → Err →
    // false. This avoids an exists()+remove TOCTOU and is the single "consume".
    std::fs::remove_file(resume_marker_path(state_dir, session_id)).is_ok()
}
