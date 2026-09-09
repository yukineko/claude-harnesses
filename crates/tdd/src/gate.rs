//! The test-existence gate: does the UNCOMMITTED diff add implementation code
//! with no test in it? Deterministic — it reads git, never runs the suite
//! (that's donegate's job). Its scope is the working tree only: the unstaged
//! and staged diffs plus untracked files, all relative to HEAD. Tests that are
//! already committed are never consulted, so every message this module renders
//! says "no test in the uncommitted changes" and not "this change has no
//! test" — the latter is an absence the gate never checked (CLAUDE.md §4).
//! The verdict drives whether the Stop hook blocks.

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
    pub fn has_test_evidence(&self) -> bool {
        self.test_marker_added || self.test_file_changed
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
        if is_test_file(file) {
            test_marker_added = true; // any added line in a test file is evidence
            continue;
        }
        if marker_hit {
            test_marker_added = true; // inline test written in an impl file
            continue;
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
                if f.has_test_evidence() {
                    if f.test_file_changed && f.test_marker_added {
                        "yes (test file + inline test)"
                    } else if f.test_file_changed {
                        "yes (test file changed)"
                    } else {
                        "yes (inline test added)"
                    }
                } else {
                    "none in the uncommitted changes"
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

    #[test]
    fn separate_test_file_is_evidence() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs", "tests/add_test.rs"]);
        let added = lines(vec![
            added("src/lib.rs", "pub fn add(a:i32,b:i32)->i32{a+b}"),
            added("tests/add_test.rs", "assert_eq!(add(1,2),3);"),
        ]);
        let v = classify(&cfg, &changed, &added);
        let Determination::Known(Some(f)) = &v.scan else {
            panic!("expected a known, scoped scan");
        };
        assert!(f.test_file_changed);
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
}
