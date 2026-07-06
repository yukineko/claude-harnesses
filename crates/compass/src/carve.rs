//! carve — persistence + JSON view for the interrogate [`CarveState`] (DESIGN
//! §11). The Rust binary NEVER drives the carve loop; the `/compass` SKILL does
//! (`evaluate` → AskUserQuestion → `apply`, repeat). This module only:
//!   - persists [`CarveState`] across the binary's stateless invocations, and
//!   - renders the deterministic `{ open_questions, status, round }` JSON the
//!     skill reads between LLM steps.
//!
//! # State keying / location
//!
//! State is repo-colocated at `.compass/carve-state.json` (same directory as the
//! charter, matching taskprog/condukt's "live next to the repo" convention).
//! One carve is in flight per repo at a time: the skill drives a single
//! `/compass` run start-to-finish, and `carve-reset` clears it for a fresh
//! start. We do not key by session because the skill calls the binary directly
//! (no hook payload / session id is supplied on these subcommands).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use harness_core::interrogate::{CarveState, CarveStatus, OpenQuestion};
use serde::Serialize;

/// `.compass/carve-state.json` under the project root.
pub fn state_path(root: &Path) -> PathBuf {
    root.join(".compass").join("carve-state.json")
}

/// Load the persisted [`CarveState`], if any. A missing file => `None`; a
/// corrupt file is also treated as `None` (the caller re-initializes) so a stale
/// state never wedges a carve.
pub fn load(root: &Path) -> Option<CarveState> {
    let text = std::fs::read_to_string(state_path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Persist `state` to `.compass/carve-state.json`, creating parent dirs.
pub fn save(root: &Path, state: &CarveState) -> Result<()> {
    let path = state_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(state).context("serializing carve state")?;
    // Write to a temp sibling then rename (mirror the outcomes store).
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

/// Clear the persisted carve state (start fresh). Idempotent: a missing file is
/// not an error.
pub fn reset(root: &Path) -> Result<()> {
    let path = state_path(root);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// Stable string form of a [`CarveStatus`] for the JSON view the skill reads.
fn status_str(status: CarveStatus) -> &'static str {
    match status {
        CarveStatus::Open => "open",
        CarveStatus::Resolved => "resolved",
        CarveStatus::Sentinel => "sentinel",
    }
}

/// The deterministic `{ open_questions, status, round }` view the skill reads
/// between LLM steps. `status` is lower-cased ("open" | "resolved" | "sentinel").
#[derive(Debug, Clone, Serialize)]
pub struct CarveView {
    pub open_questions: Vec<OpenQuestion>,
    pub status: &'static str,
    pub round: u32,
}

impl CarveView {
    /// Build a view from the deterministic-floor open questions plus the current
    /// state (status/round). `open` is whatever `evaluate`/`apply` last returned.
    pub fn new(open: Vec<OpenQuestion>, state: &CarveState) -> Self {
        CarveView {
            open_questions: open,
            status: status_str(state.status),
            round: state.round,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::interrogate::{Authority, Bundle, Fragment};

    /// A unique auto-cleaned temp dir. `tempfile` creates it with O_EXCL-style
    /// uniqueness (no pid-collision TOCTOU) and removes it on drop, so tests
    /// never share a path or leak dirs into /tmp.
    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn save_load_round_trips() {
        let tmp = temp_root();
        let root = tmp.path();
        let mut state = CarveState::new(Bundle::default(), 4);
        state.round = 2;
        state.bundle.fragments.push(Fragment {
            text: "decision".to_string(),
            source_path: "interrogate:answer:C3:dod".to_string(),
            authority: Authority::High,
            score: 0,
            anchor: None,
        });

        save(root, &state).expect("save");
        let loaded = load(root).expect("load");
        assert_eq!(loaded.round, 2);
        assert_eq!(loaded.bundle.fragments.len(), 1);
        assert_eq!(loaded.max_rounds, 4);
    }

    #[test]
    fn load_missing_is_none_and_reset_is_idempotent() {
        let tmp = temp_root();
        let root = tmp.path();
        assert!(load(root).is_none());
        // reset on a missing file is a no-op, not an error.
        reset(root).expect("reset missing");

        let state = CarveState::new(Bundle::default(), 1);
        save(root, &state).expect("save");
        assert!(load(root).is_some());
        reset(root).expect("reset present");
        assert!(load(root).is_none());
    }

    #[test]
    fn view_serializes_expected_shape() {
        let state = CarveState::new(Bundle::default(), 4);
        let open = vec![OpenQuestion {
            gate: "C1".to_string(),
            reference: "charter".to_string(),
            gap: "north_star empty".to_string(),
            sources: Vec::new(),
            default: None,
        }];
        let view = CarveView::new(open, &state);
        let json = serde_json::to_value(&view).expect("serialize");
        assert_eq!(json["status"], "open");
        assert_eq!(json["round"], 0);
        assert_eq!(json["open_questions"][0]["gate"], "C1");
    }
}
