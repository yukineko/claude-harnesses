//! PDO session-anchor integration (DESIGN §4.4 / §4.6b).
//!
//! stuckguard already tracks the edited file of every tool call (see `sig`), so
//! it can cheaply answer two anchor questions without new detection machinery:
//!
//! - **scope drift (§4.4):** are recent edits landing *outside* the files this
//!   session said it would touch? (advisory nudge, opt-in)
//! - **heartbeat piggyback (§4.6b):** keep this session's claim/lease alive on
//!   every tool call so a long single task isn't falsely reaped and stolen.
//!
//! The session's anchor (scope / run / key) lives in `overwatch`'s lease
//! registry; [`fetch_session_anchor`] reads it via `overwatch lease --session
//! <id> --json` and classifies the outcome into three [`AnchorLookup`]
//! answers — **not** a bare `Option`, because the question genuinely has
//! three answers: a live lease, a legitimate "this session holds none", and
//! "could not tell" (overwatch could not be resolved/run, or its answer
//! could not be parsed).
//!
//! **Why the third answer matters (this is not a style preference).**
//! [`heartbeat_piggyback`] is the *only* code path that refreshes this
//! session's overwatch lease heartbeat on every tool call (see
//! `main::watch`), and it only fires when an anchor was found. Before this
//! module drew the three-way distinction, "overwatch could not even be
//! asked" and "overwatch was asked and said no lease" both degraded to the
//! identical silent `None` — so a run of `overwatch` invocation failures
//! (e.g. a bare `overwatch` name that never resolved on `PATH`; see backlog
//! `1e783882`) silently stopped refreshing a **real** lease's heartbeat.
//! `overwatch`'s own reap TTL (`LEASE_TTL_SECS`,
//! `crates/overwatch/src/store.rs`) is 30 minutes: once a live session's
//! heartbeat goes stale that long, `reap_stale`
//! (`crates/overwatch/src/store.rs`) deletes the lease and another session
//! can claim the same work — silently, because nothing here ever said "I
//! could not ask". This was deliberately left as a documented open question
//! by an earlier migration (see `32ead775`) pending whether the downstream
//! reap impact was real; that was UNVERIFIED then and is confirmed now (the
//! chain above), so the silence is resolved here.
//!
//! For the *caller* (`main::watch`), the effect is unchanged on purpose:
//! stuckguard is a pure advisory hook that never blocks a tool call or ends a
//! turn, so both "no lease" and "could not tell" still mean "no anchor this
//! call, heartbeat piggyback skipped" — same as before. What changes is that
//! "could not tell" is no longer silent *overall*: it prints a diagnostic to
//! stderr (`describe_undetermined_anchor`, wired via
//! [`fetch_session_anchor`]) naming the session, the reason, and the fact
//! that heartbeat piggyback was skipped, mirroring the pattern already used
//! by `config::load_with_diagnostics` for an unreadable config file.

use std::path::PathBuf;
use std::process::Command;

use harness_core::boundary;
use harness_core::verdict::Determination;
use serde::Deserialize;

use crate::sig::Event;

/// The current session's live anchor, as read from the overwatch lease.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionAnchor {
    /// The lease key (used for `overwatch heartbeat --key`).
    #[serde(default)]
    pub key: String,
    /// The run id (used for `condukt state heartbeat --run`).
    #[serde(default)]
    pub run_id: String,
    /// Files/globs this session is responsible for. Empty = no fixed scope.
    #[serde(default)]
    pub scope: Vec<String>,
}

/// Parse an `overwatch lease --json` line into a `SessionAnchor`. Extra fields
/// (title, timestamps, done_criteria) are ignored. Pure — unit tested.
pub fn parse_anchor(json: &str) -> Option<SessionAnchor> {
    serde_json::from_str::<SessionAnchor>(json.trim()).ok()
}

/// The three-way outcome of asking overwatch for `session_id`'s live lease
/// (§4.6b). Kept distinct from a plain `Option<SessionAnchor>` — see the
/// module docs above for why the third answer matters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorLookup {
    /// overwatch answered with a live lease held by this session.
    Leased(SessionAnchor),
    /// overwatch was reachable and answered: this session holds no lease. A
    /// legitimate, frequent, silent observation (most tool calls happen
    /// outside any fixed-scope PDO lease) — never diagnosed.
    NoLease,
    /// Could not determine whether this session holds a lease: the
    /// `overwatch` binary could not be resolved or run, or its answer could
    /// not be parsed. Carries why. Never silent overall — see
    /// [`fetch_session_anchor_with_diagnostics`].
    Undetermined(String),
}

