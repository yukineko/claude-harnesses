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
//! `3b1eb24`). So a suppressed enforcement still reports, in three places of
//! **unequal reliability** (see [`crate::hookio::observe_json`], which is what
//! observe mode emits):
//!
//! * `additionalContext` — **model-facing**. Per the hooks reference it is
//!   wrapped in a system reminder and injected into Claude's context, and
//!   "doesn't appear as a chat message in the interface". That is the docs'
//!   actual wording and it is all that is claimed here: it is not a channel a
//!   human is shown, so it cannot be relied on to inform one. (Not the stronger
//!   claim that it is unreachable by any means — a transcript inspector is not
//!   the operator-facing readout this mode needs either way.)
//! * a top-level `systemMessage` — **best-effort**. Documented only as a
//!   "Warning message shown to the user", with no example pairing it with a
//!   PreToolUse response that omits `permissionDecision`, so its rendering on
//!   this non-blocking shape is undocumented. Unverified, not verified-absent —
//!   either way nothing here may depend on it.
//! * the durable append-only ledger ([`append`]) read back by
//!   `taintguard tally` — **the guaranteed human readout, and therefore the
//!   load-bearing one**. Counts of suppressed enforcements come from here.
//!
//! Observe-only suppresses the *enforcement*, never the *reporting*.
//!
//! # What observe-only does NOT suppress (changed in 0.1.6)
//!
//! **Only [`crate::state::Check::Tainted`] is ever suppressed.** A
//! [`crate::state::Check::Undetermined`] check — corrupt marker, wrong-schema
//! marker, unwritable store — resolves to `ask`/`deny` in *either* posture, as
//! does a panic in the barrier (see `main.rs`'s `analyse_gate`). Through 0.1.5
//! `Undetermined` was suppressed too, which was a fail-open dressed as a
//! measurement: this module's whole premise is "how much friction does enforcing
//! **known** taint cause", and an `Undetermined` finding names no sources at all
//! (its record's `sources` was always empty), so suppressing it produced no
//! measurable friction datum while collapsing cannot-determine into permission
//! (CLAUDE.md §3). "Could not determine" is the same class as "panicked", and
//! panic already enforced.
//!
//! Consequence for the numbers, stated here because it is easy to misread: an
//! enforced `Undetermined` under observe-only appends **no** ledger line. So the
//! ledger counts *suppressed enforcements*, which is **not** the same as *times
//! the gate fired* — see [`tally`] and README.ja.md. Recording it would have
//! inflated a counter whose name asserts suppression; the alternative, splitting
//! [`tally`]'s `suppressed` into three, would change this module's output
//! contract and was out of scope. The un-recorded event is still observable: it
//! produced a real `ask`/`deny`, and its reason carries
//! [`undetermined_not_suppressed_note`], which says both that the posture was not
//! honoured and that nothing was written here.
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
    ///
    /// Applies to a `Tainted` finding **only**. An `Undetermined` finding (and a
    /// panic) enforces even in this posture — see the module docs, "What
    /// observe-only does NOT suppress".
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
    /// `"external-read"`, `"lessons"`, `"internal-error"`). Never empty in a
    /// record written by 0.1.6+, because only a `Tainted` check is suppressed and
    /// a `Tainted` check names its sources. (Records written by ≤0.1.5 for an
    /// `Undetermined` check have this empty.)
    pub sources: Vec<String>,
    /// Always `"tainted"` in records written by 0.1.6+: `Undetermined` is no
    /// longer suppressed, so it never reaches the ledger (see the module docs).
    ///
    /// The field is **kept rather than dropped** because ledgers written by
    /// ≤0.1.5 can contain `"undetermined"` lines, and those must still
    /// deserialize — [`tally`] counts a line it cannot parse as `corrupt`, so
    /// removing this field would retroactively turn an operator's existing
    /// measurement into reported corruption.
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

/// The warning text for a suppressed enforcement.
///
/// States all three things the operator needs: that the turn IS tainted, which
/// sources tainted it, and that enforcement was suppressed *because
/// observe-only is active* (so the absence of an `ask`/`deny` is not mistaken
/// for a clean turn).
///
/// Where this text actually lands is NOT symmetrical, so this doc no longer
/// calls it "human/model-facing": it is carried both by the model-only
/// `additionalContext` and by the best-effort, undocumented-on-a-non-blocking-
/// response top-level `systemMessage` (see [`crate::hookio::observe_json`]).
/// Neither is a guaranteed human channel. The guaranteed one is the ledger read
/// back by `taintguard tally`; see the module docs.
pub fn warning(sources_desc: &str, tool: &str) -> String {
    format!(
        "[taintguard] OBSERVE-ONLY (measurement mode, {OBSERVE_ONLY_ENV}={OBSERVE_ONLY_OPT_IN}): \
         this turn consumed untrusted-provenance content ({sources_desc}). Normally this \
         `{tool}` call would be downgraded to ask/deny for the rest of the turn; \
         enforcement was SUPPRESSED because observe-only is active, and the event was \
         recorded to the observe-only ledger for counting. This is NOT a clean turn."
    )
}

