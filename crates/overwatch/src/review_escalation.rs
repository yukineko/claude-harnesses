//! Foreign-file bridge: read condukt's durable escalation queue
//! (`escalate.rs`) into the overwatch review-queue as a new
//! [`crate::review_queue::EntryKind::Escalation`] source.
//!
//! # Why a foreign-file read, not a crate dependency
//!
//! The workspace's dependency direction is `harness-core <- overwatch <-
//! blastguard <- condukt`. condukt already depends on overwatch, so
//! `overwatch` taking a `condukt` crate dependency would be a cycle. Instead
//! this module reads condukt's `escalations.json` **by path**, as fail-soft
//! foreign JSON, using ONLY `harness_core` primitives — the SAME
//! `harness_core::config::base_dir` / `harness_core::projkey::{repo_root,
//! project_key}` symbols [`crate::store`] already uses to derive overwatch's
//! own storage root.
//!
//! # "Could not read" is not "nobody is asking"
//!
//! An escalation is condukt's record that a worker STOPPED and a human has to
//! answer. Reporting a queue we could not read as an empty queue does not lose
//! a statistic — it loses the question, and the run it belongs to waits on an
//! answer nobody will ever be shown. So every reader here is three-valued, on
//! the SAME shared type the rest of this crate uses
//! ([`harness_core::verdict::Determination`], as in [`crate::store`]'s
//! `scan_*` readers), and draws the same line
//! [`harness_core::boundary::read_to_string`] draws:
//!
//! | situation | answer |
//! |---|---|
//! | condukt's `escalations.json` is not there | `Known(vec![])` — a real zero |
//! | it is there and parses | `Known(open rows)` |
//! | it is there but cannot be read (permissions, non-UTF-8, I/O) | `Undetermined` |
//! | it is there but does not parse as a registry | `Undetermined` |
//!
//! The first row is deliberately NOT undetermined: most projects never run
//! condukt, and answering "undetermined" for all of them would make the value
//! mean nothing (and would fire the queue's warning on every unrelated repo).
//! Absent and unreadable are different observations, and this module keeps them
//! different.
//!
//! # Known limitation
//!
//! This reads condukt's **default** state path
//! (`~/.condukt/state/<project-key>/escalations.json`). If a user overrides
//! `state_dir` in `~/.condukt/config.toml`, those escalations live elsewhere
//! and will simply not be found here — the absent-file case above, `Known` and
//! empty. Parsing condukt's own config across the crate boundary is
//! deliberately out of scope, to keep this coupling to the minimal file
//! contract described below. This is the one place where a genuinely-nonempty
//! queue can still read as a trustworthy zero, and it is a limitation of the
//! PATH, not of the read.
//!
//! # File contract
//!
//! condukt's on-disk shape (`crates/condukt/src/escalate.rs`) is
//! `Registry { escalations: Vec<Escalation> }`. [`ConduktEscalation`] is a
//! MINIMAL mirror carrying only the fields this bridge needs (`id`, `run`,
//! `task`, `question`, `resolved`, `created_at`); condukt's other fields
//! (`options`, `recommended`, `chosen`) are silently ignored by serde. Every
//! mirrored field is `#[serde(default)]` so a partial/older condukt record
//! (or a future condukt version that drops a field) still parses instead of
//! failing the whole read.

use harness_core::verdict::Determination;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Minimal mirror of condukt's `escalate::Escalation` — only the fields
/// overwatch's review-queue needs. See the module doc for the cross-tool file
/// contract this mirrors.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConduktEscalation {
    /// Stable content-hash id assigned by condukt.
    #[serde(default)]
    pub id: String,
    /// The condukt run this question belongs to.
    #[serde(default)]
    pub run: String,
    /// The task within the run that is blocked on this answer.
    #[serde(default)]
    pub task: String,
    /// The question being asked.
    #[serde(default)]
    pub question: String,
    /// Whether a human has already answered.
    #[serde(default)]
    pub resolved: bool,
    /// When the escalation was enqueued (unix seconds).
    #[serde(default)]
    pub created_at: i64,
}

