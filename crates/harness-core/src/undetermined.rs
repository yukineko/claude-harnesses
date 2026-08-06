//! Telemetry for the "could not determine" branch (backlog 6d493e39).
//!
//! # Why this exists
//!
//! The fail-opens this repo has spent months removing all shared one property:
//! they were **completely silent**. A check that could not run returned an empty
//! set, an `unwrap_or(false)`, a default — and nothing anywhere recorded that a
//! question had gone unanswered. You could only find them by reading code.
//!
//! This module makes the *volume* of that class an observed number instead of a
//! judgement. Every `Verdict::undetermined` / `Determination::undetermined`
//! records one line: when, which crate, which file and line, and why. `overwatch
//! undetermined-metrics` then aggregates by crate and by reason over a time
//! window. Nobody has to guess whether undetermined paths are hot.
//!
//! It is deliberately NOT a gate. Recording is best-effort and a write failure
//! never changes a verdict — telemetry that can block is a new failure mode
//! bolted onto every gate in the fleet.
//!
//! # But a silent telemetry sink is itself a fail-open
//!
//! "Zero undetermined events recorded" and "nothing was recording" render
//! identically as `0`, and the second one reads as good news. That is the exact
//! shape this module was built to eliminate, so it must not reproduce it:
//!
//! * [`sink_state`] reports, at any moment, whether recording is active and
//!   where it writes — so a reader can tell an empty stream from a closed one.
//!   `overwatch undetermined-metrics` prints it alongside the counts.
//! * write failures are counted in [`dropped_writes`] and warned once on stderr,
//!   rather than vanishing.
//! * the per-process cap emits an explicit `capped` marker record instead of
//!   quietly dropping the tail (CLAUDE.md's "no silent caps").
//!
//! # Test runs must not pollute the measurement
//!
//! `cargo test` sets `CARGO` in the environment of the test binary; a deployed
//! plugin binary, spawned by Claude Code, does not have it. (Measured, not
//! assumed — `cargo_is_the_documented_test_discriminator` pins both halves.) So
//! recording suppresses itself under cargo, because a metric mixing production
//! hook runs with the suite's thousands of deliberate error-path constructions
//! would measure nothing.
//!
//! That suppression is a heuristic, and heuristics that silently discard data
//! are how measurements die. Hence: it is reported by [`sink_state`], and
//! `HARNESS_UNDETERMINED_SINK` overrides it in both directions (a path forces
//! recording ON even under cargo, which is how this module's own tests observe
//! it; `off` forces it off).

use std::io::Write;
use std::panic::Location;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Environment override for the sink. An absolute path forces recording ON to
/// that file (even under cargo); the literal `off` forces it OFF.
pub const SINK_ENV: &str = "HARNESS_UNDETERMINED_SINK";

/// Records written by one process before the cap engages. Undetermined events
/// should be rare; a process producing this many is itself the finding, and the
/// cap keeps a pathological loop from writing an unbounded file inside a hook.
pub const MAX_RECORDS_PER_PROCESS: u64 = 512;

static WRITTEN: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static WARNED: AtomicBool = AtomicBool::new(false);

/// Where undetermined telemetry goes, or why it goes nowhere.
///
/// Exists so `0 records` is never ambiguous: a reader can always distinguish
/// "nothing happened" from "nothing was listening".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkState {
    /// Recording, to this file.
    Active(PathBuf),
    /// Suppressed because `CARGO` is set — a `cargo test` / `cargo run`
    /// context, whose error-path constructions are deliberate and would swamp
    /// the production signal.
    SuppressedUnderCargo,
    /// Turned off explicitly via `HARNESS_UNDETERMINED_SINK=off`.
    DisabledByEnv,
    /// The sink path could not be resolved. Carries why.
    Unresolvable(String),
}

impl SinkState {
    /// A one-line human rendering, for `undetermined-metrics` to print next to
    /// the counts.
    pub fn describe(&self) -> String {
        match self {
            SinkState::Active(p) => format!("active ({})", p.display()),
            SinkState::SuppressedUnderCargo => {
                "SUPPRESSED (CARGO is set — this is a cargo test/run context, not \
                 a deployed hook). A zero count here says nothing about production."
                    .to_string()
            }
            SinkState::DisabledByEnv => {
                format!("DISABLED ({SINK_ENV}=off). A zero count means nothing was recorded.")
            }
            SinkState::Unresolvable(why) => {
                format!("UNRESOLVABLE ({why}). A zero count means nothing was recorded.")
            }
        }
    }
}

