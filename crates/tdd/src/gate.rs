//! The test-existence gate: does the UNCOMMITTED diff add implementation code
//! with no test in it? Deterministic — it reads git, never runs the suite
//! (that's donegate's job). Its scope is the working tree only: the unstaged
//! and staged diffs plus untracked files, all relative to HEAD. Tests that are
//! already committed are never consulted, so every message this module renders
//! says "no test in the uncommitted changes" and not "this change has no
//! test" — the latter is an absence the gate never checked (CLAUDE.md §4).
//! The verdict drives whether the Stop hook blocks.
//!
//! **The same limit applies on the other axis: authorship.** The scan is
//! `git status --porcelain` plus `git diff -U0` / `git diff --cached -U0` /
//! `git ls-files --others` (see [`crate::git`]) — commands that answer for the
//! whole checkout and carry no session identity. So `Fields::impl_files` is
//! every uncommitted implementation file, not "the files you wrote": in a
//! shared checkout it can hold a human's edit or a concurrent session's change
//! (CLAUDE.md §8 documents that this is measured, not hypothetical). tdd does
//! not narrow it — narrowing on a footprint the transcript may not carry would
//! drop real work and let untested code through — so it must instead SAY so,
//! exactly as the sibling gates do (`reviewgate::review::inject_reason`,
//! `propguard::gate::block_reason`). "N implementation lines added, write a
//! test" addressed to the agent is a claim about who added them, and that is
//! the second absence this module may not assert.

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use harness_core::verdict::{Determination, Verdict};
use regex::RegexSet;

use crate::config::Config;
use crate::git::{self, AddedLine, AddedScan, ChangeScan};

/// What a successful scan found. Only populated on `Scan::Known(Some(_))` —
/// i.e. a real git repo whose diff could be read.
pub struct Fields {
    /// Number of *added* implementation lines (impl-glob file, not a test file,
    /// not itself a test marker).
    pub added_impl_lines: usize,
    /// An added line matched a test marker (e.g. an inline `#[test]`).
    pub test_marker_added: bool,
    /// A changed/added file is a test by location/name.
    pub test_file_changed: bool,
    /// A few implementation files touched, for the message.
    pub impl_files: Vec<String>,
}

impl Fields {
    /// Evidence is an added line that MATCHED a test marker — nothing else.
    ///
    /// `test_file_changed` used to be an independent disjunct here, which made
    /// a path the gate's proof: appending one blank line to any file under
    /// `tests/` satisfied it and silenced the gate for the whole turn (backlog
    /// 1454ba50, measured). A path is not a test, and a deleted or renamed test
    /// file is the opposite of one. The field is still recorded — it is a true
    /// fact about the changeset and the message uses it — but it is no longer
    /// evidence on its own.
    pub fn has_test_evidence(&self) -> bool {
        self.test_marker_added
    }
}

/// What the git scan could determine, in the shared three-valued type
/// (`harness_core::verdict::Determination`) instead of a pair of home-grown
/// booleans (the former `git_unscoped` / `git_scan_failed`):
///
/// * `Known(Some(fields))` — the scan ran; `fields` is what it found.
/// * `Known(None)` — the scan ran and DETERMINED there is no repo here, so
///   there is nothing to scope against (allow, as before). A determined
///   answer, not a failure.
/// * `Undetermined(why)` — a `git` command errored inside a real repo. Not the
///   same fact as "no repo", and never reported as one: the changeset is
///   undetermined, so the gate fails closed rather than reading a collapsed
///   scan as "nothing changed".
pub type Scan = Determination<Option<Fields>>;

/// The gate's report: what the scan found, wrapped so the blocking decision
/// travels through the shared [`Verdict`] rather than a private bool.
pub struct Report {
    pub scan: Scan,
}

impl Report {
    /// The gate's answer in the shared type. `Known(None)` (confirmed no repo)
    /// and a `Known(Some(fields))` with test evidence both mint `Clean` via
    /// `from_findings(vec![])` — a real "ran, found nothing to block on".
    /// `Undetermined` passes the reason straight through: never collapsed into
    /// `Clean`.
    pub fn verdict(&self, cfg: &Config) -> Verdict {
        match &self.scan {
            Determination::Undetermined(why) => Verdict::Undetermined(why.clone()),
            Determination::Known(None) => Verdict::from_findings(Vec::new()),
            Determination::Known(Some(f)) => {
                let blocks =
                    f.added_impl_lines >= cfg.min_added_impl_lines.max(1) && !f.has_test_evidence();
                if blocks {
                    // State the observation, not an absence that was never
                    // checked. The scan reads `git status` + the unstaged and
                    // staged diffs + untracked files (`git.rs`) — all relative
                    // to HEAD — so it can report that no test appears in the
                    // uncommitted changes, and nothing about tests that are
                    // already committed. This reason travels to overwatch as a
                    // recorded finding, so the distinction has to survive here
                    // too, not only in the model-facing message.
                    Verdict::violation(format!(
                        "{} new implementation line(s) added, with no test visible in the \
                         uncommitted changes (already-committed tests were not consulted)",
                        f.added_impl_lines
                    ))
                } else {
                    Verdict::from_findings(Vec::new())
                }
            }
        }
    }

    /// Block the stop iff the shared verdict blocks (`Violation` or
    /// `Undetermined` — both restricted, never silently allowed).
    pub fn blocks(&self, cfg: &Config) -> bool {
        self.verdict(cfg).blocks()
    }
}

