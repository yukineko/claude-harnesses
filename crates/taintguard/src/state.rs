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
//!
//! # The absent-marker hole this module closes (writability probe)
//!
//! An ABSENT marker is ambiguous between two very different situations:
//! "this session genuinely never consumed untrusted content" (safe to allow)
//! and "a prior [`mark`] tried to record taint but the session state dir was
//! unwritable (read-only mount, `chmod 555`, disk full, permissions) and the
//! write silently failed" (must NOT be read as clean — [`mark`]'s write is a
//! fail-soft primitive by design, see its own docs). Reading BOTH as `Clean`
//! is exactly the cannot-determine→allow fail-open this crate exists to
//! prevent, applied to itself.
//!
//! [`check`] therefore does not trust a bare absence: on the absent-marker
//! branch ONLY (never when a marker is present and readable — there is
//! nothing to doubt there), it additionally probes whether the session state
//! dir is actually writable right now (create it if missing, write and remove
//! a throwaway file). A successful probe proves the store is healthy, so the
//! absence is trustworthy → `Clean`. A failed probe means a mark could have
//! been lost → `Undetermined` (fails closed exactly like a corrupt marker). A
//! healthy, writable, genuinely-empty store therefore still allows normally —
//! the probe only changes the answer when the store cannot be trusted.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use harness_core::boundary;
use harness_core::store::{project_key, safe_session};
use harness_core::verdict::Determination;

/// On-disk shape of the marker file.
///
/// `tainted` is deliberately NOT `#[serde(default)]`: a marker written by
/// `mark` always includes it, so a file that PARSES as JSON but lacks this
/// field (e.g. `{"foo":123}` — a wrong-schema or hand-crafted file, since an
/// atomic `save_bytes` write can never produce a torn/partial JSON body) is a
/// structurally-invalid marker, not a legitimate "not tainted" record. Making
/// it required turns that case into a `serde_json::from_str` `Err`, which
/// [`read_state`] already maps to `Undetermined` — the same fail-closed
/// answer as a corrupt marker, instead of silently defaulting to `false` →
/// `Clean` → allow.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct TaintState {
    tainted: bool,
    /// Provenance tags that contributed to the taint (e.g. `"web"`,
    /// `"external-read"`), deduplicated. Purely diagnostic — surfaced in the
    /// gate's `permissionDecisionReason` so the agent knows why. Missing is
    /// fine (an older/minimal marker with just `tainted: true`), so this one
    /// keeps `#[serde(default)]`.
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

/// [`STATE_DIR_ENV`] for sibling modules' tests (e.g. `crate::observe`), which
/// must point the state base at a temp dir the same way this module's tests do.
#[cfg(test)]
pub(crate) const STATE_DIR_ENV_FOR_TEST: &str = STATE_DIR_ENV;

/// One process-wide lock for the `TAINTGUARD_STATE_DIR` env var, shared by every
/// test in this crate's lib target.
///
/// `std::env::set_var` is process-global and `cargo test` runs a binary's tests
/// in parallel threads, so `state`'s tests and `observe`'s tests would otherwise
/// clobber each other's state base mid-assertion. One lock, not one per module
/// (two independent locks would not exclude each other, which is the bug this
/// replaces).
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire [`ENV_LOCK`] for the caller's whole test body. Poison is recovered
/// (`into_inner`) because a panicking test must not wedge every later test.
#[cfg(test)]
pub(crate) fn env_lock_for_test() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

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

/// Project-scoped (NOT session-scoped) state dir for `cwd`, reusing the exact
/// same `$TAINTGUARD_STATE_DIR`-or-`~/.taintguard/state` base and `project_key`
/// derivation as the session markers above.
///
/// Used by the observe-only ledger ([`crate::observe::ledger_path`]), which
/// must accumulate ACROSS sessions: the Stop hook's [`clear`] wipes a session's
/// marker, and a fire-rate measured within a single session is not the number
/// the measurement is for. Exposed here rather than re-deriving the layout in
/// `observe` so there is one owner of the on-disk shape.
pub fn project_state_dir(cwd: &Path) -> PathBuf {
    let cwd_canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    state_base().join(project_key(&cwd_canonical))
}

/// Marker file name within a session's state dir.
const MARKER_FILENAME: &str = "taint.json";

/// Full path to this session's taint marker.
fn marker_path(cwd: &Path, session: &str) -> PathBuf {
    state_dir(cwd, session).join(MARKER_FILENAME)
}

/// [`marker_path`] exposed for the binary target's tests, which live in a
/// separate crate and so cannot reach the private helper. Needed to plant a
/// deliberately corrupt marker and drive the `Undetermined` branch — the same
/// fault-injection this module's own `corrupt_marker_is_undetermined_and_fails_closed`
/// test uses. Not part of the runtime contract.
#[doc(hidden)]
pub fn marker_path_for_test(cwd: &Path, session: &str) -> PathBuf {
    marker_path(cwd, session)
}