/// Resolve the sink for the current environment and working directory.
///
/// The default path is the overwatch store for this project — the same
/// `base_dir("overwatch")/<project key>/overwatch/` that overwatch's other four
/// streams live in — so `overwatch undetermined-metrics` finds it without
/// configuration and without harness-core depending on overwatch (which would
/// invert the dependency direction).
pub fn sink_state() -> SinkState {
    match std::env::var(SINK_ENV) {
        Ok(v) if v.trim().eq_ignore_ascii_case("off") => return SinkState::DisabledByEnv,
        Ok(v) if !v.trim().is_empty() => return SinkState::Active(PathBuf::from(v.trim())),
        _ => {}
    }
    if std::env::var_os("CARGO").is_some() {
        return SinkState::SuppressedUnderCargo;
    }
    match std::env::current_dir() {
        Ok(cwd) => SinkState::Active(default_sink_path(&cwd)),
        Err(e) => SinkState::Unresolvable(format!("cannot resolve cwd: {e}")),
    }
}

/// The default sink path for a project, mirroring overwatch's `storage_root`.
pub fn default_sink_path(cwd: &std::path::Path) -> PathBuf {
    let base = crate::config::base_dir("overwatch");
    let repo_root = crate::projkey::repo_root(cwd);
    let key = crate::projkey::project_key(&repo_root);
    base.join(key).join("overwatch").join("undetermined.jsonl")
}

/// How many records this process has written.
pub fn written() -> u64 {
    WRITTEN.load(Ordering::Relaxed)
}

/// How many records this process failed to write or dropped to the cap. Never
/// silent: `undetermined-metrics` has no way to see this (it is per-process),
/// which is why a failure also warns once on stderr.
pub fn dropped_writes() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

