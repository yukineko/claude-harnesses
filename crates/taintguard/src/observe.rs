//! Observe-only (warn-only) **measurement** mode.
//!
//! # Why this module exists
//!
//! taintguard's `gate` normally downgrades write-class tools to `ask`/`deny`
//! for the rest of a tainted turn. Before enabling that fleet-wide, the
//! operator needs to answer "how much friction would this actually cause?" —
//! and today that question is only answerable by *prediction*, because the
//! gate has exactly two behaviours: enforce, or not be installed at all.
//! This module adds a third operating posture whose entire purpose is to make
//! the fire-rate **observable**: run the real check, do NOT enforce, and
//! record every enforcement that was suppressed so the total can be counted.
//!
//! # This is NOT a fail-open, and is deliberately hard to mistake for one
//!
//! Observe-only is a **declared** posture: the operator asked for it, in
//! writing, via an exact env value. It is the fourth state this crate needed —
//! "checked, and intentionally did not enforce" — and it is kept on a
//! *different axis* from [`crate::state::Check`] on purpose:
//!
//! * [`crate::state::Check`] answers **what the check found**
//!   (`Clean` / `Tainted` / `Undetermined`).
//! * [`Posture`] answers **what this process is allowed to do about a
//!   finding** (`Enforce` / `ObserveOnly`).
//!
//! Splitting the axes is the whole safety argument. If observe-only were
//! instead spelled as "make `check` return `Clean`", then "we found taint but
//! were told not to act" would become indistinguishable from "there was no
//! taint" — which is precisely the cannot-determine→clean collapse this crate
//! exists to prevent (CLAUDE.md §3), *and* it would destroy the measurement
//! this mode is being built for (a suppressed finding that reads as `Clean`
//! cannot be counted). Because the axes are separate, no code path in this
//! crate can turn a `Tainted`/`Undetermined` check into a `Clean` one.
//!
//! Note also what is NOT reinvented here: `Posture` is a two-state operating
//! mode, not a verdict. The three-valued verdict stays in
//! `harness_core::verdict` / [`crate::state::Check`]; this module adds no
//! competing tri-state (CLAUDE.md §3's "do not re-invent the shared
//! three-valued type" applies to verdicts, and `Posture` is not one).
//!
//! # Silence is not one of the options
//!
//! An observe-only gate that simply stayed quiet would be
//! indistinguishable from a gate that found nothing — the same defect as the
//! statusline whose blank output read as "plenty of headroom" (commit
//! `3b1eb24`). So a suppressed enforcement still produces **two** visible
//! artifacts: a human/model-visible `additionalContext` warning (see
//! [`crate::hookio::context_json`]) and a durable ledger line
//! ([`append`]). Observe-only suppresses the *enforcement*, never the
//! *reporting*.
//!
//! # Opt-in fails closed
//!
//! Only the exact string `"1"` in `TAINTGUARD_OBSERVE_ONLY` selects
//! observe-only. Unset, empty, `"0"`, `"false"`, `"true"`, `" 1"`, a typo, or
//! any other value all resolve to [`Posture::Enforce`] — see [`resolve`].
//! There is intentionally **no `Default` impl** for [`Posture`]: a permissive
//! posture must never be reachable via `Default::default()`, `.into()`, or
//! `?`-elision.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Env var that opts a process into observe-only mode.
pub const OBSERVE_ONLY_ENV: &str = "TAINTGUARD_OBSERVE_ONLY";

/// The **only** value that selects observe-only. Compared byte-for-byte: no
/// trimming, no case-folding, no truthy-string parsing. Anything else enforces.
pub const OBSERVE_ONLY_OPT_IN: &str = "1";

/// Filename of the append-only observe-only ledger, inside the *project*-scoped
/// state dir (see [`ledger_path`]).
pub const LEDGER_FILENAME: &str = "observe-only.jsonl";

/// What this process is permitted to do about a taint finding.
///
/// Deliberately has **no `Default`** and no `From<bool>`: the permissive
/// variant must always be written out explicitly by [`resolve`] after it has
/// seen the exact opt-in value. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a posture decides whether a finding is enforced; it must be acted on"]
pub enum Posture {
    /// Normal operation: a `Tainted`/`Undetermined` check produces an
    /// `ask`/`deny` PreToolUse decision. This is the answer for every
    /// unrecognised env value, and the answer when the var is absent.
    Enforce,
    /// Measurement mode: run the check, report the finding, but emit **no**
    /// `permissionDecision`, and append a ledger line so the suppressed
    /// enforcement can be counted.
    ObserveOnly,
}