/// Where the plugin cache stores versioned overwatch installs:
/// `~/.claude/plugins/cache/yukineko/overwatch/<version>/bin/overwatch`.
/// Mirrors `crates/ctxrot/src/hooks/guard.rs`'s `find_overwatch_binary` cache
/// path.
fn overwatch_cache_dir() -> PathBuf {
    harness_core::config::home()
        .join(".claude")
        .join("plugins")
        .join("cache")
        .join("yukineko")
        .join("overwatch")
}

/// Resolve the `overwatch` binary: PATH first, then the newest versioned
/// install under the plugin cache. Distinguishes "genuinely not installed"
/// (`Known(None)` — no PATH entry and no cache dir/candidate: a real
/// observation) from "could not tell" (`Undetermined` — the cache dir exists
/// but could not be listed, e.g. permission denied).
///
/// Mirrors `crates/ctxrot/src/hooks/guard.rs`'s `find_overwatch_binary`
/// shape (PATH probe, then newest versioned cache dir) but deliberately does
/// NOT copy its directory read: that function's
/// `std::fs::read_dir(&base).ok()?.filter_map(|e| e.ok())` folds an
/// unreadable cache dir into "zero candidates" — the exact absence/opacity
/// conflation `harness_core::boundary` exists to prevent (see its own module
/// doc). This routes the read through `boundary::read_dir_entries`, which
/// keeps a missing directory (`Known(vec![])`, legitimately empty) apart
/// from an unreadable one (`Undetermined`, carries why).
fn resolve_overwatch_binary() -> Determination<Option<PathBuf>> {
    // PATH probe. A failure to even start the process here is overwhelmingly
    // "overwatch is not on PATH" — the ordinary case — but could also mean a
    // PATH entry exists yet is not executable. Either way there is still a
    // second place to look (the plugin cache) before giving up, so a PATH
    // miss alone is not folded into Undetermined here; it only decides
    // whether to short-circuit with a PATH hit.
    let mut probe = Command::new("overwatch");
    probe.arg("--version");
    if let Determination::Known(_) = boundary::run(&mut probe) {
        return Determination::known(Some(PathBuf::from("overwatch")));
    }

    let base = overwatch_cache_dir();
    let entries = match boundary::read_dir_entries(&base) {
        Determination::Known(entries) => entries,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };
    let mut candidates: Vec<(Vec<u64>, PathBuf)> = entries
        .into_iter()
        .filter_map(|version_dir| {
            let key = version_sort_key(&version_dir)?;
            let bin = version_dir.join("bin").join("overwatch");
            bin.exists().then_some((key, bin))
        })
        .collect();
    candidates.sort();
    Determination::known(candidates.pop().map(|(_, bin)| bin))
}

/// Ordering key for a `<version>` directory under the plugin cache: its name
/// split into numeric components, so `0.3.10` sorts *after* `0.3.9`.
///
/// The obvious `candidates.sort()` on the paths compares version dirs as
/// strings, where `"0.3.9" > "0.3.10"` and the older install wins. That is not
/// a hypothetical ordering: the cache retains every version dir ever rolled
/// out (measured 2026-08-06: `ls ~/.claude/plugins/cache/yukineko/overwatch`
/// → `0.1.30 0.1.38 0.2.18 0.2.20 0.2.24`), and two-digit patch numbers are
/// already present there.
///
/// `None` for a name that is not purely numeric-dotted — an unparseable dir is
/// not silently ranked below the real ones (which would let a stray directory
/// decide the winner by default); it is dropped from the candidate set
/// entirely. This is the one place where dropping is right rather than a
/// fail-open: the directory is not a version, so it is not an answer to
/// "which version is newest", and the caller still sees `Known(None)` if it
/// was the only entry.
fn version_sort_key(version_dir: &std::path::Path) -> Option<Vec<u64>> {
    version_dir
        .file_name()?
        .to_str()?
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}