/// The note appended to the fail-closed `ask`/`deny` reason when this process
/// **is** in observe-only but the check came back
/// [`crate::state::Check::Undetermined`] — the one finding observe-only does not
/// suppress.
///
/// Replaces the former `warning_undetermined`, which was the observe-mode
/// *warning* for that case. It had to go rather than be kept unused: its text
/// said "enforcement was SUPPRESSED because observe-only is active, and the event
/// was recorded to the observe-only ledger", and after 0.1.6 neither half is true
/// on this path — nothing is suppressed and nothing is recorded. Leaving that
/// prose in the tree would have been a CLAUDE.md §4 defect (a docstring telling a
/// safer story than the code), so the function is gone and this one took its
/// place.
///
/// It exists at all because an operator who deliberately exported
/// `TAINTGUARD_OBSERVE_ONLY=1` and then gets an `ask` needs to distinguish three
/// things: their posture being ignored, their posture being mis-parsed, and this
/// one path never honouring it on purpose. Saying so also documents the ledger
/// decision at the moment it matters, so nobody goes looking in `tally` for a
/// count that was never written.
pub fn undetermined_not_suppressed_note() -> String {
    format!(
        "(Observe-only IS set ({OBSERVE_ONLY_ENV}={OBSERVE_ONLY_OPT_IN}) but is deliberately NOT \
         honoured here: observe-only suppresses enforcement for KNOWN taint so its friction can \
         be measured, and a state that could not be determined names no sources, so suppressing \
         it would measure nothing while turning cannot-determine into permission — \
         cannot-determine always resolves to the restricted side. No observe-only ledger line \
         was written for this event either: the ledger counts SUPPRESSED enforcements, and this \
         one was not suppressed.)"
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

    /// `Posture` must not be obtainable permissively for free. Two separate
    /// claims, both asserted here — the second one used to be *documented
    /// intent only*, which is why it is spelled out:
    ///
    /// 1. `resolve`'s catch-all arm yields [`Posture::Enforce`].
    /// 2. `Posture` implements neither `Default` nor `From<bool>`, so a
    ///    permissive posture cannot appear via `Default::default()`, `.into()`,
    ///    or `?`-elision — it has to be written out by [`resolve`] after it has
    ///    seen the exact opt-in value (module docs, "Opt-in fails closed").
    ///
    /// Claim 2 is a *negative* trait property, which is not expressible as a
    /// bound, so it is probed by inherent-method shadowing (see the comment in
    /// the body). VERIFIED by fault injection: adding
    /// `impl Default for Posture { fn default() -> Self { Posture::ObserveOnly } }`
    /// turns this test RED (`Posture` must not implement `Default`); the
    /// previous version of this test stayed green under that same injection
    /// because its body only checked claim 1.
    #[test]
    fn enforce_is_the_catch_all_not_a_default() {
        // Claim 1 — `resolve` is the sole constructor path used by production
        // code, and its catch-all arm is Enforce.
        assert_eq!(resolve(None), Posture::Enforce);

        // Claim 2 — "`Posture` does NOT implement `Default`" cannot be written
        // as a where-clause, so it is observed instead: an *inherent*
        // associated fn is only a resolution candidate when its own
        // `where` clause holds, and it takes priority over a trait's default
        // body. So `Probe::<T>::has_default()` answers `true` exactly when
        // `T: Default` and falls back to `false` otherwise.
        struct Probe<T>(std::marker::PhantomData<T>);
        impl<T: Default> Probe<T> {
            fn has_default() -> bool {
                true
            }
        }
        impl<T: From<bool>> Probe<T> {
            fn has_from_bool() -> bool {
                true
            }
        }
        trait NoPermissiveCtor {
            fn has_default() -> bool {
                false
            }
            fn has_from_bool() -> bool {
                false
            }
        }
        impl<T> NoPermissiveCtor for Probe<T> {}

        // Positive control FIRST: a probe that always answered `false` would
        // make the two assertions below vacuous. `u8` has both impls.
        assert!(
            Probe::<u8>::has_default(),
            "the probe itself is broken: it must see `u8: Default`"
        );
        assert!(
            Probe::<u8>::has_from_bool(),
            "the probe itself is broken: it must see `u8: From<bool>`"
        );

        assert!(
            !Probe::<Posture>::has_default(),
            "`Posture` must NOT implement `Default`: a permissive posture would \
             become reachable via `Default::default()` / `?`-elision without \
             anyone having read the opt-in env value"
        );
        assert!(
            !Probe::<Posture>::has_from_bool(),
            "`Posture` must NOT implement `From<bool>`: a bool is exactly the \
             two-valued shape that lets `ObserveOnly` be produced by an \
             `.into()` far from the opt-in check"
        );
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
    /// land in ONE file so a project-wide fire-rate can be totalled.
    ///
    /// Scope is ALL this test checks. It does **not** call [`crate::state::clear`],
    /// so it says nothing about the related claim that the Stop hook cannot wipe
    /// the measurement — an earlier version of this docstring asserted that
    /// parenthetically and was wrong to (CLAUDE.md §4). That claim is pinned
    /// end-to-end, through the real binary's `clear` subcommand, by
    /// `stop_hook_clear_cannot_wipe_the_project_scoped_ledger` in
    /// `tests/provenance_gate.rs`.
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
    }

    /// The un-honoured-posture note must (a) name the posture, so the operator
    /// can tell an ignored posture from a deliberately-overridden one, and
    /// (b) state that nothing was written to the ledger, so nobody hunts in
    /// `tally` for a count that does not exist. It must NOT claim a suppression:
    /// this note is emitted on the path that ENFORCED.
    #[test]
    fn undetermined_note_names_the_posture_and_denies_both_suppression_and_recording() {
        let n = undetermined_not_suppressed_note();
        assert!(n.to_lowercase().contains("observe-only"));
        assert!(n.contains(OBSERVE_ONLY_ENV));
        assert!(n.contains("NOT honoured"));
        assert!(n.contains("ledger"));
        assert!(
            n.contains("not suppressed"),
            "the note must deny the suppression, not assert one: {n}"
        );
        assert!(
            !n.contains("was SUPPRESSED"),
            "this path enforced; the note must never say enforcement was suppressed: {n}"
        );
    }
}