/// Resolve a [`Posture`] from the raw env value, as a pure function so tests
/// can drive every case without racing `std::env::set_var` (mirrors
/// [`crate::interactive::resolve`]).
///
/// Fails closed by construction: the `ObserveOnly` arm requires an exact match
/// against [`OBSERVE_ONLY_OPT_IN`], and the catch-all arm — which covers
/// `None`, `Some("")`, `Some("0")`, `Some("true")`, `Some(" 1")` and every
/// typo — yields [`Posture::Enforce`].
pub fn resolve(raw: Option<&str>) -> Posture {
    match raw {
        Some(value) if value == OBSERVE_ONLY_OPT_IN => Posture::ObserveOnly,
        _ => Posture::Enforce,
    }
}

/// [`resolve`] wired to the real process environment.
pub fn posture() -> Posture {
    resolve(std::env::var(OBSERVE_ONLY_ENV).ok().as_deref())
}

/// One suppressed enforcement, as written to the ledger.
///
/// The *existence* of a record is the countable signal; the fields exist so a
/// later analysis can break the total down by trigger source and by tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Record {
    /// Unix seconds when the enforcement was suppressed. `0` means the system
    /// clock could not be read as an offset from the epoch — a diagnostic
    /// degradation only: the record still exists and still counts, because the
    /// measurement this mode produces is "how many times did the gate fire",
    /// not "at exactly what instant". A clock failure must not cause the event
    /// itself to go unrecorded.
    pub ts: u64,
    /// The write-class tool whose call would have been downgraded.
    pub tool: String,
    /// Which provenance sources had tainted the turn (`"web"`,
    /// `"external-read"`, `"lessons"`, `"internal-error"`). Empty for an
    /// `Undetermined` check, where the taint state could not be read at all.
    pub sources: Vec<String>,
    /// `"tainted"` or `"undetermined"` — kept distinct so the measurement can
    /// tell "the gate would have blocked a real taint" from "the gate would
    /// have blocked because it could not read its own state". Collapsing these
    /// would hide a store-health problem inside a friction statistic.
    pub check: String,
    /// Session id the suppression happened in, so per-session fire-rates can be
    /// derived from a project-wide ledger.
    pub session: String,
}

impl Record {
    /// Build a record, stamping the current time.
    pub fn now(tool: &str, sources: &[String], check: &str, session: &str) -> Self {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Record {
            ts,
            tool: tool.to_string(),
            sources: sources.to_vec(),
            check: check.to_string(),
            session: session.to_string(),
        }
    }
}

/// Path of the observe-only ledger for `cwd`.
///
/// Lives in the **project**-scoped dir, not the session-scoped one, precisely
/// because it must accumulate ACROSS sessions: a fire-rate measured over one
/// session is not the number anyone wants, and the Stop hook's `clear` wipes
/// session markers. Reuses [`crate::state::project_state_dir`] so the ledger
/// obeys the same `$TAINTGUARD_STATE_DIR` resolution as the markers rather than
/// inventing a second store.
pub fn ledger_path(cwd: &Path) -> PathBuf {
    crate::state::project_state_dir(cwd).join(LEDGER_FILENAME)
}

/// Append `record` to the observe-only ledger as one JSON line.
///
/// Returns `Err` when the line could not be appended (unwritable dir,
/// serialization failure, short write). The caller is expected to log that to
/// stderr — mirroring [`crate::state::mark`], a failed write is **not**
/// silently reported as success.
///
/// Unlike `mark`, there is no fail-closed backstop behind this `Err` and none
/// is claimed: a lost ledger line means the measurement under-counts, which is
/// exactly why the `Err` is surfaced instead of swallowed. It cannot cause a
/// *permission* fail-open, because the ledger is never read to decide whether
/// to enforce — [`crate::state::check`] alone does that, and it has its own
/// writability probe. So an under-counting ledger degrades the statistic (and
/// says so on stderr), never the gate.
pub fn append(cwd: &Path, record: &Record) -> Result<(), String> {
    use std::io::Write;

    let path = ledger_path(cwd);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Err(format!(
                "could not create observe-only ledger dir {}: {e}",
                parent.display()
            ));
        }
    }
    let mut line = match serde_json::to_string(record) {
        Ok(s) => s,
        Err(e) => return Err(format!("could not serialize observe-only record: {e}")),
    };
    line.push('\n');

    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            return Err(format!(
                "could not open observe-only ledger {} for append: {e}",
                path.display()
            ))
        }
    };
    match file.write_all(line.as_bytes()) {
        Ok(()) => Ok(()),
        Err(e) => Err(format!(
            "could not append to observe-only ledger {}: {e}",
            path.display()
        )),
    }
}