/// Probe whether `dir` is actually writable right now, by creating it (if
/// missing) and creating-then-removing a throwaway file inside it. Used ONLY
/// on [`check`]'s absent-marker branch — see the module docs for why an
/// absent marker cannot be trusted without this. Never touches a marker file
/// itself (the probe file's name can never collide with [`MARKER_FILENAME`]).
///
/// Uses `std::fs` directly rather than `harness_core::boundary`/`store`: this
/// is a write-and-clean-up probe (no read to make `Determination`-shaped, and
/// no persistent artifact to route through the atomic `save_bytes` primitive)
/// with a plain bool answer — "could I write here just now", not a value to
/// report through. taintguard is not in the raw-IO ratchet's `GATE_CRATES`
/// list, so this does not raise that ratchet's tracked baseline either way.
fn probe_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    // Name includes the pid and a monotonic counter (not just the pid) so
    // concurrent probes from the SAME process (parallel test threads sharing
    // one state dir, or unlikely-but-possible concurrent hook invocations for
    // one session) never collide on one probe file.
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let probe = dir.join(format!(
        ".taintguard-writable-probe.{}.{seq}",
        std::process::id()
    ));
    if std::fs::write(&probe, b"probe").is_err() {
        return false;
    }
    // Best-effort cleanup: leaving the probe file behind would not affect
    // correctness (it never collides with MARKER_FILENAME), but tidy up
    // anyway.
    let _ = std::fs::remove_file(&probe);
    true
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
///
/// See the module docs for why the absent-marker branch additionally probes
/// store writability before trusting the absence.
pub fn check(cwd: &Path, session: &str) -> Check {
    let dir = state_dir(cwd, session);
    let marker = dir.join(MARKER_FILENAME);
    match read_state(&marker) {
        Determination::Known(None) => {
            if probe_writable(&dir) {
                Check::Clean
            } else {
                Check::Undetermined(format!(
                    "{} is absent AND the session state dir ({}) is not writable right \
                     now — a prior taint mark could have been silently lost rather than \
                     this turn genuinely never having consumed untrusted content; treating \
                     it as tainted rather than trusting the absence",
                    marker.display(),
                    dir.display()
                ))
            }
        }
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
/// happened.
///
/// That said, a caller does NOT need this `Err` to stay safe even if it
/// drops it on the floor: [`check`]'s writability probe is the actual
/// backstop. If this write is lost because the session state dir is
/// unwritable, the marker stays ABSENT, and `check` — finding it absent —
/// probes that same dir's writability before trusting the absence; a failed
/// probe (the dir is still unwritable) resolves to `Undetermined` (tainted),
/// not `Clean`. So a lost mark degrades to "gate fails closed", never to a
/// silent allow, independent of whether this `Err` was logged or ignored.
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
        let guard = super::env_lock_for_test();
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

    /// FIX #3: a valid-JSON, wrong-schema marker (missing the required
    /// `tainted` field) must fail closed to `Undetermined`, not silently
    /// serde-default to `tainted: false` → `Clean`.
    #[test]
    fn wrong_schema_marker_is_undetermined_and_fails_closed() {
        let e = env("wrong-schema");
        let path = marker_path(&e.cwd, &e.session);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{"foo":123}"#).unwrap();
        assert!(matches!(check(&e.cwd, &e.session), Check::Undetermined(_)));
        assert!(
            is_tainted(&e.cwd, &e.session),
            "a marker missing the required `tainted` field must fail closed"
        );
    }

    /// FIX #1 (PRIMARY): an absent marker in a genuinely healthy, writable
    /// store must still allow (the writability probe must not over-block a
    /// normal clean session) — the control half of the fix.
    #[test]
    fn absent_marker_in_a_writable_store_is_still_clean() {
        let e = env("writable-control");
        assert!(matches!(check(&e.cwd, &e.session), Check::Clean));
        assert!(!is_tainted(&e.cwd, &e.session));
    }

    /// FIX #1 (PRIMARY): an absent marker in an UNWRITABLE store must NOT be
    /// trusted as clean — a prior `mark` could have silently failed to
    /// persist there. `chmod 555` on the state dir itself (parent of the
    /// would-be session dir) so `probe_writable`'s `create_dir_all` fails.
    #[cfg(unix)]
    #[test]
    fn absent_marker_in_an_unwritable_store_is_undetermined_and_fails_closed() {
        use std::os::unix::fs::PermissionsExt;

        let e = env("unwritable");
        let state_root = std::env::var(STATE_DIR_ENV).expect("env() sets this");
        let state_root = PathBuf::from(state_root);

        // The dir exists (env() created the tempdir) but a mark was NEVER
        // attempted — this is exactly "readable-but-unwritable, no marker at
        // all", not "mark tried and lost a write". Both must fail closed.
        std::fs::set_permissions(&state_root, std::fs::Permissions::from_mode(0o555)).unwrap();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let outcome = check(&e.cwd, &e.session);
            assert!(
                matches!(outcome, Check::Undetermined(_)),
                "expected Undetermined, got {outcome:?}"
            );
            assert!(is_tainted(&e.cwd, &e.session));
        }));

        // Restore write permission unconditionally so `Env`'s `TempDir` can
        // clean up on drop regardless of the assertions' outcome above.
        std::fs::set_permissions(&state_root, std::fs::Permissions::from_mode(0o755)).unwrap();
        result.unwrap();
    }
}