/// Diagnostic text for the `AnchorLookup::Undetermined` branch. Pure — unit
/// tested without capturing stderr, mirroring
/// `config::describe_unreadable_config`. Names the session id and the reason
/// so a reader can never mistake "heartbeat piggyback was silently skipped
/// this call" for "this session simply holds no lease".
fn describe_undetermined_anchor(session_id: &str, why: &str) -> String {
    format!(
        "stuckguard: could not query overwatch for session {session_id}'s lease ({why}); \
         heartbeat piggyback (§4.6b) was skipped this call. If overwatch stays unreachable, \
         this session's lease (if any) will not be refreshed and may eventually be reaped as \
         stale by another session."
    )
}

/// Shell out to `overwatch lease --session <id> --json` and classify the
/// result into the three [`AnchorLookup`] answers. Never panics; every
/// failure mode routes through `harness_core::boundary`.
fn lookup_session_anchor(session_id: &str) -> AnchorLookup {
    let binary = match resolve_overwatch_binary() {
        Determination::Undetermined(why) => {
            return AnchorLookup::Undetermined(format!(
                "could not resolve the overwatch binary: {}",
                why.as_str()
            ));
        }
        // Genuinely not installed (no PATH entry, no plugin-cache install):
        // there is no lease system to ask in the first place, so this is a
        // legitimate NoLease, not a judgment failure.
        Determination::Known(None) => return AnchorLookup::NoLease,
        Determination::Known(Some(bin)) => bin,
    };

    let mut cmd = Command::new(&binary);
    cmd.args(["lease", "--session", session_id, "--json"]);
    let output = match boundary::run(&mut cmd) {
        Determination::Known(output) => output,
        Determination::Undetermined(why) => {
            return AnchorLookup::Undetermined(format!(
                "could not run `{} lease --session {session_id} --json`: {}",
                binary.display(),
                why.as_str()
            ));
        }
    };

    // `overwatch lease_for_session` (crates/overwatch/src/lease.rs) exits 1,
    // with no stdout, specifically for "no live anchor for this session" —
    // its own documented fail-soft contract ("silent, non-zero exit"). That
    // exit code is unfortunately reused by `main`'s generic `Result` error
    // path too (any other `Result::Err` inside overwatch's `main` also
    // surfaces as exit 1 via `?`), so reading exit 1 as NoLease is not a
    // perfect disambiguation from "some other internal overwatch error" —
    // fixing that would mean giving overwatch a distinct exit code, out of
    // this crate's scope (`touched_files` is `crates/stuckguard/` only).
    // It is still the more honest reading available here: it matches
    // overwatch's own stated contract for its by far most common non-zero
    // exit, rather than treating every non-zero exit — including this
    // frequent, legitimate one — as a judgment failure.
    if output.code() == 1 {
        return AnchorLookup::NoLease;
    }

    let stdout = match output.stdout_on_success() {
        Determination::Known(stdout) => stdout,
        Determination::Undetermined(why) => {
            return AnchorLookup::Undetermined(why.as_str().to_string());
        }
    };
    match parse_anchor(&stdout) {
        Some(a) => AnchorLookup::Leased(a),
        None => AnchorLookup::Undetermined(format!(
            "overwatch answered for session {session_id} but its output could not be parsed \
             as a lease"
        )),
    }
}

/// The real lookup behind [`fetch_session_anchor`], with the diagnostic sink
/// injected instead of hardcoded to `eprintln!` — the same seam
/// `config::load_with_diagnostics` uses, and for the same reason: it lets a
/// test observe *whether* the diagnostic fires, not just that the text
/// formatter (`describe_undetermined_anchor`) produces good text, and not
/// just that the fallback-to-`None` behavior downstream (`main::watch`) is
/// reachable — either of which would stay green even with the `eprintln!`
/// deleted. [`fetch_session_anchor`] is the only real caller.
pub fn fetch_session_anchor_with_diagnostics(
    session_id: &str,
    diag: &mut dyn FnMut(String),
) -> AnchorLookup {
    let lookup = lookup_session_anchor(session_id);
    if let AnchorLookup::Undetermined(why) = &lookup {
        diag(describe_undetermined_anchor(session_id, why));
    }
    lookup
}