/// Count the ledger lines for `cwd` — the measurement this mode exists to
/// produce. Returns the number of parseable records and the number of lines
/// that could NOT be parsed, kept separate so a corrupt tail is visible as
/// corruption rather than silently lowering the count.
///
/// An absent ledger is `(0, 0)`: nothing has been suppressed yet. An unreadable
/// ledger is an `Err` — "I could not read the tally" must not be reported as a
/// tally of zero.
pub fn tally(cwd: &Path) -> Result<(usize, usize), String> {
    let path = ledger_path(cwd);
    // Read through `harness_core::boundary` rather than `std::fs` so "absent"
    // (`Known(None)`) and "there but unreadable" (`Undetermined`) arrive as
    // distinct values instead of two `io::Error` kinds one `?` could flatten —
    // the same discipline `state::read_state` uses. An absent ledger is a real
    // zero; an unreadable one must not be reported as one.
    let text = match harness_core::boundary::read_to_string(&path) {
        harness_core::verdict::Determination::Known(None) => return Ok((0, 0)),
        harness_core::verdict::Determination::Known(Some(text)) => text,
        harness_core::verdict::Determination::Undetermined(why) => {
            return Err(format!(
                "could not read observe-only ledger {}: {why}",
                path.display()
            ))
        }
    };
    let mut ok = 0usize;
    let mut bad = 0usize;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if serde_json::from_str::<Record>(line).is_ok() {
            ok += 1;
        } else {
            bad += 1;
        }
    }
    Ok((ok, bad))
}

/// The human/model-facing warning text for a suppressed enforcement.
///
/// States all three things the operator needs: that the turn IS tainted, which
/// sources tainted it, and that enforcement was suppressed *because
/// observe-only is active* (so the absence of an `ask`/`deny` is not mistaken
/// for a clean turn).
pub fn warning(sources_desc: &str, tool: &str) -> String {
    format!(
        "[taintguard] OBSERVE-ONLY (measurement mode, {OBSERVE_ONLY_ENV}={OBSERVE_ONLY_OPT_IN}): \
         this turn consumed untrusted-provenance content ({sources_desc}). Normally this \
         `{tool}` call would be downgraded to ask/deny for the rest of the turn; \
         enforcement was SUPPRESSED because observe-only is active, and the event was \
         recorded to the observe-only ledger for counting. This is NOT a clean turn."
    )
}