fn build_globset(globs: &[String]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        if let Ok(glob) = Glob::new(g) {
            b.add(glob);
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

fn build_markers(patterns: &[String]) -> RegexSet {
    RegexSet::new(patterns).unwrap_or_else(|_| {
        // Drop individually-invalid patterns rather than disabling all markers.
        let good: Vec<&String> = patterns
            .iter()
            .filter(|p| regex::Regex::new(p).is_ok())
            .collect();
        RegexSet::new(good).unwrap_or_else(|_| RegexSet::empty())
    })
}

/// Classify a set of changed files + added lines into a report. Pure (no git):
/// unit-testable. Maps the tri-state scans: any `Failed` → `Undetermined`
/// (fail closed); otherwise any `NotRepo` → `Known(None)` (allow, as before);
/// otherwise the successful `Files`/`Lines` sets drive the existing
/// test-evidence logic, carried as `Known(Some(fields))`.
pub fn classify(cfg: &Config, changed: &ChangeScan, added: &AddedScan) -> Report {
    let impl_set = build_globset(&cfg.impl_globs);
    let test_set = build_globset(&cfg.test_path_globs);
    let markers = build_markers(&cfg.test_markers);

    // A git command errored in a real repo → the changeset is undetermined.
    // Fail closed rather than let a collapsed-empty scan read as "no changes".
    // Checked first so a Failed scan is never masked by a NotRepo companion.
    if matches!(changed, ChangeScan::Failed) || matches!(added, AddedScan::Failed) {
        return Report {
            scan: Determination::undetermined(
                "a `git` command failed while scanning the changeset",
            ),
        };
    }

    let (ChangeScan::Files(changed), AddedScan::Lines(added)) = (changed, added) else {
        // At least one side is NotRepo (and neither is Failed): no git scope,
        // allow the stop exactly as before.
        return Report {
            scan: Determination::known(None),
        };
    };

    let is_test_file = |f: &str| test_set.is_match(f);
    let is_impl_file = |f: &str| impl_set.is_match(f) && !is_test_file(f);

    let test_file_changed = changed.iter().any(|f| is_test_file(f));

    let mut added_impl_lines = 0usize;
    let mut test_marker_added = false;
    let mut impl_files: Vec<String> = Vec::new();
    for AddedLine { file, text } in added {
        let marker_hit = markers.is_match(text);
        // One arm, one rule: the added line must MATCH A MARKER, and it must
        // land somewhere a test can live (an impl file — an inline `mod tests`
        // — or a test file).
        //
        // Both halves were learned by measurement. The marker requirement came
        // first (backlog 1ff0fcc9): the comment on the impl arm always said
        // "inline test written in an impl file", but the guard that would make
        // it true was missing, so a marker hit in ANY changed path counted —
        // the default set matches the bare substring `test(`, which reaches
        // CHANGELOG.md, a Cargo.toml comment, a JSON fixture or a shell script.
        // The test-file arm was the same hole with the halves swapped: it took
        // ANY added line, marker or not, so a comment, an import or a single
        // blank line under `tests/` was accepted as proof a test was written
        // (backlog 1454ba50 — probed against a real checkout, not reasoned).
        // "A regex matched somewhere" and "a path under tests/ was touched" are
        // both "cannot tell whether a test was added", and resolving that to
        // clean is the fail-open this repo exists to catch. Narrowing can only
        // BLOCK more, never less.
        if marker_hit && (is_impl_file(file) || is_test_file(file)) {
            test_marker_added = true;
            continue;
        }
        if is_test_file(file) {
            continue; // a test path, but this line is not a test
        }
        if is_impl_file(file) && !text.trim().is_empty() {
            added_impl_lines += 1;
            if !impl_files.contains(file) {
                impl_files.push(file.clone());
            }
        }
    }

    Report {
        scan: Determination::known(Some(Fields {
            added_impl_lines,
            test_marker_added,
            test_file_changed,
            impl_files,
        })),
    }
}

/// Run the gate against a real project root. The `Failed` arm (via `classify`)
/// fails the gate closed instead of collapsing an errored git command into an
/// empty, allow-shaped scan.
pub fn evaluate(cfg: &Config, root: &Path) -> Report {
    let changed = git::changed_files(root);
    let added = git::added_lines(root);
    classify(cfg, &changed, &added)
}

/// A stable short discriminator for a blocking report, for cross-gate
/// correlated-error detection (`overwatch::violation::RawViolation::check_kind`).
/// Only meaningful when the report actually blocks; distinguishes the two
/// distinct blocking reasons (missing test vs. an undetermined git scan)
/// rather than lumping every tdd block under one signature.
pub fn check_kind(v: &Report) -> &'static str {
    match &v.scan {
        Determination::Undetermined(_) => "git-scan-undetermined",
        _ => "missing-test",
    }
}

/// The reason injected back into the model when the stop is blocked.
pub fn block_reason(v: &Report, attempt: u32, max: u32) -> String {
    match &v.scan {
        // Undetermined changeset (a git command errored): a DISTINCT, loud
        // reason. We are not allowing the stop blindly on an empty/collapsed
        // scan; the existing escape hatches still apply so the turn is never
        // trapped.
        Determination::Undetermined(_) => format!(
            "🔴 tdd: couldn't determine what changed — a `git` command failed (attempt \
             {attempt}/{max}). Not allowing the stop blindly on an undetermined changeset \
             (that would let untested code through). Fix the git error (see `tdd status`), \
             or run `tdd skip --reason \"...\"` to skip once (THIS session only, recorded), \
             or set TDD_DISABLE=1 in the environment Claude Code itself was started with \
             (exporting it from a tool call does not reach this hook)."
        ),
        // Neither reachable in practice (a non-blocking report never reaches
        // `block_reason`), but resolved to the same loud message rather than an
        // empty string, since an empty block reason would be worse than either.
        Determination::Known(None) => generic_block_reason(0, &[], attempt, max),
        Determination::Known(Some(f)) => {
            generic_block_reason(f.added_impl_lines, &f.impl_files, attempt, max)
        }
    }
}

/// The model-facing block reason. It states two scope limits, and they are
/// **different claims that do not substitute for each other**: what tdd looked
/// at (the uncommitted changes — already-committed tests were not consulted)
/// and *whose* changes those are (unknown — authorship was never verified; see
/// the module docs). Neither is decoration: this string is the only thing the
/// agent reads, and both absences are ones the gate did not check.
///
/// The blocking predicate is untouched by either sentence — [`Report::blocks`]
/// still decides, and the whole unattributed set is still counted.
fn generic_block_reason(
    added_impl_lines: usize,
    impl_files: &[String],
    attempt: u32,
    max: u32,
) -> String {
    let sample = if impl_files.is_empty() {
        String::new()
    } else {
        let shown: Vec<&str> = impl_files.iter().take(6).map(String::as_str).collect();
        format!("\n  implementation changed: {}", shown.join(", "))
    };
    format!(
        "🔴 tdd: write a test first — {added_impl_lines} new implementation line(s) added, and no \
         test is visible in the uncommitted changes (attempt {attempt}/{max}).{sample}\n\n\
         Scope actually inspected: the working tree only — the unstaged and staged diffs plus \
         untracked files, all relative to HEAD. tdd did NOT read already-committed tests, so this \
         is \"no test in the uncommitted changes\", not \"this change has no test\". If the test \
         covering this change is already committed, say so and take the one-shot skip below.\n\n\
         Authorship was NOT verified. tdd's scan is `git status` / `git diff` / `git ls-files` \
         over the whole checkout, and none of those carries session identity, so the files \
         listed above are simply everything uncommitted — they may include a human's edits or \
         a concurrent session's changes as well as your own. tdd did not check who wrote them. \
         If a listed file is not yours, say so and take the one-shot skip below.\n\n\
         Add a test that exercises this change (a `#[test]`, `def test_…`, `func Test…`, \
         `it(...)`, or a file under tests/), then finish. Prefer test-first: run \
         `tdd red --task <id>` to capture the failing test before you implement, and \
         `tdd green --task <id>` once it passes.\n\n\
         Genuinely no test needed (pure refactor/rename/docs)? Run \
         `tdd skip --reason \"...\"` — one stop, THIS session only, and recorded. Disable \
         entirely: TDD_DISABLE=1 in the environment Claude Code itself was started with \
         (exporting it from a tool call does not reach this hook).",
    )
}

/// The notice printed when tdd concedes a stop it had already given up on and
/// another gate blocked that same stop (`run_guarded`'s concession
/// bookkeeping). Extracted from `main.rs` so its wording is unit-testable: it
/// describes what the scan observed, so this module's scope limits (see the
/// module docstring) bind it exactly as they bind [`block_reason`].
pub fn concession_reentry_notice() -> String {
    "tdd: already gave up on this stop (max_attempts exhausted) and another gate \
     blocked it, so the concession is still owed — allowing stop. When the give-up \
     was recorded, no test was visible in the uncommitted changes; already-committed \
     tests were not consulted, and the scan has not been re-run since."
        .to_string()
}

/// The notice printed when tdd exhausts `max_attempts` and allows the stop.
/// Extracted from `main.rs` for the same reason as
/// [`concession_reentry_notice`].
pub fn giveup_notice(max_attempts: u32) -> String {
    format!(
        "tdd: after {max_attempts} attempts a test is still not visible in the uncommitted \
         changes (already-committed tests were not consulted) — allowing stop. Add one, or set \
         TDD_DISABLE=1."
    )
}

/// Compact human report for manual `tdd gate` / `tdd status` runs.
pub fn human_report(v: &Report, cfg: &Config) -> String {
    match &v.scan {
        Determination::Undetermined(_) => "🔴 git scan FAILED (a git command errored) — tdd gate \
                                            BLOCKS the stop (undetermined changeset, failing \
                                            closed)"
            .to_string(),
        Determination::Known(None) => "(no git repo — tdd gate allows the stop)".to_string(),
        Determination::Known(Some(f)) => {
            let mut s = String::new();
            s.push_str(&format!("added impl lines: {}\n", f.added_impl_lines));
            s.push_str(&format!(
                "test evidence:    {}\n",
                // Report the observation, not the inference it invites
                // (CLAUDE.md 4). What was observed is that a marker regex
                // matched an added line in a file where a test can live; that
                // is not proof the line IS a test, so the wording does not say
                // so. The old wording named "test marker in an impl file" on a
                // branch that the test-file arm could also reach with zero
                // marker matches — an overclaim the gate's own message made
                // (backlog 860a78b3).
                if f.has_test_evidence() {
                    "yes (an added line matched a test marker)"
                } else if f.test_file_changed {
                    // Worth saying out loud: a test PATH was touched and still
                    // did not count, which is exactly the case a reader would
                    // otherwise assume was a bug.
                    "none — a test file changed, but no added line matched a test marker"
                } else {
                    "none in the uncommitted changes"
                }
            ));
            // Still reported, because it is a true fact about the changeset and
            // a reader looking for "but I did touch tests/" needs to see that
            // the gate saw it too. Kept on its own line, and labelled as not
            // being evidence, so it can never again be read as half of a claim
            // about a marker (backlog 1454ba50, 860a78b3).
            s.push_str(&format!(
                "test file:        {}\n",
                if f.test_file_changed {
                    "changed (a path, not a test — not evidence on its own)"
                } else {
                    "not changed"
                }
            ));
            if v.blocks(cfg) {
                s.push_str(
                    "\n🔴 would BLOCK: implementation lines added and no test visible in \
                     the uncommitted changes (already-committed tests were not consulted)",
                );
            } else {
                s.push_str("\n✓ would allow the stop");
            }
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn added(file: &str, text: &str) -> AddedLine {
        AddedLine {
            file: file.to_string(),
            text: text.to_string(),
        }
    }

    /// Build a successful `ChangeScan` from file names.
    fn files(names: &[&str]) -> ChangeScan {
        ChangeScan::Files(names.iter().map(|s| s.to_string()).collect())
    }

    /// Build a successful `AddedScan` from added lines.
    fn lines(v: Vec<AddedLine>) -> AddedScan {
        AddedScan::Lines(v)
    }

    #[test]
    fn impl_without_test_blocks() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs"]);
        let added = lines(vec![added(
            "src/lib.rs",
            "pub fn add(a:i32,b:i32)->i32{a+b}",
        )]);
        let v = classify(&cfg, &changed, &added);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert_eq!(f.added_impl_lines, 1);
        assert!(!f.has_test_evidence());
        assert!(v.blocks(&cfg));
    }

    #[test]
    fn inline_test_is_evidence() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs"]);
        let added = lines(vec![
            added("src/lib.rs", "pub fn add(a:i32,b:i32)->i32{a+b}"),
            added("src/lib.rs", "    #[test]"),
            added(
                "src/lib.rs",
                "    fn test_add() { assert_eq!(add(1,2),3); }",
            ),
        ]);
        let v = classify(&cfg, &changed, &added);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert!(f.test_marker_added);
        assert!(!v.blocks(&cfg));
    }

    /// The marker text used by the pair below. It matches the default marker
    /// `\b(it|test|describe)\s*\(` on the bare substring `test(` — the point is
    /// that ordinary prose reaches it, not that anyone wrote a test.
    const PROSE_WITH_A_MARKER: &str = "- changelog: reworked the test(x) helper";

    #[test]
    fn marker_in_an_impl_file_is_evidence_control() {
        // Anti-vacuity control for the next test: if PROSE_WITH_A_MARKER stopped
        // matching the marker set, the fail-open test below would pass for the
        // wrong reason (no hit at all, rather than a hit correctly ignored).
        let cfg = Config::default();
        let changed = files(&["src/lib.rs"]);
        let added = lines(vec![
            added("src/lib.rs", "pub fn add(a:i32,b:i32)->i32{a+b}"),
            added("src/lib.rs", PROSE_WITH_A_MARKER),
        ]);
        let v = classify(&cfg, &changed, &added);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert!(
            f.test_marker_added,
            "control failed: the marker text no longer matches, so the \
             fail-open test is vacuous"
        );
    }

    #[test]
    fn marker_in_a_non_impl_non_test_file_is_not_evidence() {
        // The fail-open (backlog 1ff0fcc9): the marker arm carried the comment
        // "inline test written in an impl file" but no is_impl_file guard, so a
        // regex hit in ANY changed path set test evidence and allowed the stop.
        // CHANGELOG.md is in neither impl_globs nor test_path_globs, and the
        // default marker set matches the bare substring `test(` — so one line of
        // release notes silenced the whole gate for the turn.
        let cfg = Config::default();
        let changed = files(&["src/lib.rs", "CHANGELOG.md"]);
        let added = lines(vec![
            added("src/lib.rs", "pub fn add(a:i32,b:i32)->i32{a+b}"),
            added("CHANGELOG.md", PROSE_WITH_A_MARKER),
        ]);
        let v = classify(&cfg, &changed, &added);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert_eq!(f.added_impl_lines, 1);
        assert!(
            !f.test_marker_added,
            "a marker hit outside both impl_globs and test_path_globs counted as \
             an inline test: 'cannot tell whether a test was added' resolved to \
             'clean'"
        );
        assert!(
            v.blocks(&cfg),
            "impl lines landed with no test anywhere, yet the gate allowed the stop"
        );
    }

    #[test]
    fn marker_in_a_manifest_is_not_evidence() {
        // Same shape, second surface: Cargo.toml is not an impl glob either.
        let cfg = Config::default();
        let changed = files(&["src/lib.rs", "Cargo.toml"]);
        let added = lines(vec![
            added("src/lib.rs", "pub fn add(a:i32,b:i32)->i32{a+b}"),
            added("Cargo.toml", "# bumped for the test(x) fix"),
        ]);
        let v = classify(&cfg, &changed, &added);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert!(!f.test_marker_added);
        assert!(v.blocks(&cfg));
    }

    #[test]
    fn separate_test_file_is_evidence() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs", "tests/add_test.rs"]);
        let added = lines(vec![
            added("src/lib.rs", "pub fn add(a:i32,b:i32)->i32{a+b}"),
            // Was `assert_eq!(add(1,2),3);` — a bare assert that matches NONE
            // of the default markers. The property this test names is "a real
            // test was added in a separate test file"; the old fixture only
            // represented "a line was added under tests/", which is the very
            // thing 1454ba50 removed from the evidence rule. The fixture is
            // what drifted from the name, so the fixture is what moved.
            added(
                "tests/add_test.rs",
                "#[test] fn test_add(){assert_eq!(add(1,2),3);}",
            ),
        ]);
        let v = classify(&cfg, &changed, &added);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert!(f.test_file_changed);
        assert!(
            f.test_marker_added,
            "a real #[test] added in a separate test file was not recorded as a marker"
        );
        assert!(!v.blocks(&cfg));
    }

    #[test]
    fn docs_only_change_allowed() {
        let cfg = Config::default();
        let changed = files(&["README.md"]);
        let added = lines(vec![added("README.md", "# hello")]);
        let v = classify(&cfg, &changed, &added);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert_eq!(f.added_impl_lines, 0);
        assert!(!v.blocks(&cfg));
    }

    #[test]
    fn no_git_never_blocks() {
        let cfg = Config::default();
        let v = classify(&cfg, &ChangeScan::NotRepo, &AddedScan::NotRepo);
        assert!(
            matches!(v.scan, Determination::Known(None)),
            "a NotRepo scan must record the confirmed-non-repo scope; got {:?}",
            describe(&v.scan)
        );
        assert!(!v.blocks(&cfg));
    }

    #[test]
    fn blank_added_lines_dont_count() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs"]);
        let added = lines(vec![added("src/lib.rs", "   ")]);
        let v = classify(&cfg, &changed, &added);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert_eq!(f.added_impl_lines, 0);
        assert!(!v.blocks(&cfg));
    }

    // ── fail-closed on an undetermined changeset ───────────────────────────
    //
    // The fail-open this fix closes: a `git` command errored inside a real
    // repo, the scan collapsed to empty, and the gate read that as "nothing
    // changed → allow". A `Failed` scan must BLOCK (undetermined ≠ clean),
    // exactly like a failing checker/reviewer subprocess already does.

    /// A `Failed` changed-files scan → `Undetermined` → BLOCK, even though
    /// nothing else is set (0 impl lines, no test). This is the regression.
    #[test]
    fn failed_change_scan_blocks_it_does_not_allow() {
        let cfg = Config::default();
        let v = classify(&cfg, &ChangeScan::Failed, &AddedScan::Lines(Vec::new()));
        assert!(
            matches!(v.scan, Determination::Undetermined(_)),
            "a failed git command must record an undetermined scan; got {:?}",
            describe(&v.scan)
        );
        assert!(
            !matches!(v.scan, Determination::Known(None)),
            "Failed is not the no-scope NotRepo case"
        );
        assert!(
            v.blocks(&cfg),
            "an undetermined changeset must fail the gate closed (block), not allow"
        );
    }

    /// A `Failed` added-lines scan alone also fails closed, and it wins over a
    /// NotRepo companion (Failed is checked first, never masked into allow).
    #[test]
    fn failed_added_scan_blocks_even_with_notrepo_companion() {
        let cfg = Config::default();
        let v = classify(&cfg, &ChangeScan::NotRepo, &AddedScan::Failed);
        assert!(matches!(v.scan, Determination::Undetermined(_)));
        assert!(
            v.blocks(&cfg),
            "Failed must not be masked by a NotRepo companion"
        );
    }

    /// The block_reason for a failed scan is a DISTINCT, loud message that still
    /// names every escape hatch, so the turn is never trapped.
    #[test]
    fn failed_scan_block_reason_is_distinct_and_escapable() {
        let v = classify(&Config::default(), &ChangeScan::Failed, &AddedScan::Failed);
        let reason = block_reason(&v, 1, 3);
        assert!(
            reason.contains("git"),
            "must name the git failure: {reason}"
        );
        assert!(
            reason.contains("tdd skip --reason"),
            "must name the one-shot skip: {reason}"
        );
        assert!(
            reason.contains("TDD_DISABLE"),
            "must name the disable hatch: {reason}"
        );
    }

    /// Non-regression: a CLEAN repo (successful, EMPTY `Files`/`Lines`) must
    /// still ALLOW — the fix must not turn clean trees red.
    #[test]
    fn clean_repo_empty_scan_still_allows() {
        let cfg = Config::default();
        let v = classify(
            &cfg,
            &ChangeScan::Files(Vec::new()),
            &AddedScan::Lines(Vec::new()),
        );
        assert!(
            !matches!(v.scan, Determination::Undetermined(_)),
            "an empty SUCCESS is not a failure"
        );
        assert!(
            matches!(v.scan, Determination::Known(Some(_))),
            "a clean repo is in scope, just with no changes; got {:?}",
            describe(&v.scan)
        );
        assert!(
            !v.blocks(&cfg),
            "a genuinely clean repo must still be allowed"
        );
    }

    /// The gate's answer travels in the shared type: blocking yields
    /// `Violation` and the Stop-hook channel of that type actually blocks;
    /// an allow yields the unforgeable `Clean`.
    #[test]
    fn verdict_is_the_shared_type() {
        let cfg = Config::default();
        let blocking = classify(
            &cfg,
            &files(&["src/lib.rs"]),
            &lines(vec![added("src/lib.rs", "pub fn f(){}")]),
        );
        let v = blocking.verdict(&cfg);
        assert!(matches!(v, Verdict::Violation(_)), "got {v:?}");
        assert!(v.blocks());
        let decision = v
            .stop_decision()
            .expect("a blocking verdict emits a decision");
        assert_eq!(decision["decision"], "block");

        let clean = classify(
            &cfg,
            &ChangeScan::Files(Vec::new()),
            &AddedScan::Lines(Vec::new()),
        );
        assert!(matches!(clean.verdict(&cfg), Verdict::Clean(_)));
        assert!(clean.verdict(&cfg).stop_decision().is_none());

        let undetermined = classify(&cfg, &ChangeScan::Failed, &AddedScan::Failed);
        assert!(matches!(
            undetermined.verdict(&cfg),
            Verdict::Undetermined(_)
        ));
        assert!(undetermined.verdict(&cfg).blocks());
    }

    fn describe(scan: &Scan) -> &'static str {
        match scan {
            Determination::Known(Some(_)) => "Known(Some)",
            Determination::Known(None) => "Known(None)",
            Determination::Undetermined(_) => "Undetermined",
        }
    }

    // ══════════════════════════════════════════════════════════════════════
    // CLAUDE.md §4 — "no accompanying test" is a claim tdd never checked.
    //
    // `crate::git::added_lines` reads `git diff -U0`, `git diff --cached -U0`
    // and `git ls-files --others --exclude-standard` (git.rs:136-145);
    // `crate::git::changed_files` reads `git status --porcelain=v1 -z`
    // (git.rs:90). That is the UNCOMMITTED working-tree + index + untracked
    // change set and nothing else — no `git log`, no `HEAD` comparison. A
    // test that is already committed — written first, in an
    // earlier commit, exactly as this repo's own F→P discipline requires — is
    // invisible to the scan. The gate is entitled to say "I saw implementation
    // lines and no test IN WHAT I INSPECTED". It is not entitled to say the
    // change has no test.
    //
    // The verdict predicate is NOT changing: the same input must still block,
    // and the message must still name the lines and files actually observed.
    // Those are the anti-vacuity controls below.
    // ══════════════════════════════════════════════════════════════════════

    /// Ways an implementer may name what was actually inspected.
    /// NOTE: `"uncommitted".contains("committed")` is true, so the
    /// already-committed tokens below deliberately avoid the bare word.
    const TDD_INSPECTED_SCOPE_TOKENS: &[&str] = &[
        "uncommitted",
        "not yet committed",
        "working tree",
        "working-tree",
        "未コミット",
        "コミットされていない",
    ];

    /// Ways an implementer may say already-committed tests were not consulted.
    const TDD_COMMITTED_NOT_CONSULTED_TOKENS: &[&str] = &[
        "already committed",
        "already-committed",
        "previously committed",
        "existing commits",
        "コミット済み",
        "既にコミット",
        "既存のコミット",
    ];

    /// The report the gate builds for "implementation added, no test visible in
    /// the uncommitted diff" — the input that blocks today and must keep
    /// blocking after the wording fix.
    fn missing_test_report() -> Report {
        Report {
            scan: Determination::Known(Some(Fields {
                added_impl_lines: 12,
                test_marker_added: false,
                test_file_changed: false,
                impl_files: vec!["src/foo.rs".to_string(), "src/bar.rs".to_string()],
            })),
        }
    }

    /// The same shape WITH test evidence — the control proving the predicate
    /// still discriminates.
    fn report_with_test_evidence() -> Report {
        Report {
            scan: Determination::Known(Some(Fields {
                added_impl_lines: 12,
                test_marker_added: true,
                test_file_changed: false,
                impl_files: vec!["src/foo.rs".to_string()],
            })),
        }
    }

    /// (a) The dishonest claim, in the model-facing block reason.
    #[test]
    fn block_reason_does_not_assert_the_change_has_no_test() {
        let reason = block_reason(&missing_test_report(), 1, 3);
        let lower = reason.to_lowercase();
        assert!(
            !lower.contains("no accompanying test"),
            "tdd only inspected the UNCOMMITTED diff; it cannot assert the change has \
             no accompanying test.\n--- reason ---\n{reason}"
        );
        assert!(
            !lower.contains("without a test"),
            "same claim, different phrasing — tdd did not look at committed tests.\n\
             --- reason ---\n{reason}"
        );
    }

    /// (a, positive half) It must state what it DID inspect, and that
    /// already-committed tests were not consulted.
    #[test]
    fn block_reason_states_that_only_the_uncommitted_changes_were_inspected() {
        let reason = block_reason(&missing_test_report(), 1, 3);
        let lower = reason.to_lowercase();
        assert!(
            TDD_INSPECTED_SCOPE_TOKENS
                .iter()
                .any(|t| lower.contains(&t.to_lowercase())),
            "the block reason must name the scope it actually inspected — the \
             uncommitted/working-tree changes (one of {TDD_INSPECTED_SCOPE_TOKENS:?}).\n\
             --- reason ---\n{reason}"
        );
        assert!(
            TDD_COMMITTED_NOT_CONSULTED_TOKENS
                .iter()
                .any(|t| lower.contains(&t.to_lowercase())),
            "the block reason must say already-committed tests were NOT consulted \
             (one of {TDD_COMMITTED_NOT_CONSULTED_TOKENS:?}).\n--- reason ---\n{reason}"
        );
    }

    /// (a) The same dishonest claim in the `Verdict::Violation` reason
    /// (gate.rs line ~69), which travels to overwatch as the recorded finding.
    #[test]
    fn verdict_reason_does_not_assert_the_change_has_no_test() {
        let cfg = Config::default();
        let v = missing_test_report().verdict(&cfg);
        assert!(
            v.blocks(),
            "the verdict predicate must not change: this still blocks"
        );
        let reason = v
            .reason()
            .map(|r| r.as_str().to_string())
            .unwrap_or_default();
        assert!(
            !reason.to_lowercase().contains("no accompanying test"),
            "the recorded violation reason asserts an absence tdd never checked.\n\
             --- reason ---\n{reason}"
        );
        // ANTI-VACUITY for this assertion: the reason must still carry the
        // number of implementation lines tdd genuinely counted.
        assert!(
            reason.contains("12"),
            "the violation reason must still report the 12 added implementation lines \
             it really observed.\n--- reason ---\n{reason}"
        );
    }

    /// (b) ANTI-VACUITY #1. The block reason must still carry the facts tdd
    /// really did observe, and the escape hatches.
    #[test]
    fn block_reason_still_names_the_observed_lines_files_and_escape_hatches() {
        let reason = block_reason(&missing_test_report(), 1, 3);
        for tok in [
            "12",
            "src/foo.rs",
            "src/bar.rs",
            "tdd skip",
            "TDD_DISABLE",
            "1/3",
        ] {
            assert!(
                reason.contains(tok),
                "the block reason must still contain {tok:?}\n--- reason ---\n{reason}"
            );
        }
    }

    /// (b) ANTI-VACUITY #2. The verdict predicate is unchanged: the same input
    /// still blocks, and test evidence still allows.
    #[test]
    fn the_blocking_predicate_is_unchanged_by_the_wording_fix() {
        let cfg = Config::default();
        assert!(
            missing_test_report().blocks(&cfg),
            "impl lines with no visible test must STILL block"
        );
        assert!(
            !report_with_test_evidence().blocks(&cfg),
            "a change with test evidence must STILL be allowed"
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // CLAUDE.md §4 — the SAME unchecked-absence claim, in the two notices that
    // were lifted verbatim out of `main.rs` and so never received the wording
    // correction `block_reason` / `human_report` already carry.
    //
    // The scope argument is identical (see the banner above): the scan reads
    // the porcelain status, the unstaged/staged diffs and the untracked files,
    // all relative to HEAD, and never the commit log. So neither notice may
    // assert that there IS no test; each may only report the absence it
    // observed IN THE SCOPE IT INSPECTED. The sibling crate donegate words its
    // equivalent give-up notices that way already ("The checks were RED when
    // the give-up was recorded and have not been re-run" — it names what it
    // observed, not what it concluded).
    //
    // These are wording tests only — no verdict predicate is involved — so the
    // anti-vacuity halves below carry the whole burden of stopping a future
    // author from "fixing" the claim by deleting the sentence.
    // ══════════════════════════════════════════════════════════════════════

    /// (a) The concession notice must not assert an absence tdd never checked.
    /// The UNQUALIFIED claim is rejected; a properly SCOPED one ("no test was
    /// visible in the uncommitted changes") is what the positive half demands.
    #[test]
    fn concession_reentry_notice_does_not_assert_an_unchecked_absence_of_a_test() {
        let notice = concession_reentry_notice();
        let lower = notice.to_lowercase();
        for bare in ["there was still no test when", "still no test when"] {
            assert!(
                !lower.contains(bare),
                "the concession notice states the absence of a test as an unqualified \
                 fact ({bare:?}); tdd inspected only the UNCOMMITTED changes and never \
                 read already-committed tests.\n--- notice ---\n{notice}"
            );
        }
        assert!(
            TDD_INSPECTED_SCOPE_TOKENS
                .iter()
                .any(|t| lower.contains(&t.to_lowercase())),
            "where the concession notice reports having found no test it must name the \
             scope it actually inspected — the uncommitted/working-tree changes (one of \
             {TDD_INSPECTED_SCOPE_TOKENS:?}).\n--- notice ---\n{notice}"
        );
        assert!(
            TDD_COMMITTED_NOT_CONSULTED_TOKENS
                .iter()
                .any(|t| lower.contains(&t.to_lowercase())),
            "the concession notice must say already-committed tests were NOT consulted \
             (one of {TDD_COMMITTED_NOT_CONSULTED_TOKENS:?}).\n--- notice ---\n{notice}"
        );
    }

    /// (b) ANTI-VACUITY for the concession notice: scoping the claim must not
    /// become an excuse to drop the two facts the notice genuinely reports —
    /// that the concession is still owed, and that the stop is being allowed.
    #[test]
    fn concession_reentry_notice_still_reports_the_owed_concession_and_the_allowed_stop() {
        let notice = concession_reentry_notice();
        let lower = notice.to_lowercase();
        assert!(
            lower.contains("concession") && lower.contains("owed"),
            "the concession notice must still say the concession is still owed\n\
             --- notice ---\n{notice}"
        );
        assert!(
            lower.contains("allowing stop"),
            "the concession notice must still say it is ALLOWING the stop\n\
             --- notice ---\n{notice}"
        );
    }

    /// (a) The give-up notice carries the same unchecked claim ("still no test
    /// after N attempts"). Same rule: reject the unqualified absolute, require
    /// the scope to be named wherever the absence is reported.
    #[test]
    fn giveup_notice_does_not_assert_an_unchecked_absence_of_a_test() {
        let notice = giveup_notice(3);
        let lower = notice.to_lowercase();
        for bare in ["still no test after", "no test after 3 attempts"] {
            assert!(
                !lower.contains(bare),
                "the give-up notice states the absence of a test as an unqualified fact \
                 ({bare:?}); tdd inspected only the UNCOMMITTED changes, so a test that \
                 landed in an earlier commit is invisible to it.\n\
                 --- notice ---\n{notice}"
            );
        }
        assert!(
            TDD_INSPECTED_SCOPE_TOKENS
                .iter()
                .any(|t| lower.contains(&t.to_lowercase())),
            "where the give-up notice reports having found no test it must name the \
             scope it actually inspected — the uncommitted/working-tree changes (one of \
             {TDD_INSPECTED_SCOPE_TOKENS:?}).\n--- notice ---\n{notice}"
        );
        assert!(
            TDD_COMMITTED_NOT_CONSULTED_TOKENS
                .iter()
                .any(|t| lower.contains(&t.to_lowercase())),
            "the give-up notice must say already-committed tests were NOT consulted \
             (one of {TDD_COMMITTED_NOT_CONSULTED_TOKENS:?}).\n--- notice ---\n{notice}"
        );
    }

    /// (b) ANTI-VACUITY for the give-up notice: it must keep the attempt count
    /// it was handed, the TDD_DISABLE=1 escape hatch, and the fact that it is
    /// allowing the stop. Deleting the sentence is not a fix.
    #[test]
    fn giveup_notice_still_names_the_attempt_count_and_escape_hatch() {
        let notice = giveup_notice(3);
        for tok in ["3", "TDD_DISABLE=1"] {
            assert!(
                notice.contains(tok),
                "the give-up notice must still contain {tok:?}\n--- notice ---\n{notice}"
            );
        }
        assert!(
            notice.to_lowercase().contains("allowing stop"),
            "the give-up notice must still say it is ALLOWING the stop\n\
             --- notice ---\n{notice}"
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // Unverified-authorship disclosure for the `impl_files` list rendered in
    // `generic_block_reason`'s "implementation changed: …" line.
    //
    // `git::changed_files` (git.rs:90, `git status --porcelain=v1 -z`) and
    // `git::added_lines` (git.rs:136-145, `git diff -U0` / `git diff --cached
    // -U0` / `git ls-files --others --exclude-standard`) all read the WHOLE
    // working tree relative to HEAD. None of them carries session identity —
    // no `git blame`, no author, no session id — so the files this gate lists
    // as "implementation changed" may not be the agent's own edits: a human
    // may have touched them, or a concurrent/parallel session sharing this
    // checkout (CLAUDE.md §8: another session must ALWAYS be assumed to
    // exist). Two sibling gates already carry this disclosure for the
    // identically-unattributed file list they render from the same kind of
    // scan (`crate::git::changed_files` equivalents):
    //   - reviewgate, review.rs:510-514
    //   - propguard, gate.rs:831-834
    // tdd's `impl_files` list is built from that same unattributed vector
    // (`Fields::impl_files`, populated in `classify` from `added_lines`) and
    // carries no such disclosure today.
    // ══════════════════════════════════════════════════════════════════════

    /// Ways an implementer may name authorship / who made the change.
    const TDD_AUTHORSHIP_TOKENS: &[&str] = &[
        "authorship",
        "author",
        "who wrote",
        "who made",
        "who authored",
        "who edited",
    ];

    /// Ways an implementer may say that authorship was not verified.
    const TDD_UNVERIFIED_TOKENS: &[&str] = &[
        "not verified",
        "not verify",
        "unverified",
        "did not verify",
        "does not verify",
        "never verified",
        "doesn't verify",
    ];

    /// Ways an implementer may say the list can include edits that are not
    /// the agent's own — a human's, or a concurrent/parallel session's.
    const TDD_HUMAN_OR_CONCURRENT_TOKENS: &[&str] = &[
        "human",
        "concurrent session",
        "parallel session",
        "another session",
        "other session",
        "concurrent/parallel session",
    ];

    /// A report whose `impl_files` list is real files from THIS worktree that
    /// the test process itself did not create — the same shape sibling gates
    /// use (`.githooks/pre-push` in reviewgate/propguard's own tests) to make
    /// the point concrete: this exact list can legitimately contain a
    /// human-edited file, and the gate has no way to tell.
    fn missing_test_report_with_unattributed_files() -> Report {
        Report {
            scan: Determination::Known(Some(Fields {
                added_impl_lines: 12,
                test_marker_added: false,
                test_file_changed: false,
                impl_files: vec!["src/foo.rs".to_string(), ".githooks/pre-push".to_string()],
            })),
        }
    }

    /// (a) THE DISHONEST CLAIM. `block_reason`, exercised through the real
    /// `Report` → `block_reason` path (not a private helper called with
    /// hand-made args), must disclose that authorship of the listed impl
    /// files was never verified AND that the list may therefore include a
    /// human's or a concurrent/parallel session's edits. Today it only says
    /// "implementation changed: <files>", asserting nothing about authorship
    /// but also disclosing nothing — an agent reading it has no way to know
    /// the list might not be its own edits. EXPECTED RED.
    #[test]
    fn block_reason_discloses_unverified_authorship_of_the_impl_files_list() {
        let reason = block_reason(&missing_test_report_with_unattributed_files(), 1, 3);
        let lower = reason.to_lowercase();
        assert!(
            TDD_AUTHORSHIP_TOKENS.iter().any(|t| lower.contains(t)),
            "the block reason must name authorship (one of \
             {TDD_AUTHORSHIP_TOKENS:?}) when it lists implementation files it never \
             attributed.\n--- reason ---\n{reason}"
        );
        assert!(
            TDD_UNVERIFIED_TOKENS.iter().any(|t| lower.contains(t)),
            "the block reason must say authorship was NOT verified (one of \
             {TDD_UNVERIFIED_TOKENS:?}).\n--- reason ---\n{reason}"
        );
        assert!(
            TDD_HUMAN_OR_CONCURRENT_TOKENS
                .iter()
                .any(|t| lower.contains(t)),
            "the block reason must say the file list may include edits by the human \
             or a concurrent/parallel session (one of \
             {TDD_HUMAN_OR_CONCURRENT_TOKENS:?}) — tdd's scan carries no session \
             identity (git.rs `changed_files`/`added_lines`).\n--- reason ---\n{reason}"
        );
    }

    /// (b1) ANTI-VACUITY. The disclosure must not come at the cost of the
    /// facts tdd genuinely observed: the added-line count and every impl file
    /// it actually passed in.
    #[test]
    fn block_reason_with_disclosure_still_names_the_lines_and_every_impl_file() {
        let report = missing_test_report_with_unattributed_files();
        let reason = block_reason(&report, 1, 3);
        assert!(
            reason.contains("12"),
            "the block reason must still contain the observed added-line count \
             (12).\n--- reason ---\n{reason}"
        );
        let Determination::Known(Some(fields)) = &report.scan else {
            unreachable!("fixture is Known(Some(_))");
        };
        for f in &fields.impl_files {
            assert!(
                reason.contains(f.as_str()),
                "the block reason must still list every impl file passed in, \
                 including {f:?}\n--- reason ---\n{reason}"
            );
        }
    }

    /// (b2) ANTI-VACUITY. The disclosure must not come at the cost of asking
    /// for a test and naming both escape hatches.
    #[test]
    fn block_reason_with_disclosure_still_asks_for_a_test_and_names_both_escape_hatches() {
        let reason = block_reason(&missing_test_report_with_unattributed_files(), 1, 3);
        let lower = reason.to_lowercase();
        assert!(
            lower.contains("test"),
            "the block reason must still ask for a test.\n--- reason ---\n{reason}"
        );
        assert!(
            reason.contains("tdd skip"),
            "the block reason must still name the `tdd skip` escape hatch.\n\
             --- reason ---\n{reason}"
        );
        assert!(
            reason.contains("TDD_DISABLE=1"),
            "the block reason must still name the TDD_DISABLE=1 escape hatch.\n\
             --- reason ---\n{reason}"
        );
    }

    /// (b3) ANTI-VACUITY. This new authorship disclosure is a DIFFERENT claim
    /// from the existing "scope inspected" disclosure
    /// (`TDD_INSPECTED_SCOPE_TOKENS` / `TDD_COMMITTED_NOT_CONSULTED_TOKENS`,
    /// pinned by `block_reason_states_that_only_the_uncommitted_changes_were_inspected`
    /// above): one is about WHAT tdd looked at (uncommitted vs. committed),
    /// the other is about WHO made the changes it looked at (the agent vs. a
    /// human/concurrent session). Neither substitutes for the other, so this
    /// test pins that the pre-existing scope sentence is untouched by the new
    /// requirement — (a) cannot be satisfied merely by relabeling it.
    #[test]
    fn block_reason_still_states_the_uncommitted_only_scope_alongside_the_new_disclosure() {
        let reason = block_reason(&missing_test_report_with_unattributed_files(), 1, 3);
        let lower = reason.to_lowercase();
        assert!(
            TDD_INSPECTED_SCOPE_TOKENS
                .iter()
                .any(|t| lower.contains(&t.to_lowercase())),
            "the block reason must still name the scope it actually inspected — the \
             uncommitted/working-tree changes (one of {TDD_INSPECTED_SCOPE_TOKENS:?}) — \
             this is a DIFFERENT claim from the authorship disclosure.\n\
             --- reason ---\n{reason}"
        );
        assert!(
            TDD_COMMITTED_NOT_CONSULTED_TOKENS
                .iter()
                .any(|t| lower.contains(&t.to_lowercase())),
            "the block reason must still say already-committed tests were NOT \
             consulted (one of {TDD_COMMITTED_NOT_CONSULTED_TOKENS:?}).\n\
             --- reason ---\n{reason}"
        );
    }

    /// (b4) ANTI-VACUITY. The verdict predicate is unchanged by the wording
    /// fix: a report with `added_impl_lines >= 1` and no test evidence still
    /// blocks, and one with test evidence still does not. This duplicates the
    /// existing `the_blocking_predicate_is_unchanged_by_the_wording_fix` test
    /// above (same claim); it is written again here, under its own name and
    /// against the unattributed-files fixture, so it stands as an
    /// independent anti-vacuity control specifically for this authorship-
    /// disclosure change rather than relying on an assertion written for a
    /// different fix.
    #[test]
    fn authorship_disclosure_does_not_change_the_blocking_predicate() {
        let cfg = Config::default();
        assert!(
            missing_test_report_with_unattributed_files().blocks(&cfg),
            "impl lines with no visible test must STILL block, unattributed file \
             list notwithstanding"
        );
        assert!(
            !report_with_test_evidence().blocks(&cfg),
            "a change with test evidence must STILL be allowed"
        );
    }

    /// (b5) ANTI-VACUITY. The reason is not empty and remains a substantial
    /// instruction, so an empty string (or a one-line stub) cannot vacuously
    /// satisfy (a)'s token checks by virtue of never being compared against
    /// anything real.
    #[test]
    fn block_reason_with_disclosure_remains_a_substantial_instruction() {
        let reason = block_reason(&missing_test_report_with_unattributed_files(), 1, 3);
        assert!(
            !reason.is_empty(),
            "the block reason must not be empty\n--- reason ---\n{reason}"
        );
        assert!(
            reason.len() > 200,
            "the block reason must remain a substantial instruction (>200 chars), \
             not a stub that happens to contain the required tokens \
             (len={})\n--- reason ---\n{reason}",
            reason.len()
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // `human_report`'s test-evidence line (f667897d). Before this commit the
    // marker-only branch printed "yes (inline test added)" and the combined
    // branch printed "yes (test file + inline test)" -- both asserting an
    // inline TEST was observed when all `classify` ever recorded is a marker
    // REGEX HIT (CLAUDE.md §4: prose must not claim more than was checked).
    // Nothing exercised `human_report` before this pair was added -- no test
    // in this file called it at all -- so the wording fix itself had zero
    // kill rate.
    // ══════════════════════════════════════════════════════════════════════

    #[test]
    fn human_report_marker_only_evidence_does_not_overclaim_an_inline_test() {
        let cfg = Config::default();
        let report = report_with_test_evidence(); // test_marker_added, no test file
        let out = human_report(&report, &cfg);
        assert!(
            !out.contains("inline test added"),
            "human_report claimed an inline TEST was observed, but classify only \
             recorded a marker regex hit in an impl file\n--- report ---\n{out}"
        );
        assert!(
            out.contains("an added line matched a test marker"),
            "human_report must describe what was actually observed (a marker \
             regex hit on an added line), not what it invites the reader to \
             infer\n--- report ---\n{out}"
        );
        // Re-anchored from the old literal "test marker in an impl file".
        // That phrase was itself the overclaim 860a78b3 filed: it named a FILE
        // CLASS the code does not record, on a branch the test-file arm could
        // also reach with zero marker matches. The named property (do not claim
        // an inline test was observed) is asserted above and is unchanged; only
        // the string standing in for "what was observed" moved, and it moved
        // toward the observation.
        assert!(
            !out.contains("in an impl file"),
            "human_report named a file class classify() never recorded\n\
             --- report ---\n{out}"
        );
    }

    #[test]
    fn human_report_combined_evidence_does_not_overclaim_an_inline_test() {
        let cfg = Config::default();
        let report = Report {
            scan: Determination::Known(Some(Fields {
                added_impl_lines: 3,
                test_marker_added: true,
                test_file_changed: true,
                impl_files: vec!["src/foo.rs".to_string()],
            })),
        };
        let out = human_report(&report, &cfg);
        assert!(
            !out.contains("inline test"),
            "human_report claimed an inline TEST was observed in the combined \
             (test file + marker) branch, but classify only recorded a marker \
             regex hit\n--- report ---\n{out}"
        );
        // Re-anchored alongside its sibling above: both observations are still
        // required to appear, but each is now stated as the thing classify()
        // actually recorded, on its own line, so neither can be read as
        // qualifying the other.
        assert!(
            out.contains("an added line matched a test marker"),
            "human_report dropped the marker observation\n--- report ---\n{out}"
        );
        assert!(
            out.contains("test file:        changed"),
            "human_report dropped the changed-test-file observation\n\
             --- report ---\n{out}"
        );
        assert!(
            !out.contains("in an impl file"),
            "human_report named a file class classify() never recorded\n\
             --- report ---\n{out}"
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // What counts as TEST EVIDENCE: at least one ADDED line that MATCHES a
    // configured test-marker regex, in a file that is either an impl-glob or a
    // test-glob file. A path, on its own, is not evidence; a blank line is not
    // evidence; an import or a comment is not evidence.
    //
    // Today the gate answers a different question. `Fields::has_test_evidence`
    // (gate.rs:48-50) is `self.test_marker_added || self.test_file_changed`;
    // `test_file_changed` (gate.rs:167) is set by a test-glob path merely
    // APPEARING in the changed set; and the first arm of the `classify` loop
    // (gate.rs:174-177) sets `test_marker_added` for ANY added line in a test
    // file — no marker check, no blank-line guard. Measured consequence: one
    // blank line added to any `tests/` path, or a test path in the changed set
    // contributing no added lines at all, silences the gate for the whole turn.
    // That is "I cannot tell whether a test was added" resolving to "clean" —
    // the fail-open CLAUDE.md §3 names.
    //
    // The controls in this block are anti-vacuity: (a), (b) and (f) must pass
    // BEFORE and AFTER the change, so a "fix" that simply never finds evidence
    // cannot satisfy this suite.
    // ══════════════════════════════════════════════════════════════════════

    /// One implementation line, so every case below has something to block on
    /// (`min_added_impl_lines` defaults to 1).
    const EVIDENCE_IMPL_LINE: &str = "pub fn add(a:i32,b:i32)->i32{a+b}";

    /// A line that DOES match the default marker set (`\bfn\s+test_`).
    const EVIDENCE_MARKER_LINE: &str = "    fn test_add() { assert_eq!(add(1,2),3); }";

    /// Lines that match NO default marker — an import and a comment: exactly
    /// the kind of edit that lands in a test file without adding a test.
    const EVIDENCE_NON_MARKER_LINES: &[&str] = &["use crate::add;", "    // fixture for the adder"];

    /// ANTI-VACUITY for (c): the "non-marker" fixture text above must really
    /// contain no marker. Placed in an IMPL file, where the marker arm is
    /// already guarded today, so this control passes before and after the
    /// change. If someone edits `EVIDENCE_NON_MARKER_LINES` into something that
    /// happens to match a marker, (c) would pass for the wrong reason and this
    /// control is what catches it.
    #[test]
    fn control_non_marker_fixture_text_really_matches_no_marker() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs"]);
        let mut v = vec![added("src/lib.rs", EVIDENCE_IMPL_LINE)];
        for l in EVIDENCE_NON_MARKER_LINES {
            v.push(added("src/lib.rs", l));
        }
        let report = classify(&cfg, &changed, &lines(v));
        let Determination::Known(Some(f)) = &report.scan else {
            panic!("expected a known, scoped scan");
        };
        assert!(
            !f.test_marker_added,
            "control failed: the supposedly marker-free fixture text now matches a \
             test marker, so the non-marker cases below are vacuous"
        );
    }

    /// (a) CONTROL — a marker-matching added line in an IMPL file IS evidence,
    /// and the stop is allowed. True today; must stay true. Without this, a
    /// change that always answered "no evidence" would satisfy (c)/(d)/(e).
    #[test]
    fn control_marker_line_in_an_impl_file_is_evidence() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs"]);
        let added_lines = lines(vec![
            added("src/lib.rs", EVIDENCE_IMPL_LINE),
            added("src/lib.rs", EVIDENCE_MARKER_LINE),
        ]);
        let v = classify(&cfg, &changed, &added_lines);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert!(
            f.test_marker_added,
            "control failed: a marker-matching added line in an impl-glob file must \
             set the marker flag"
        );
        assert!(
            f.has_test_evidence(),
            "control failed: a marker-matching added line in an impl-glob file is \
             test evidence"
        );
        assert!(
            !v.blocks(&cfg),
            "control failed: an inline test written in an impl file must still allow \
             the stop — tightening evidence must not turn a real test into a block"
        );
    }

    /// (b) CONTROL — a marker-matching added line in a TEST file IS evidence,
    /// and the stop is allowed. True today (for the wrong reason: the
    /// unconditional test-file arm), and it must stay true once the marker
    /// check is added, which is what makes it a control rather than a
    /// regression test.
    ///
    /// Deliberately asserts `has_test_evidence()` rather than naming which
    /// private flag carries the evidence: the contract is "this is evidence",
    /// not "this particular boolean is the one that records it".
    #[test]
    fn control_marker_line_in_a_test_file_is_evidence() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs", "tests/add_test.rs"]);
        let added_lines = lines(vec![
            added("src/lib.rs", EVIDENCE_IMPL_LINE),
            added("tests/add_test.rs", EVIDENCE_MARKER_LINE),
        ]);
        let v = classify(&cfg, &changed, &added_lines);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert!(
            f.has_test_evidence(),
            "control failed: a marker-matching added line in a test-glob file is test \
             evidence"
        );
        assert!(
            !v.blocks(&cfg),
            "control failed: a real test added in a test file must still allow the \
             stop"
        );
    }

    /// (c) A NON-marker added line in a test file is NOT evidence. An import or
    /// a comment added to `tests/add_test.rs` is not a test; treating it as one
    /// lets implementation land with nothing exercising it. EXPECTED RED: the
    /// first arm of `classify`'s loop (gate.rs:174-177) sets
    /// `test_marker_added` for ANY added line in a test file.
    #[test]
    fn non_marker_line_in_a_test_file_is_not_evidence() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs", "tests/add_test.rs"]);
        let mut v = vec![added("src/lib.rs", EVIDENCE_IMPL_LINE)];
        for l in EVIDENCE_NON_MARKER_LINES {
            v.push(added("tests/add_test.rs", l));
        }
        let report = classify(&cfg, &changed, &lines(v));
        let Determination::Known(Some(f)) = &report.scan else {
            panic!("expected a known, scoped scan");
        };
        assert_eq!(f.added_impl_lines, 1);
        assert!(
            !f.test_marker_added,
            "an added line in a test-glob file that matches NO test marker (an \
             import, a comment) was recorded as a test marker: 'cannot tell whether \
             a test was added' resolved to 'clean'"
        );
        assert!(
            !f.has_test_evidence(),
            "an import and a comment added to a test file counted as test evidence; \
             evidence is a marker-matching added line, not any edit to a test path"
        );
        assert!(
            report.blocks(&cfg),
            "an implementation line landed with no marker-matching added line \
             anywhere, yet the gate allowed the stop"
        );
    }

    /// (d) A BLANK added line in a test file is NOT evidence. This is the
    /// cheapest possible silencer: one newline appended to any `tests/` path.
    /// EXPECTED RED, same unguarded arm as (c) — and note the blank-line guard
    /// that `blank_added_lines_dont_count` already pins for impl files
    /// (gate.rs:190) has no counterpart on the test-file arm.
    #[test]
    fn blank_line_in_a_test_file_is_not_evidence() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs", "tests/add_test.rs"]);
        let added_lines = lines(vec![
            added("src/lib.rs", EVIDENCE_IMPL_LINE),
            added("tests/add_test.rs", ""),
            added("tests/add_test.rs", "   "),
        ]);
        let v = classify(&cfg, &changed, &added_lines);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert_eq!(f.added_impl_lines, 1);
        assert!(
            !f.test_marker_added,
            "a blank added line in a test-glob file was recorded as a test marker: \
             whitespace is not a test"
        );
        assert!(
            !f.has_test_evidence(),
            "appending a blank line to a test file counted as test evidence, so one \
             newline silences the gate for the whole turn"
        );
        assert!(
            v.blocks(&cfg),
            "an implementation line landed and the only thing added to the test file \
             was whitespace, yet the gate allowed the stop"
        );
    }

    /// (e) A test-glob path merely PRESENT in the changed set, contributing
    /// ZERO added lines, is NOT evidence. This is the `test_file_changed`
    /// half of `has_test_evidence` (gate.rs:49, set at gate.rs:167 from the
    /// changed-file list alone): a deletion, a rename, a mode change, or a
    /// `git add` of an unchanged path puts a test path in the changed set
    /// without a single added line. EXPECTED RED.
    #[test]
    fn a_test_glob_path_with_zero_added_lines_is_not_evidence() {
        let cfg = Config::default();
        // `tests/add_test.rs` is in the changed set but contributes no added
        // lines at all — every added line belongs to the impl file.
        let changed = files(&["src/lib.rs", "tests/add_test.rs"]);
        let added_lines = lines(vec![added("src/lib.rs", EVIDENCE_IMPL_LINE)]);
        let v = classify(&cfg, &changed, &added_lines);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert_eq!(f.added_impl_lines, 1);
        assert!(
            !f.has_test_evidence(),
            "a test-glob path appearing in the changed set with ZERO added lines \
             counted as test evidence; a path is not a test, and a deleted or \
             renamed test file is the opposite of one"
        );
        assert!(
            v.blocks(&cfg),
            "an implementation line landed and no test line was added anywhere, yet \
             the gate allowed the stop because a test path appeared in the changed \
             set"
        );
    }

    /// (f) CONTROL — a marker-matching added line in a file that is NEITHER
    /// impl-glob NOR test-glob is not evidence. `marker_in_a_non_impl_non_test_file_is_not_evidence`
    /// and `marker_in_a_manifest_is_not_evidence` already pin this for
    /// CHANGELOG.md and Cargo.toml; this adds the third surface named in the
    /// contract, a JSON fixture, and must keep passing after the change.
    #[test]
    fn control_marker_line_in_a_json_fixture_is_not_evidence() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs", "fixtures/cases.json"]);
        let added_lines = lines(vec![
            added("src/lib.rs", EVIDENCE_IMPL_LINE),
            added("fixtures/cases.json", "  {\"name\": \"describe(add)\"},"),
        ]);
        let v = classify(&cfg, &changed, &added_lines);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert_eq!(f.added_impl_lines, 1);
        assert!(
            !f.test_marker_added,
            "a marker hit outside both impl_globs and test_path_globs counted as an \
             inline test: 'cannot tell whether a test was added' resolved to 'clean'"
        );
        assert!(
            !f.has_test_evidence(),
            "a JSON fixture line counted as test evidence"
        );
        assert!(
            v.blocks(&cfg),
            "impl lines landed with no test anywhere, yet the gate allowed the stop"
        );
    }

    // ── the same three cases, at the VERDICT layer ────────────────────────
    //
    // The three tests above assert the field-level property first, so Rust
    // stops there and the user-visible half — "does the gate actually block?" —
    // is never observed failing. These companions assert only the verdict, so
    // the consequence that matters to a user (a stop allowed with untested
    // implementation in the tree) is itself observed RED rather than inferred
    // from the flag.

    /// (c) at the verdict layer. EXPECTED RED.
    #[test]
    fn non_marker_line_in_a_test_file_still_blocks_the_stop() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs", "tests/add_test.rs"]);
        let mut v = vec![added("src/lib.rs", EVIDENCE_IMPL_LINE)];
        for l in EVIDENCE_NON_MARKER_LINES {
            v.push(added("tests/add_test.rs", l));
        }
        let report = classify(&cfg, &changed, &lines(v));
        assert!(
            matches!(report.verdict(&cfg), Verdict::Violation(_)),
            "an import and a comment added to a test file allowed the stop: the gate \
             read 'a test path was edited' as 'a test was added'"
        );
        assert!(
            report.blocks(&cfg),
            "implementation landed with no marker-matching added line anywhere and \
             the stop was allowed"
        );
    }

    /// (d) at the verdict layer. EXPECTED RED.
    #[test]
    fn blank_line_in_a_test_file_still_blocks_the_stop() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs", "tests/add_test.rs"]);
        let added_lines = lines(vec![
            added("src/lib.rs", EVIDENCE_IMPL_LINE),
            added("tests/add_test.rs", ""),
            added("tests/add_test.rs", "   "),
        ]);
        let v = classify(&cfg, &changed, &added_lines);
        assert!(
            matches!(v.verdict(&cfg), Verdict::Violation(_)),
            "one blank line appended to a test file allowed the stop — the cheapest \
             possible way to silence the gate for a whole turn"
        );
        assert!(
            v.blocks(&cfg),
            "implementation landed and the only thing added to the test file was \
             whitespace, and the stop was allowed"
        );
    }

    /// (e) at the verdict layer. EXPECTED RED.
    #[test]
    fn a_test_glob_path_with_zero_added_lines_still_blocks_the_stop() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs", "tests/add_test.rs"]);
        let added_lines = lines(vec![added("src/lib.rs", EVIDENCE_IMPL_LINE)]);
        let v = classify(&cfg, &changed, &added_lines);
        assert!(
            matches!(v.verdict(&cfg), Verdict::Violation(_)),
            "a test-glob path in the changed set contributing ZERO added lines \
             allowed the stop; a path is not a test"
        );
        assert!(
            v.blocks(&cfg),
            "implementation landed, no test line was added anywhere, and the stop was \
             allowed because a test path appeared in the changed set"
        );
    }

    /// CONTROL for the three verdict companions above: the same verdict layer
    /// must still ALLOW when a real marker-matching line is added to the test
    /// file. Without this, a change that made the gate block unconditionally
    /// would satisfy them.
    #[test]
    fn control_marker_line_in_a_test_file_still_allows_the_stop() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs", "tests/add_test.rs"]);
        let added_lines = lines(vec![
            added("src/lib.rs", EVIDENCE_IMPL_LINE),
            added("tests/add_test.rs", EVIDENCE_MARKER_LINE),
        ]);
        let v = classify(&cfg, &changed, &added_lines);
        assert!(
            matches!(v.verdict(&cfg), Verdict::Clean(_)),
            "control failed: a real test added in a test file must still mint Clean"
        );
        assert!(
            !v.blocks(&cfg),
            "control failed: a real test added in a test file must still allow the stop"
        );
    }
}
