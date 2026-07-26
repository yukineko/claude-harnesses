//! Session-scoped taint marker.
//!
//! A turn that consumed untrusted-provenance content (a WebFetch/WebSearch
//! result, or a `Read` of a file outside the project root) is recorded here so
//! the `gate` subcommand can downgrade write-class tools for the rest of that
//! turn. The marker lives at a `context_state_dir`-style path
//! (`harness_core::store::context_state_dir`'s convention, mirrored here with
//! taintguard's own base dir): `<state base>/<project_key>/<session>/taint.json`.
//!
//! Fail-closed by construction: [`check`] (and its `bool` convenience
//! [`is_tainted`]) treats an unreadable or unparseable marker as tainted, never
//! as clean — a corrupt/unreadable "have we seen untrusted content" record must
//! not be read as "no, we haven't". See [`Check::Undetermined`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use harness_core::boundary;
use harness_core::store::{project_key, safe_session};
use harness_core::verdict::Determination;

/// On-disk shape of the marker file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct TaintState {
    #[serde(default)]
    tainted: bool,
    /// Provenance tags that contributed to the taint (e.g. `"web"`,
    /// `"external-read"`), deduplicated. Purely diagnostic — surfaced in the
    /// gate's `permissionDecisionReason` so the agent knows why.
    #[serde(default)]
    sources: Vec<String>,
}

/// The result of checking this session's taint state — kept three-valued (not
/// collapsed to a bool at this layer) so a caller building a human-facing
/// reason can distinguish "tainted by X" from "could not tell, so treating as
/// tainted" (see CLAUDE.md §3 / `harness_core::verdict::Determination`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// No marker, or a marker that reads `tainted: false`: this turn has not
    /// (yet) consumed untrusted-provenance content.
    Clean,
    /// Tainted by at least one recorded source.
    Tainted(Vec<String>),
    /// The marker could not be read to a conclusion (IO error other than
    /// "not found", or a corrupt/unparseable file). Resolves to the same
    /// restricted side as `Tainted` everywhere it is consumed.
    Undetermined(String),
}

/// Env override for the state base dir (test isolation + operator override),
/// mirroring `harness_core::store::context_ledger_base`'s contract: only an
/// **absolute** path is honored; a relative override is ignored (a relative
/// path resolves differently depending on the caller's cwd, which would let
/// `mark`/`gate`/`clear` — three separate process invocations — silently drift
/// onto different bases).
const STATE_DIR_ENV: &str = "TAINTGUARD_STATE_DIR";