/// Read the live anchor for `session_id`, printing any "could not determine"
/// diagnostic to stderr. Thin wrapper over
/// [`fetch_session_anchor_with_diagnostics`]; see there for the RED/GREEN
/// seam this split exists to create.
pub fn fetch_session_anchor(session_id: &str) -> AnchorLookup {
    fetch_session_anchor_with_diagnostics(session_id, &mut |msg| eprintln!("{msg}"))
}

/// Keep this session's claim/lease alive (§4.6b). Fires a `condukt` and an
/// `overwatch` heartbeat; both are best-effort — errors and missing binaries are
/// ignored (the nudge path must never be blocked by this side effect).
pub fn heartbeat_piggyback(anchor: &SessionAnchor) {
    if !anchor.run_id.is_empty() {
        let mut cmd = Command::new("condukt");
        cmd.args(["state", "heartbeat", "--run", &anchor.run_id]);
        let _ = boundary::run(&mut cmd);
    }
    if !anchor.key.is_empty() {
        let mut cmd = Command::new("overwatch");
        cmd.args(["heartbeat", "--key", &anchor.key]);
        let _ = boundary::run(&mut cmd);
    }
}

/// Literal path prefix of a glob: the part before the first glob metacharacter,
/// trailing `/` trimmed.
fn glob_prefix(g: &str) -> &str {
    let end = g.find(['*', '?', '[']).unwrap_or(g.len());
    g[..end].trim_end_matches('/')
}

/// Is `file` within the anchor `scope`? Matches by substring on each glob's
/// literal prefix, so a tool's absolute `file_path`
/// (`/home/u/repo/crates/x/src/a.rs`) still matches a repo-relative scope glob
/// (`crates/x/src/**`). A bare glob (`**`, empty prefix) covers everything.
/// Deliberately lenient: over-matching "in scope" only makes the drift advisory
/// *less* likely to fire (conservative for an opt-in nudge).
fn file_in_scope(file: &str, scope: &[String]) -> bool {
    scope.iter().any(|g| {
        let p = glob_prefix(g);
        p.is_empty() || file.contains(p)
    })
}

/// Detect PDO scope drift (§4.4): the trailing run of consecutive *edited* files
/// (events carrying a `file`) that all fall outside `scope`. Returns the drifted
/// files (deduped, in order) when that run reaches `threshold`, else `None`.
/// Non-edit events (no `file`) are skipped; an in-scope edit resets the run.
/// `None` when scope is empty (no anchor to compare against) or threshold is 0.
pub fn scope_drift(events: &[Event], scope: &[String], threshold: usize) -> Option<Vec<String>> {
    if scope.is_empty() || threshold == 0 {
        return None;
    }
    let mut run: Vec<String> = Vec::new();
    for ev in events {
        let Some(file) = &ev.file else { continue };
        if file_in_scope(file, scope) {
            run.clear();
        } else {
            run.push(file.clone());
        }
    }
    if run.len() < threshold {
        return None;
    }
    // Dedup while preserving order.
    let mut seen = std::collections::BTreeSet::new();
    let drifted: Vec<String> = run.into_iter().filter(|f| seen.insert(f.clone())).collect();
    Some(drifted)
}