/// Mirror of condukt's on-disk `Registry` wrapper: an object with an
/// `escalations` array.
#[derive(Debug, Default, Deserialize)]
struct Registry {
    #[serde(default)]
    escalations: Vec<ConduktEscalation>,
}

/// Parse condukt's `escalations.json` text and return only the still-OPEN
/// (`!resolved`) records. PURE and total: it never panics, and it never answers
/// with an empty registry it did not actually read. `source` names the origin
/// for the diagnostic (the path, when the text came from one).
///
/// * text that parses → `Known(open rows)` (possibly a real, trustworthy empty)
/// * text that does not parse — corrupt, truncated, or blank → `Undetermined`
///
/// A text that does not parse is `Undetermined`, never an empty registry: the
/// records it WOULD have held are exactly the ones a reader is about to
/// conclude do not exist. That includes EMPTY/blank text, which is not a valid
/// registry either — condukt writes this file whole (`to_string_pretty` into a
/// temp file, then rename, see its `escalate.rs::save`), so a genuinely empty
/// queue on disk is `{"escalations": []}` and a zero-byte file is a truncated
/// or half-written artefact, not a statement that nothing is pending.
///
/// The absent-FILE case is a property of the read, not of any text, and lives
/// in [`scan_open_escalations`] — which is this function's only production
/// caller, so the two can never disagree about what a given text holds.
pub fn parse_open_escalations(txt: &str, source: &str) -> Determination<Vec<ConduktEscalation>> {
    match serde_json::from_str::<Registry>(txt) {
        Ok(reg) => Determination::known(
            reg.escalations
                .into_iter()
                .filter(|e| !e.resolved)
                .collect(),
        ),
        Err(e) => Determination::undetermined(format!(
            "condukt's escalation registry at {source} does not parse: {e} — reading it as \
             an empty queue would report a blocked human question as no question at all"
        )),
    }
}

/// Derive the DEFAULT path to condukt's `escalations.json` for the project
/// rooted at (or above) `cwd`:
/// `harness_core::config::base_dir("condukt")/state/<project-key>/escalations.json`
/// — mirroring condukt's own `escalate.rs::escalations_path` /
/// `config.rs::base_dir` derivation, and reusing the exact `harness_core`
/// symbols [`crate::store`] already calls for overwatch's own storage root.
/// See the module doc for the known non-default-`state_dir` limitation.
pub fn condukt_escalations_path(cwd: &Path) -> PathBuf {
    let base = harness_core::config::base_dir("condukt");
    let repo_root = harness_core::projkey::repo_root(cwd);
    let project_key = harness_core::projkey::project_key(&repo_root);
    base.join("state")
        .join(project_key)
        .join("escalations.json")
}