/// Base directory for taint markers: `$TAINTGUARD_STATE_DIR` (absolute only)
/// or `~/.taintguard/state`.
fn state_base() -> PathBuf {
    std::env::var(STATE_DIR_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .filter(|s| Path::new(s).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| harness_core::config::base_dir("taintguard").join("state"))
}

/// Session-scoped state dir for `cwd`/`session`, canonicalizing `cwd` first so
/// symlink/relative differences in the caller's cwd resolve to the same key
/// (mirrors `harness_core::store::context_state_dir`). An empty session id
/// maps to the shared `"default"` session bucket.
fn state_dir(cwd: &Path, session: &str) -> PathBuf {
    let session = if session.is_empty() {
        safe_session("default")
    } else {
        safe_session(session)
    };
    let cwd_canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    state_base().join(project_key(&cwd_canonical)).join(session)
}

/// Full path to this session's taint marker.
fn marker_path(cwd: &Path, session: &str) -> PathBuf {
    state_dir(cwd, session).join("taint.json")
}

/// Read the marker, keeping "absent" (`Known(None)`), "there but opaque"
/// (`Undetermined`), and "there and parses" (`Known(Some(_))`) distinct — the
/// input-side counterpart of [`Check`].
fn read_state(path: &Path) -> Determination<Option<TaintState>> {
    match boundary::read_to_string(path) {
        Determination::Known(None) => Determination::known(None),
        Determination::Known(Some(text)) => match serde_json::from_str::<TaintState>(&text) {
            Ok(state) => Determination::known(Some(state)),
            Err(e) => Determination::undetermined(format!(
                "{} exists but does not parse as a taint marker: {e}",
                path.display()
            )),
        },
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Check this session's taint state for `cwd`.
pub fn check(cwd: &Path, session: &str) -> Check {
    match read_state(&marker_path(cwd, session)) {
        Determination::Known(None) => Check::Clean,
        Determination::Known(Some(state)) => {
            if state.tainted && !state.sources.is_empty() {
                Check::Tainted(state.sources)
            } else if state.tainted {
                // Marked tainted but with no recorded source (shouldn't happen
                // via `mark`, but a hand-edited/legacy marker could do this) —
                // still tainted, just with an empty (not fabricated) source list.
                Check::Tainted(Vec::new())
            } else {
                Check::Clean
            }
        }
        Determination::Undetermined(why) => Check::Undetermined(why.to_string()),
    }
}

/// `true` unless [`check`] returns `Clean` — the fail-closed bool convenience
/// the `gate` subcommand's happy path uses. Both `Tainted` and `Undetermined`
/// are `true`.
pub fn is_tainted(cwd: &Path, session: &str) -> bool {
    !matches!(check(cwd, session), Check::Clean)
}

/// Mark this session as tainted by `source` (e.g. `"web"`, `"external-read"`).
/// Merges with any existing marker (accumulating distinct sources) rather than
/// overwriting.
///
/// The underlying write ([`harness_core::store::save_bytes`]) is a fail-soft
/// primitive by design (never breaks the turn), so this function verifies the
/// write actually landed by reading it back and returns `Err` when it did not
/// — the caller is expected to log that to stderr. A failed mark is not
/// silently "successful": the caller must know a real write may not have
/// happened (though the downstream `gate` already fails closed on an
/// unreadable/absent-but-expected marker in the panic-barrier path — see
/// `hooks::mark::analyse`).
pub fn mark(cwd: &Path, session: &str, source: &str) -> Result<(), String> {
    let path = marker_path(cwd, session);
    let mut state = match read_state(&path) {
        Determination::Known(Some(existing)) => existing,
        // Absent or unreadable: start fresh rather than fail the mark — a
        // taint mark must still land even when the prior marker was opaque.
        _ => TaintState::default(),
    };
    state.tainted = true;
    if !state.sources.iter().any(|s| s == source) {
        state.sources.push(source.to_string());
    }
    let bytes = match serde_json::to_vec(&state) {
        Ok(b) => b,
        Err(e) => return Err(format!("could not serialize taint marker: {e}")),
    };
    harness_core::store::save_bytes(&path, &bytes);
    match boundary::read_to_string(&path) {
        Determination::Known(Some(text)) if text.as_bytes() == bytes.as_slice() => Ok(()),
        _ => Err(format!(
            "taint marker write could not be verified: {}",
            path.display()
        )),
    }
}

/// Clear this session's taint marker (Stop hook). Absent is success (already
/// clear). An IO failure other than "not found" is returned as `Err` for the
/// caller to log to stderr; the marker is left in place on that path — staying
/// tainted is the safe side of a failed clear, never the reverse.
pub fn clear(cwd: &Path, session: &str) -> Result<(), String> {
    let path = marker_path(cwd, session);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "could not remove taint marker {}: {e}",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TAINTGUARD_STATE_DIR` is a process-global env var, so tests that set it
    /// (via [`env`]) must never run concurrently with each other — `cargo
    /// test` runs tests in parallel threads by default. `_guard` holds this
    /// lock for the whole test (mirrors `ctxrot::config::HOME_ENV_LOCK`).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Isolate each test's state dir under a throwaway temp dir + unique
    /// session id, so parallel test runs never collide on the same marker path
    /// (mirrors the pattern used throughout the repo's other state-dir tests).
    /// Holds [`ENV_LOCK`] for its whole lifetime (released on drop) since it
    /// mutates the process-global `TAINTGUARD_STATE_DIR`.
    struct Env {
        _guard: std::sync::MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
        cwd: PathBuf,
        session: String,
    }

    fn env(name: &str) -> Env {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::Builder::new()
            .prefix(&format!("taintguard-state-{name}-"))
            .tempdir()
            .expect("tempdir");
        std::env::set_var(STATE_DIR_ENV, dir.path());
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&cwd).expect("mk project dir");
        Env {
            _guard: guard,
            _dir: dir,
            cwd,
            session: format!("sess-{name}"),
        }
    }

    #[test]
    fn clean_session_is_not_tainted() {
        let e = env("clean");
        assert!(matches!(check(&e.cwd, &e.session), Check::Clean));
        assert!(!is_tainted(&e.cwd, &e.session));
    }

    #[test]
    fn mark_then_check_is_tainted_with_source() {
        let e = env("mark");
        mark(&e.cwd, &e.session, "web").expect("mark should succeed");
        match check(&e.cwd, &e.session) {
            Check::Tainted(sources) => assert_eq!(sources, vec!["web".to_string()]),
            other => panic!("expected Tainted, got {other:?}"),
        }
        assert!(is_tainted(&e.cwd, &e.session));
    }

    #[test]
    fn mark_accumulates_distinct_sources() {
        let e = env("accum");
        mark(&e.cwd, &e.session, "web").unwrap();
        mark(&e.cwd, &e.session, "external-read").unwrap();
        mark(&e.cwd, &e.session, "web").unwrap(); // duplicate, no-op
        match check(&e.cwd, &e.session) {
            Check::Tainted(sources) => {
                assert_eq!(
                    sources,
                    vec!["web".to_string(), "external-read".to_string()]
                )
            }
            other => panic!("expected Tainted, got {other:?}"),
        }
    }

    #[test]
    fn clear_restores_clean() {
        let e = env("clear");
        mark(&e.cwd, &e.session, "web").unwrap();
        assert!(is_tainted(&e.cwd, &e.session));
        clear(&e.cwd, &e.session).expect("clear should succeed");
        assert!(matches!(check(&e.cwd, &e.session), Check::Clean));
    }

    #[test]
    fn clear_of_an_already_clean_session_is_ok() {
        let e = env("clear-noop");
        clear(&e.cwd, &e.session).expect("clearing an absent marker is success");
    }

    #[test]
    fn corrupt_marker_is_undetermined_and_fails_closed() {
        let e = env("corrupt");
        let path = marker_path(&e.cwd, &e.session);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(matches!(check(&e.cwd, &e.session), Check::Undetermined(_)));
        assert!(
            is_tainted(&e.cwd, &e.session),
            "a corrupt marker must fail closed to tainted"
        );
    }

    #[test]
    fn different_sessions_do_not_share_taint() {
        let e = env("multi");
        mark(&e.cwd, "session-A", "web").unwrap();
        assert!(is_tainted(&e.cwd, "session-A"));
        assert!(!is_tainted(&e.cwd, "session-B"));
    }
}