/// The advisory nudge text for scope drift.
pub fn scope_drift_message(scope: &[String], drifted: &[String]) -> String {
    format!(
        "🧭 stuckguard: このセッションは {} の担当のはずですが、直近の編集は {} でした。\
         scope を広げる意図なら anchor を更新してください（`overwatch begin --key ... --scope ...` を再実行）。\
         そうでなければ元のタスクに戻ってください。",
        scope.join(", "),
        drifted.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn edit(seq: u64, file: &str) -> Event {
        Event {
            seq,
            tool: "Edit".to_string(),
            sig: format!("sig-{seq}"),
            tokens: BTreeSet::new(),
            file: Some(file.to_string()),
            old_h: None,
            new_h: None,
            error: false,
            failed_test_digest: None,
        }
    }

    fn non_edit(seq: u64) -> Event {
        Event {
            seq,
            tool: "Bash".to_string(),
            sig: format!("sig-{seq}"),
            tokens: BTreeSet::new(),
            file: None,
            old_h: None,
            new_h: None,
            error: false,
            failed_test_digest: None,
        }
    }

    #[test]
    fn parse_anchor_reads_scope_and_ignores_extra_fields() {
        let json = r#"{"key":"k","title":"t","run_id":"r","scope":["crates/x/src/**"],"done_criteria":"green","claimed_at":1}"#;
        let a = parse_anchor(json).expect("parses");
        assert_eq!(a.key, "k");
        assert_eq!(a.run_id, "r");
        assert_eq!(a.scope, vec!["crates/x/src/**".to_string()]);
    }

    #[test]
    fn scope_drift_fires_after_threshold_out_of_scope_edits() {
        let scope = vec!["crates/overwatch/src/**".to_string()];
        let events = vec![
            edit(1, "/home/u/repo/crates/foo/src/a.rs"),
            edit(2, "crates/bar/src/b.rs"),
            edit(3, "crates/baz/src/c.rs"),
        ];
        let drifted = scope_drift(&events, &scope, 3).expect("drift detected");
        assert_eq!(drifted.len(), 3);
    }

    #[test]
    fn scope_drift_absolute_path_matches_relative_scope_glob() {
        // An absolute file_path inside the scope's dir is IN scope -> no drift.
        let scope = vec!["crates/overwatch/src/**".to_string()];
        let events = vec![
            edit(1, "/home/u/repo/crates/overwatch/src/store.rs"),
            edit(2, "/home/u/repo/crates/overwatch/src/lease.rs"),
            edit(3, "/home/u/repo/crates/overwatch/src/main.rs"),
        ];
        assert!(scope_drift(&events, &scope, 3).is_none());
    }

    #[test]
    fn in_scope_edit_resets_the_run() {
        let scope = vec!["crates/overwatch/src/**".to_string()];
        let events = vec![
            edit(1, "crates/foo/a.rs"),        // out
            edit(2, "crates/foo/b.rs"),        // out
            edit(3, "crates/overwatch/src/x"), // IN -> resets
            edit(4, "crates/foo/c.rs"),        // out (run = 1)
        ];
        assert!(scope_drift(&events, &scope, 3).is_none());
    }

    #[test]
    fn non_edit_events_do_not_break_the_run() {
        let scope = vec!["crates/overwatch/src/**".to_string()];
        let events = vec![
            edit(1, "crates/foo/a.rs"),
            non_edit(2), // skipped
            edit(3, "crates/foo/b.rs"),
            non_edit(4), // skipped
            edit(5, "crates/foo/c.rs"),
        ];
        assert!(scope_drift(&events, &scope, 3).is_some());
    }

    #[test]
    fn empty_scope_never_drifts() {
        let events = vec![edit(1, "a.rs"), edit(2, "b.rs"), edit(3, "c.rs")];
        assert!(scope_drift(&events, &[], 3).is_none());
    }

    // --- binary resolution / lookup tri-state (backlog 1469673b resolved) ---
    //
    // These tests mutate the process-global `HOME`/`PATH` env vars, so they
    // are serialized behind this lock under parallel `cargo test`.
    //
    // Scope of that guarantee, stated exactly: it serializes the tests *in
    // this module* against each other, and nothing else. It is a separate
    // `Mutex` from the same-named `ENV_LOCK` in `main.rs`'s test module
    // (which guards `LESSONS_STORE_DIR`), and two distinct mutexes cannot
    // serialize against one another — so an `anchor` test and a `main` test
    // can still call `set_var` concurrently. They write disjoint variables,
    // so neither clobbers the other's value; what remains is the lower-level
    // race `std::env::set_var` has with any concurrent read of the
    // environment block regardless of key (the reason Rust 2024 made it
    // `unsafe`), which these tests exercise by spawning subprocesses. Folding
    // both modules onto one shared lock is the real fix and is queued
    // separately; the point here is that this comment must not claim a
    // cross-module guarantee the code does not provide.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A `PATH` guaranteed to contain no `overwatch` binary, so
    /// `resolve_overwatch_binary`'s PATH probe genuinely misses and falls
    /// through to the plugin-cache search — even on a dev machine (like this
    /// one) whose real `PATH` includes a real installed overwatch via its
    /// plugin-cache `bin/` dir.
    const PATH_WITHOUT_OVERWATCH: &str = "/usr/bin:/bin";

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stuckguard-anchor-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Run `f` with `HOME` pointed at `home` and `PATH` overridden to
    /// [`PATH_WITHOUT_OVERWATCH`], restoring both afterward. Serialized by
    /// [`ENV_LOCK`].
    fn with_env<T>(home: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var("HOME").ok();
        let prev_path = std::env::var("PATH").ok();
        std::env::set_var("HOME", home);
        std::env::set_var("PATH", PATH_WITHOUT_OVERWATCH);

        let result = f();

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        result
    }

    #[test]
    fn describe_undetermined_anchor_names_session_and_reason() {
        let msg = describe_undetermined_anchor("sess-xyz", "permission denied (test)");
        assert!(
            msg.contains("sess-xyz"),
            "diagnostic must name the session id: {msg}"
        );
        assert!(
            msg.contains("permission denied (test)"),
            "diagnostic must carry the reason: {msg}"
        );
        assert!(
            msg.contains("heartbeat"),
            "diagnostic must say heartbeat was affected, not just 'could not query': {msg}"
        );
    }

    #[test]
    fn resolve_overwatch_binary_not_installed_is_known_none() {
        let home = temp_home("not-installed");
        // No `.claude/plugins/cache/yukineko/overwatch` dir at all under this
        // HOME: a directory that does not exist is a real, legitimate
        // observation of absence (mirrors `read_dir_entries`'s own NotFound
        // contract), not a judgment failure.
        let outcome = with_env(&home, resolve_overwatch_binary);
        assert_eq!(
            outcome,
            Determination::Known(None),
            "no PATH entry and no cache dir must resolve to a legitimate Known(None), \
             not Undetermined: {outcome:?}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_overwatch_binary_unreadable_cache_dir_is_undetermined() {
        use std::os::unix::fs::PermissionsExt;

        let home = temp_home("unreadable-cache");
        let cache_dir = overwatch_cache_dir_under(&home);
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::set_permissions(&cache_dir, std::fs::Permissions::from_mode(0o000)).unwrap();

        // If chmod 000 does not actually deny this uid (root), the premise of
        // the test is absent — say so instead of asserting the wrong thing.
        let denied = std::fs::read_dir(&cache_dir).is_err();

        let outcome = with_env(&home, resolve_overwatch_binary);

        std::fs::set_permissions(&cache_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&home);

        assert!(
            denied,
            "precondition: chmod 000 must deny this uid (running as root?)"
        );
        assert!(
            matches!(outcome, Determination::Undetermined(_)),
            "a cache dir that exists but cannot be listed must be Undetermined, \
             not folded into Known(None): {outcome:?}"
        );
    }

    #[test]
    fn resolve_overwatch_binary_finds_the_newest_versioned_cache_candidate() {
        let home = temp_home("versioned-cache");
        let cache_dir = overwatch_cache_dir_under(&home);
        for version in ["0.1.0", "0.2.0", "0.1.9"] {
            let bin_dir = cache_dir.join(version).join("bin");
            std::fs::create_dir_all(&bin_dir).unwrap();
            std::fs::write(bin_dir.join("overwatch"), b"#!/bin/sh\nexit 0\n").unwrap();
        }
        // A version dir with no bin/overwatch inside it must not count as a
        // candidate (`.filter(|p| p.exists())`).
        std::fs::create_dir_all(cache_dir.join("0.9.9")).unwrap();

        let outcome = with_env(&home, resolve_overwatch_binary);
        let _ = std::fs::remove_dir_all(&home);

        assert!(
            matches!(&outcome, Determination::Known(Some(_))),
            "expected Known(Some(..)), got {outcome:?}"
        );
        if let Determination::Known(Some(path)) = outcome {
            assert!(
                path.ends_with("0.2.0/bin/overwatch"),
                "must resolve the newest version dir with a real binary present: {path:?}"
            );
        }
    }

    /// The sibling test above picks version dirs where lexicographic order and
    /// version order happen to agree (`0.1.0` < `0.1.9` < `0.2.0`), so it
    /// cannot tell the two apart — it stays green whichever one the code
    /// implements. This one separates them: by string, `"0.3.9" > "0.3.10"`,
    /// so a plain `sort()` picks the OLDER install.
    ///
    /// Not hypothetical. The plugin cache keeps every version dir ever rolled
    /// out (measured 2026-08-06 on this machine:
    /// `ls ~/.claude/plugins/cache/yukineko/overwatch` → `0.1.30 0.1.38 0.2.18
    /// 0.2.20 0.2.24`, five retained dirs, and note `0.1.38` — the two-digit
    /// patch numbers this fixture is about have already occurred). Today's
    /// `0.2.24` still sorts correctly only because no `0.2.9` dir was ever
    /// installed; the next single-digit/two-digit straddle inside one minor
    /// series resolves the wrong way.
    #[test]
    fn resolve_overwatch_binary_orders_versions_numerically_not_lexicographically() {
        let home = temp_home("version-ordering");
        let cache_dir = overwatch_cache_dir_under(&home);
        for version in ["0.3.9", "0.3.10"] {
            let bin_dir = cache_dir.join(version).join("bin");
            std::fs::create_dir_all(&bin_dir).unwrap();
            std::fs::write(bin_dir.join("overwatch"), b"#!/bin/sh\nexit 0\n").unwrap();
        }

        let outcome = with_env(&home, resolve_overwatch_binary);
        let _ = std::fs::remove_dir_all(&home);

        assert!(
            matches!(&outcome, Determination::Known(Some(_))),
            "expected Known(Some(..)), got {outcome:?}"
        );
        if let Determination::Known(Some(path)) = outcome {
            assert!(
                path.ends_with("0.3.10/bin/overwatch"),
                "0.3.10 is newer than 0.3.9; a lexicographic sort picks 0.3.9 and would run \
                 a stale overwatch against a newer lease ledger. got: {path:?}"
            );
        }
    }

    /// Test-only helper mirroring `overwatch_cache_dir()`'s path shape but
    /// rooted at an explicit `home` (rather than `harness_core::config::home()`
    /// reading the process-global `HOME`), so a test can build fixtures at the
    /// same relative path it is about to point `HOME` at.
    fn overwatch_cache_dir_under(home: &std::path::Path) -> PathBuf {
        home.join(".claude")
            .join("plugins")
            .join("cache")
            .join("yukineko")
            .join("overwatch")
    }

    #[test]
    fn fetch_session_anchor_with_diagnostics_is_silent_when_overwatch_is_not_installed() {
        let home = temp_home("silent-not-installed");
        let mut diags = Vec::new();
        let lookup = with_env(&home, || {
            fetch_session_anchor_with_diagnostics("sess-a", &mut |m| diags.push(m))
        });
        let _ = std::fs::remove_dir_all(&home);

        assert_eq!(
            lookup,
            AnchorLookup::NoLease,
            "overwatch simply not being installed is a legitimate NoLease: {lookup:?}"
        );
        assert!(
            diags.is_empty(),
            "a legitimate NoLease must emit ZERO diagnostics (anti-vacuity control): {diags:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fetch_session_anchor_with_diagnostics_fires_exactly_once_when_undetermined() {
        use std::os::unix::fs::PermissionsExt;

        let home = temp_home("diag-fires");
        let cache_dir = overwatch_cache_dir_under(&home);
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::set_permissions(&cache_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = std::fs::read_dir(&cache_dir).is_err();

        let mut diags = Vec::new();
        let lookup = with_env(&home, || {
            fetch_session_anchor_with_diagnostics("sess-undetermined", &mut |m| diags.push(m))
        });

        std::fs::set_permissions(&cache_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&home);

        assert!(
            denied,
            "precondition: chmod 000 must deny this uid (running as root?)"
        );
        assert!(
            matches!(lookup, AnchorLookup::Undetermined(_)),
            "an unreadable cache dir must resolve to Undetermined: {lookup:?}"
        );
        assert_eq!(
            diags.len(),
            1,
            "an Undetermined lookup must emit exactly one diagnostic; got {diags:?}"
        );
        assert!(
            diags[0].contains("sess-undetermined"),
            "the diagnostic must name the session id: {}",
            diags[0]
        );
    }
}
