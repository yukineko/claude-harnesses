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
//! charter, matching condukt's "live next to the repo" convention).
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

/// Load the persisted [`CarveState`], if any. Only a missing file is
/// `Ok(None)` (no carve in flight; the caller initializes one). Any other read
/// error, or a file that does not parse, is an `Err` (undetermined, backlog
/// e3ca0b92): it is never reported as "no carve", so the caller does not
/// re-initialize and overwrite recorded answers it could not read.
/// `compass carve-reset` is the explicit way to discard such a file.
pub fn load(root: &Path) -> Result<Option<CarveState>> {
    let path = state_path(root);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "reading {} (cannot determine the carve state; run `compass carve-reset` to discard it)",
                    path.display()
                )
            })
        }
    };
    serde_json::from_str(&text).map(Some).with_context(|| {
        format!(
            "parsing {} (cannot determine the carve state; run `compass carve-reset` to discard it)",
            path.display()
        )
    })
}

/// Take the exclusive carve-state lock (`carve-state.json.lock`). A read-modify-write
/// of the state (load then [`save`]) must hold this guard across both, or concurrent
/// `compass apply` runs lose each other's answers. Fail-closed: a lock that cannot be
/// acquired is an `Err` (non-zero exit), never an unlocked fallback.
pub(crate) fn lock(root: &Path) -> Result<crate::outcome::StoreLock> {
    crate::outcome::lock_store(&state_path(root))
}

/// Persist `state` to `.compass/carve-state.json`, creating parent dirs. Uses a
/// unique temp name; callers doing load-modify-save hold [`lock`].
pub fn save(root: &Path, state: &CarveState) -> Result<()> {
    let path = state_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(state).context("serializing carve state")?;
    crate::outcome::write_atomic(&path, &json)
}

/// Locked read-modify-write of `.compass/carve-state.json` (backlog 03e655dc).
///
/// Takes [`lock`], then loads the current state, hands it to `f`, and writes
/// the state `f` returns via [`save`], all while the lock is held — so
/// concurrent `compass apply` / `compass evaluate` runs serialize instead of
/// overwriting each other's answers.
///
/// Fail-closed: if the lock cannot be acquired, or the existing file cannot be
/// read or parsed ([`load`] returns `Err`), this returns that `Err` without
/// calling `f` and without writing. Only a missing file reaches `f` as `None`.
/// If `f` returns `Err`, nothing is written. Callers must NOT already hold
/// [`lock`] (the lock is not re-entrant).
pub fn update<T>(
    root: &Path,
    f: impl FnOnce(Option<CarveState>) -> Result<(CarveState, T)>,
) -> Result<T> {
    let _lock = lock(root)?;
    let current = load(root)?;
    let (state, out) = f(current)?;
    save(root, &state)?;
    Ok(out)
}

/// Clear the persisted carve state (start fresh). Idempotent: a missing file is
/// not an error. Takes the carve lock itself so it cannot race a concurrent
/// `apply`/`evaluate` load-modify-save; callers must NOT already hold [`lock`]
/// (the lock is not re-entrant). A lock that cannot be acquired is an `Err`.
pub fn reset(root: &Path) -> Result<()> {
    let _lock = lock(root)?;
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
        let loaded = load(root).expect("load").expect("present");
        assert_eq!(loaded.round, 2);
        assert_eq!(loaded.bundle.fragments.len(), 1);
        assert_eq!(loaded.max_rounds, 4);
    }

    #[test]
    fn load_missing_is_none_and_reset_is_idempotent() {
        let tmp = temp_root();
        let root = tmp.path();
        assert!(load(root).expect("load missing").is_none());
        // reset on a missing file is a no-op, not an error.
        reset(root).expect("reset missing");

        let state = CarveState::new(Bundle::default(), 1);
        save(root, &state).expect("save");
        assert!(load(root).expect("load present").is_some());
        reset(root).expect("reset present");
        assert!(load(root).expect("load after reset").is_none());
    }

    #[test]
    fn update_on_corrupt_state_is_err_and_leaves_file_untouched() {
        let tmp = temp_root();
        let root = tmp.path();
        let path = state_path(root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json").unwrap();

        let mut called = false;
        let res: Result<()> = update(root, |_| {
            called = true;
            Ok((CarveState::new(Bundle::default(), 1), ()))
        });
        assert!(res.is_err(), "corrupt state must be Err, got Ok");
        assert!(!called, "f must not run on an undeterminable state");
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not json");
        // The lock is released even on the error path.
        assert!(!path.with_extension("json.lock").exists());
    }

    #[test]
    fn update_on_unreadable_state_is_err() {
        let tmp = temp_root();
        let root = tmp.path();
        let path = state_path(root);
        // A directory where the file should be: read_to_string fails with a
        // non-NotFound error, which must not be read as "no carve".
        std::fs::create_dir_all(&path).unwrap();
        let mut called = false;
        let res: Result<()> = update(root, |_| {
            called = true;
            Ok((CarveState::new(Bundle::default(), 1), ()))
        });
        assert!(res.is_err(), "unreadable state must be Err, got Ok");
        assert!(!called, "f must not run on an unreadable state");
    }

    #[test]
    fn update_when_lock_held_is_err_and_does_not_write() {
        let tmp = temp_root();
        let root = tmp.path();
        let path = state_path(root);
        let held = crate::outcome::lock_store(&path).expect("take lock");
        let mut called = false;
        let res: Result<()> = update(root, |_| {
            called = true;
            Ok((CarveState::new(Bundle::default(), 1), ()))
        });
        assert!(res.is_err(), "held lock must be Err, got Ok");
        assert!(!called);
        assert!(!path.exists(), "nothing may be written without the lock");
        drop(held);
    }

    #[test]
    fn update_passes_missing_as_none_and_persists_result() {
        let tmp = temp_root();
        let root = tmp.path();
        let got = update(root, |cur| {
            assert!(cur.is_none());
            let mut s = CarveState::new(Bundle::default(), 3);
            s.round = 1;
            Ok((s, 7u32))
        })
        .expect("update");
        assert_eq!(got, 7);
        let round = update(root, |cur| {
            let mut s = cur.expect("present after first update");
            s.round += 1;
            let r = s.round;
            Ok((s, r))
        })
        .expect("update 2");
        assert_eq!(round, 2);
        assert_eq!(load(root).unwrap().unwrap().round, 2);
        let names: Vec<_> = std::fs::read_dir(root.join(".compass"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["carve-state.json".to_string()],
            "no tmp/lock left"
        );
    }

    #[test]
    fn update_err_from_f_writes_nothing() {
        let tmp = temp_root();
        let root = tmp.path();
        let res: Result<()> = update(root, |_| anyhow::bail!("boom"));
        assert!(res.is_err());
        assert!(load(root).unwrap().is_none());
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