/// Record one undetermined event. Best-effort: never panics, never returns an
/// error, never changes a verdict.
///
/// `#[track_caller]` at the two constructors means `loc` is the site that
/// actually gave up, not this function — so the crate/file/line identify the
/// real branch.
pub fn record(reason: &str, loc: &'static Location<'static>) {
    let path = match sink_state() {
        SinkState::Active(p) => p,
        _ => return,
    };

    // Cap check first, so the pathological case costs one atomic and no IO.
    let n = WRITTEN.fetch_add(1, Ordering::Relaxed);
    if n > MAX_RECORDS_PER_PROCESS {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let capped = n == MAX_RECORDS_PER_PROCESS;

    let line = serde_json::json!({
        "ts": now_secs(),
        "crate": crate_of(loc.file()),
        "file": loc.file(),
        "line": loc.line(),
        // The cap marker rides on the record rather than replacing it, so the
        // stream never loses an event to its own bookkeeping. A reader seeing
        // `capped` knows the true count for this process is >= the cap.
        "reason": if capped {
            format!("[CAPPED at {MAX_RECORDS_PER_PROCESS} records/process; further events this process are NOT recorded] {reason}")
        } else {
            reason.to_string()
        },
        "capped": capped,
    });

    if let Err(e) = append_line(&path, &line.to_string()) {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        // Once per process: a hook that cannot write telemetry should say so,
        // but must not spam a turn's stderr.
        if !WARNED.swap(true, Ordering::Relaxed) {
            eprintln!(
                "harness-core: WARNING cannot record undetermined telemetry to {} ({e}); \
                 counts from this process will be missing",
                path.display()
            );
        }
    }
}

fn append_line(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // One `write_all` of a newline-terminated line: O_APPEND makes a write this
    // small atomic against other appenders, so concurrent hooks cannot interleave
    // halves of two records into one corrupt line.
    f.write_all(format!("{line}\n").as_bytes())
}

/// Derive the crate name from a `Location::file()` path.
///
/// Workspace sources render as `crates/<name>/src/...`, which is the case that
/// matters. Anything else yields `unknown` rather than a guess — a wrong crate
/// attribution would send someone auditing the wrong module.
pub fn crate_of(file: &str) -> &str {
    let norm = file.replace('\\', "/");
    let mut parts = norm.split('/');
    if parts.next() == Some("crates") {
        if let Some(name) = parts.next() {
            // Borrow from the original, not the normalized copy.
            let start = "crates/".len();
            if let Some(end) = file.get(start..).and_then(|r| r.find(['/', '\\'])) {
                return &file[start..start + end];
            }
            let _ = name;
        }
    }
    "unknown"
}

/// Unix seconds. Local rather than pulling in a dependency, and infallible:
/// a clock before the epoch yields 0 rather than dropping the record, because
/// a missing record is worse than a wrong timestamp on one line.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_is_derived_from_a_workspace_path() {
        assert_eq!(crate_of("crates/blastguard/src/diffrisk.rs"), "blastguard");
        assert_eq!(crate_of("crates/harness-core/src/usage.rs"), "harness-core");
    }

    #[test]
    fn an_unrecognized_path_is_unknown_not_a_guess() {
        // A wrong attribution is worse than an honest "unknown": it points an
        // auditor at a crate that never produced the event.
        assert_eq!(crate_of("src/main.rs"), "unknown");
        assert_eq!(
            crate_of("/rustc/deadbeef/library/std/src/io/mod.rs"),
            "unknown"
        );
        assert_eq!(crate_of("crates"), "unknown");
    }

    #[test]
    fn cargo_is_the_documented_test_discriminator() {
        // Reads SINK_ENV via sink_state() without setting it, so it still needs
        // the guard: a concurrent temp_env() in this module would have it set
        // to a path or "off" and this assertion would read that instead.
        let _guard = crate::test_env::lock();
        // Both halves of the claim in the module docs. If cargo ever stops
        // setting CARGO for test binaries, this fails rather than silently
        // turning production recording into test-polluted recording.
        assert!(
            std::env::var_os("CARGO").is_some(),
            "this assertion runs under `cargo test`, which the module docs claim \
             sets CARGO; if that is no longer true the suppression heuristic is \
             wrong in the other direction"
        );
        assert_eq!(sink_state(), SinkState::SuppressedUnderCargo);
    }

    #[test]
    fn an_explicit_sink_path_overrides_the_cargo_suppression() {
        // Which is how the integration test observes real records while still
        // running under cargo.
        let dir = std::env::temp_dir().join(format!("hc-undet-{}", std::process::id()));
        let path = dir.join("u.jsonl");
        temp_env(SINK_ENV, Some(path.to_string_lossy().as_ref()), || {
            assert_eq!(sink_state(), SinkState::Active(path.clone()));
        });
    }

    #[test]
    fn off_disables_and_says_so() {
        temp_env(SINK_ENV, Some("off"), || {
            assert_eq!(sink_state(), SinkState::DisabledByEnv);
            // The description must make a zero count uninterpretable as clean.
            assert!(SinkState::DisabledByEnv
                .describe()
                .contains("nothing was recorded"));
        });
    }

    #[test]
    fn every_inactive_state_warns_that_zero_means_nothing() {
        // The whole point: a reader must never take `0` from a closed sink as
        // "no undetermined events happened".
        for s in [
            SinkState::SuppressedUnderCargo,
            SinkState::DisabledByEnv,
            SinkState::Unresolvable("boom".into()),
        ] {
            let d = s.describe();
            assert!(
                d.contains("says nothing") || d.contains("nothing was recorded"),
                "{s:?} describes itself as {d:?}, which a reader could mistake for \
                 an observed zero"
            );
        }
        assert!(SinkState::Active(PathBuf::from("/x"))
            .describe()
            .starts_with("active"));
    }

    // Serialization comes from crate::test_env::lock(), which is crate-wide.
    // A module-private mutex here would stop these cases racing each other and
    // nothing else, while reading as though the whole hazard were covered.

    /// Set an env var for the duration of `f`, then restore.
    fn temp_env(key: &str, val: Option<&str>, f: impl FnOnce()) {
        let _guard = crate::test_env::lock();
        let prev = std::env::var_os(key);
        match val {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        f();
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