/// Read condukt's open escalations for the project at `cwd` — the sanctioned
/// reader, and the only one whose answer may feed a decision.
///
/// * no file (condukt never ran here, or never escalated) → `Known(vec![])`.
///   A real observation of zero: nothing was ever written, so nothing is
///   pending. See the module note for why this arm is NOT undetermined.
/// * file read and parsed → `Known(open rows)`.
/// * file present but unreadable (permissions, non-UTF-8, I/O) →
///   `Undetermined`, forwarded verbatim from
///   [`harness_core::boundary::read_to_string`] and deliberately not re-minted,
///   so the undetermined telemetry counts this opacity once.
/// * file present but not a parseable registry → `Undetermined` (see
///   [`parse_open_escalations`]).
pub fn scan_open_escalations(cwd: &Path) -> Determination<Vec<ConduktEscalation>> {
    let path = condukt_escalations_path(cwd);
    match harness_core::boundary::read_to_string(&path) {
        Determination::Known(None) => Determination::known(Vec::new()),
        Determination::Known(Some(txt)) => {
            parse_open_escalations(&txt, &path.display().to_string())
        }
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// BEST-EFFORT read: [`scan_open_escalations`] with the undetermined arm
/// flattened back to an empty vec.
///
/// It CANNOT distinguish "condukt has no open escalations" from "the escalation
/// queue could not be read", and a caller that renders its result as the
/// escalation source reports a lost human question as no question. That
/// collapse is the whole reason [`scan_open_escalations`] exists.
///
/// **`#[cfg(test)]` since t3: it has no production caller left.** It was the
/// two-valued shim `review-queue` and `--to-backlog` used while they were being
/// migrated; both now handle the third answer themselves (they warn and omit
/// the source), so the shim survives only as the test-only witness that the two
/// contracts are deliberately DIFFERENT — `..._collapses_where_the_scan_does_not`
/// asserts the collapse beside the scan's tri-state answer. Compiling it out of
/// the binary is what keeps the doc true: no future call site can reach for it
/// on the strength of a comment asking it not to.
#[cfg(test)]
pub fn read_open_escalations_best_effort(cwd: &Path) -> Vec<ConduktEscalation> {
    match scan_open_escalations(cwd) {
        Determination::Known(rows) => rows,
        Determination::Undetermined(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every reader test below asserts BOTH polarities, because each alone is
    // satisfiable by a broken implementation: one that answered `Undetermined`
    // for everything passes the corrupt/unreadable arms while blinding the
    // queue, and one that answered `Known(vec![])` for everything passes the
    // absent/clean arms while being the fail-open this module just removed.

    /// A registry the reader must be able to read back: one open question, one
    /// already answered.
    const TWO_ROWS_ONE_OPEN: &str = r#"{
        "escalations": [
            {"id": "esc-1", "run": "r1", "task": "t1", "question": "Q1", "options": ["a","b"], "recommended": 0, "created_at": 100, "resolved": false},
            {"id": "esc-2", "run": "r1", "task": "t2", "question": "Q2", "options": ["a"], "recommended": 0, "created_at": 200, "resolved": true, "chosen": "a"}
        ]
    }"#;

    /// The observed rows, failing the test if the answer was `Undetermined`.
    /// (Written with `assert!` + `matches!` rather than `panic!` because the
    /// workspace denies `clippy::panic` for this crate, tests included.)
    #[track_caller]
    fn known(d: Determination<Vec<ConduktEscalation>>, what: &str) -> Vec<ConduktEscalation> {
        assert!(
            matches!(d, Determination::Known(_)),
            "{what}: expected a real observation, got {d:?} — answering Undetermined for a \
             queue that WAS read destroys the signal just as thoroughly as reading an \
             unreadable one as empty"
        );
        match d {
            Determination::Known(rows) => rows,
            // Unreachable: the assertion above has already failed the test.
            Determination::Undetermined(_) => Vec::new(),
        }
    }

    #[track_caller]
    fn assert_undetermined(d: Determination<Vec<ConduktEscalation>>, what: &str) {
        assert!(
            matches!(d, Determination::Undetermined(_)),
            "{what}: a queue that could not be read IN FULL must not answer Known — \
             \"could not read\" is not \"nobody is asking\" — got {d:?}"
        );
    }

    /// Run `f` with `$HOME` pointed at a fresh temp dir (so
    /// `condukt_escalations_path` resolves inside it and the real
    /// `~/.condukt` is never touched), handing it a project cwd underneath.
    ///
    /// `$HOME` is restored and the crate-wide lock released BEFORE the caller
    /// asserts on the returned value: a panic while holding [`crate::store::
    /// HOME_ENV_LOCK`] poisons it for every later `$HOME` test in the process,
    /// which reports one real red as a pile of `PoisonError`s that say nothing
    /// about the property each test checks. The lock is taken poison-recovering
    /// for the same reason (it guards a process-global env var, not invariants
    /// of a data structure).
    fn with_sandboxed_home<T>(tag: &str, f: impl FnOnce(&Path) -> T) -> T {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);

        let guard = crate::store::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HOME");
        let home = std::env::temp_dir().join(format!(
            "overwatch-escalation-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let cwd = home.join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        std::env::set_var("HOME", &home);

        let out = f(&cwd);

        match prev {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(&home);
        out
    }

    /// Write condukt's registry file for `cwd` (creating its parents).
    fn write_registry(cwd: &Path, bytes: &[u8]) {
        let path = condukt_escalations_path(cwd);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
    }

    // ── parse ───────────────────────────────────────────────────────────────

    #[test]
    fn parse_open_escalations_keeps_only_unresolved() {
        let open = known(
            parse_open_escalations(TWO_ROWS_ONE_OPEN, "<test text>"),
            "a registry that parses cleanly",
        );
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "esc-1");
        assert!(!open[0].resolved);
    }

    #[test]
    fn parse_open_escalations_clean_but_genuinely_empty_registry_is_known_empty() {
        // ANTI-VACUITY, the other side of the two tests below: a registry that
        // WAS read and really holds nothing must stay a usable, trustworthy
        // empty. If this collapsed into Undetermined too, every project that
        // has simply answered all its questions would look unreadable and the
        // distinction would stop meaning anything.
        let open = known(
            parse_open_escalations(r#"{"escalations": []}"#, "<test text>"),
            "an empty-but-valid registry",
        );
        assert!(open.is_empty());
    }

    #[test]
    fn parse_open_escalations_corrupt_text_is_undetermined() {
        // Was `parse_open_escalations_corrupt_text_is_empty`, which asserted
        // the OPPOSITE: it pinned the fail-open as the specification. Corrupt
        // text is not an empty queue; it is a queue whose contents are unknown,
        // and the records it would have held are exactly the human questions a
        // caller is about to conclude do not exist.
        assert_undetermined(
            parse_open_escalations("not json at all {{{", "<test text>"),
            "a registry text that does not parse",
        );
    }

    #[test]
    fn parse_open_escalations_empty_text_is_undetermined() {
        // Same reversal, same reason. condukt writes this file whole (pretty
        // JSON into a temp file, then rename), so a zero-byte registry is a
        // truncated write, never how "no open escalations" is spelled on disk —
        // that is `{"escalations": []}` (asserted above) or no file at all
        // (asserted below).
        assert_undetermined(
            parse_open_escalations("", "<test text>"),
            "blank registry text",
        );
    }

    #[test]
    fn parse_open_escalations_missing_fields_are_missing_tolerant() {
        // A partial/older record (missing `id`, extra unknown fields ignored)
        // must still parse rather than failing the whole read.
        let txt = r#"{"escalations": [{"run": "r1", "task": "t1", "question": "Q", "created_at": 1, "resolved": false, "extra_condukt_only_field": 42}]}"#;
        let open = known(
            parse_open_escalations(txt, "<test text>"),
            "a partial/older record",
        );
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, "");
        assert_eq!(open[0].run, "r1");
    }

    // ── read ────────────────────────────────────────────────────────────────

    #[test]
    fn scan_open_escalations_is_tri_state() {
        // All four arms in one sandbox, in the order a real project passes
        // through them.
        let (absent, clean, corrupt, unreadable) = with_sandboxed_home("tri", |cwd| {
            let absent = scan_open_escalations(cwd);
            write_registry(cwd, TWO_ROWS_ONE_OPEN.as_bytes());
            let clean = scan_open_escalations(cwd);
            write_registry(cwd, b"{\"escalations\": [ truncated mid-writ");
            let corrupt = scan_open_escalations(cwd);
            // Non-UTF-8 bytes: the file EXISTS but cannot be read as text.
            // Deterministic for every user, unlike `chmod 000` (a no-op as root).
            write_registry(cwd, &[0xFFu8, 0xFE, 0xFD]);
            let unreadable = scan_open_escalations(cwd);
            (absent, clean, corrupt, unreadable)
        });

        // ANTI-VACUITY 1: condukt was never here. A real, trustworthy zero —
        // if this were Undetermined, every project that does not use condukt
        // would report an unreadable escalation queue forever.
        assert!(
            known(absent, "no escalations.json at all").is_empty(),
            "an absent registry is a real observation of zero, not an opacity"
        );
        // ANTI-VACUITY 2: a readable registry still yields its open row.
        let rows = known(clean, "a registry that reads and parses");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "esc-1");

        assert_undetermined(corrupt, "a registry present but unparseable");
        assert_undetermined(unreadable, "a registry present but unreadable as text");
    }

    #[test]
    fn read_open_escalations_best_effort_collapses_where_the_scan_does_not() {
        // Pins the shim's documented contract BESIDE the scan's, so the two
        // read as deliberately different rather than accidentally the same.
        // The remaining fail-open at the review-queue / --to-backlog call sites
        // is exactly this line, and it is visible here rather than implied.
        let (scan, shim, absent_shim) = with_sandboxed_home("shim", |cwd| {
            let absent_shim = read_open_escalations_best_effort(cwd);
            write_registry(cwd, &[0xFFu8, 0xFE, 0xFD]);
            (
                scan_open_escalations(cwd),
                read_open_escalations_best_effort(cwd),
                absent_shim,
            )
        });

        assert_undetermined(scan, "the sanctioned reader on an unreadable registry");
        assert!(
            shim.is_empty(),
            "the best-effort shim is documented to flatten Undetermined into an empty \
             vec; if that ever changes, its doc and its two call sites must change with it"
        );
        assert!(
            absent_shim.is_empty(),
            "and an absent registry is empty through the shim too — the shim's defect is \
             that these two ANSWERS ARE THE SAME, which is what the assertions above show"
        );
    }

    // ── the RED probes, kept ────────────────────────────────────────────────
    //
    // These two were written BEFORE the fix, deliberately phrased so they
    // compiled against the old two-valued signatures and failed at RUNTIME
    // (rather than as a compile error): they assert only that "could not read"
    // is DISTINGUISHABLE from "read, held nothing", never naming the type that
    // carries the distinction. They are kept verbatim as the provenance of the
    // RED observation — the first is byte-for-byte the test that failed; the
    // second differs only in the reader it names, since the function it
    // originally called (`read_open_escalations`) no longer exists.

    #[test]
    fn corrupt_text_is_distinguishable_from_a_clean_empty_registry() {
        let clean = parse_open_escalations(r#"{"escalations": []}"#, "<test text>");
        let corrupt = parse_open_escalations("not json at all {{{", "<test text>");
        assert_ne!(
            format!("{clean:?}"),
            format!("{corrupt:?}"),
            "a registry that could not be PARSED must not answer the same thing as one \
             that parsed cleanly and held no open escalation — a human question would \
             vanish from the queue"
        );
    }

    #[test]
    fn unreadable_file_is_distinguishable_from_an_absent_one() {
        let (absent, unreadable) = with_sandboxed_home("red", |cwd| {
            let absent = scan_open_escalations(cwd);
            write_registry(cwd, &[0xFFu8, 0xFE, 0xFD]);
            (absent, scan_open_escalations(cwd))
        });
        assert_ne!(
            format!("{absent:?}"),
            format!("{unreadable:?}"),
            "an escalations.json that could not be READ must not answer the same thing \
             as one that was never written — condukt's blocked questions would silently \
             become 'nobody is asking anything'"
        );
    }

    #[test]
    fn condukt_escalations_path_is_rooted_under_condukt_state() {
        let cwd = std::env::temp_dir();
        let path = condukt_escalations_path(&cwd);
        assert!(path.ends_with("escalations.json"));
        let s = path.to_string_lossy();
        assert!(s.contains(".condukt"));
        assert!(s.contains("state"));
    }
}