/// The warning text for a suppressed enforcement whose cause was an
/// unreadable taint state rather than a recorded taint. Kept separate from
/// [`warning`] so the operator can see that the gate could not read its own
/// store — a store-health signal that must not be folded into the friction
/// statistic.
pub fn warning_undetermined(why: &str, tool: &str) -> String {
    format!(
        "[taintguard] OBSERVE-ONLY (measurement mode, {OBSERVE_ONLY_ENV}={OBSERVE_ONLY_OPT_IN}): \
         could not verify this turn's taint state ({why}). Normally this `{tool}` call would \
         be downgraded to ask/deny (failing closed); enforcement was SUPPRESSED because \
         observe-only is active, and the event was recorded to the observe-only ledger. \
         This is NOT a verified-clean turn."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the exact string `"1"` opts in. Everything else — including the
    /// truthy-looking strings a naive parser would accept, and `"1"` with
    /// whitespace — enforces.
    #[test]
    fn only_the_exact_opt_in_value_selects_observe_only() {
        assert_eq!(resolve(Some("1")), Posture::ObserveOnly);

        for raw in [
            None,
            Some(""),
            Some("0"),
            Some("2"),
            Some("01"),
            Some("1.0"),
            Some(" 1"),
            Some("1 "),
            Some("\t1"),
            Some("1\n"),
            Some("true"),
            Some("TRUE"),
            Some("yes"),
            Some("on"),
            Some("false"),
            Some("observe"),
            Some("OBSERVE_ONLY"),
            Some("-1"),
            Some("11"),
        ] {
            assert_eq!(
                resolve(raw),
                Posture::Enforce,
                "{raw:?} must fail closed to Enforce"
            );
        }
    }

    /// `Posture` must not be obtainable permissively for free. This is a
    /// compile-time property (no `Default`, no `From<bool>`), asserted here as a
    /// documented intent: if someone adds `impl Default for Posture` returning
    /// `ObserveOnly`, the module docs and this test's rationale are the record
    /// of why that is forbidden.
    #[test]
    fn enforce_is_the_catch_all_not_a_default() {
        // `resolve` is the sole constructor path used by production code, and
        // its catch-all arm is Enforce.
        assert_eq!(resolve(None), Posture::Enforce);
    }

    fn env(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::Builder::new()
            .prefix(&format!("taintguard-observe-{name}-"))
            .tempdir()
            .expect("tempdir");
        std::env::set_var(crate::state::STATE_DIR_ENV_FOR_TEST, dir.path());
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&cwd).expect("mk project dir");
        (dir, cwd)
    }

    /// An absent ledger tallies as `(0, 0)` — nothing suppressed yet — and is
    /// NOT an error.
    #[test]
    fn absent_ledger_tallies_zero() {
        let _lock = crate::state::env_lock_for_test();
        let (_dir, cwd) = env("absent");
        assert_eq!(tally(&cwd).expect("absent ledger is not an error"), (0, 0));
    }

    /// Appends accumulate, and the tally counts them.
    #[test]
    fn appends_accumulate_and_tally() {
        let _lock = crate::state::env_lock_for_test();
        let (_dir, cwd) = env("accum");
        for tool in ["Bash", "Write"] {
            append(
                &cwd,
                &Record::now(tool, &["web".to_string()], "tainted", "s1"),
            )
            .expect("append should succeed in a writable dir");
        }
        assert_eq!(tally(&cwd).unwrap(), (2, 0));
    }

    /// A corrupt tail is reported as corruption, not silently dropped from the
    /// count — otherwise a damaged ledger would quietly under-report the
    /// fire-rate and look like "less friction than we thought".
    #[test]
    fn corrupt_lines_are_counted_separately_not_ignored() {
        let _lock = crate::state::env_lock_for_test();
        let (_dir, cwd) = env("corrupt");
        append(
            &cwd,
            &Record::now("Bash", &["web".to_string()], "tainted", "s1"),
        )
        .unwrap();
        let path = ledger_path(&cwd);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{ not json\n");
        std::fs::write(&path, text).unwrap();
        assert_eq!(tally(&cwd).unwrap(), (1, 1));
    }

    /// The ledger is project-scoped, not session-scoped: two sessions' events
    /// land in ONE file so a project-wide fire-rate can be totalled (and so the
    /// Stop hook's session `clear` cannot wipe the measurement).
    #[test]
    fn ledger_is_project_scoped_across_sessions() {
        let _lock = crate::state::env_lock_for_test();
        let (_dir, cwd) = env("scope");
        append(&cwd, &Record::now("Bash", &[], "undetermined", "sess-A")).unwrap();
        append(&cwd, &Record::now("Edit", &[], "undetermined", "sess-B")).unwrap();
        assert_eq!(tally(&cwd).unwrap(), (2, 0));
    }

    /// An UNREADABLE ledger is an `Err`, not a tally of zero. Absent and
    /// unreadable both produce "no lines came back", and conflating them would
    /// report a broken measurement as "nothing was suppressed" — the same
    /// cannot-determine→clean collapse this crate exists to prevent, applied to
    /// its own statistic.
    #[cfg(unix)]
    #[test]
    fn unreadable_ledger_is_err_not_a_zero_tally() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = crate::state::env_lock_for_test();
        let (_dir, cwd) = env("unreadable");
        append(
            &cwd,
            &Record::now("Bash", &["web".to_string()], "tainted", "s1"),
        )
        .unwrap();
        let path = ledger_path(&cwd);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let outcome = tally(&cwd);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            outcome.is_err(),
            "an unreadable ledger must be Err, not Ok((0, 0)); got {outcome:?}"
        );
        // Control: once readable again, the same ledger tallies its one line —
        // proving the Err above came from the permission fault, not from the
        // record being absent or malformed.
        assert_eq!(tally(&cwd).unwrap(), (1, 0));
    }

    /// A failed append returns `Err` rather than reporting success. Driven by
    /// pointing the ledger at an unwritable state root.
    #[cfg(unix)]
    #[test]
    fn unwritable_ledger_returns_err_not_silent_success() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = crate::state::env_lock_for_test();
        let (dir, cwd) = env("unwritable");
        let root = dir.path().to_path_buf();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        let outcome = append(
            &cwd,
            &Record::now("Bash", &["web".to_string()], "tainted", "s1"),
        );
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            outcome.is_err(),
            "an unwritable ledger must return Err, never silent Ok"
        );
    }

    /// The warning text carries all three required facts.
    #[test]
    fn warning_states_taint_source_and_suppression() {
        let w = warning("web, external-read", "Bash");
        assert!(w.contains("OBSERVE-ONLY"));
        assert!(w.contains("web, external-read"));
        assert!(w.contains("SUPPRESSED"));
        assert!(w.contains("Bash"));
        assert!(w.contains("NOT a clean turn"));

        let u = warning_undetermined("marker unreadable", "Write");
        assert!(u.contains("OBSERVE-ONLY"));
        assert!(u.contains("could not verify"));
        assert!(u.contains("SUPPRESSED"));
        assert!(u.contains("Write"));
    }
}
