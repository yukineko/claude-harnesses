//! The test-existence gate: did this change add implementation code without a
//! test? Deterministic — it reads git, never runs the suite (that's donegate's
//! job). The verdict drives whether the Stop hook blocks.

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::RegexSet;

use crate::config::Config;
use crate::git::{self, AddedLine, AddedScan, ChangeScan};

pub struct Verdict {
    /// Number of *added* implementation lines (impl-glob file, not a test file,
    /// not itself a test marker).
    pub added_impl_lines: usize,
    /// An added line matched a test marker (e.g. an inline `#[test]`).
    pub test_marker_added: bool,
    /// A changed/added file is a test by location/name.
    pub test_file_changed: bool,
    /// A few implementation files touched, for the message.
    pub impl_files: Vec<String>,
    /// True when there is no git repo, so we can't scope and allow the stop.
    pub git_unscoped: bool,
    /// True when a `git` command errored inside a real repo, so the changed set
    /// is UNDETERMINED. We must not treat that as "nothing changed → allow";
    /// the gate fails closed (blocks) — the same treatment the crate already
    /// gives a failing checker/reviewer subprocess.
    pub git_scan_failed: bool,
}

impl Verdict {
    pub fn has_test_evidence(&self) -> bool {
        self.test_marker_added || self.test_file_changed
    }

    /// Block the stop iff a git scan was undetermined (fail closed), OR enough
    /// new implementation landed with no test.
    pub fn blocks(&self, cfg: &Config) -> bool {
        if self.git_scan_failed {
            // Undetermined changeset → fail closed. Never silently allow: a git
            // command that errored is not evidence of a clean tree.
            return true;
        }
        !self.git_unscoped
            && self.added_impl_lines >= cfg.min_added_impl_lines.max(1)
            && !self.has_test_evidence()
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

/// Classify a set of changed files + added lines into a verdict. Pure (no git):
/// unit-testable. Maps the tri-state scans: any `Failed` → `git_scan_failed`
/// (block, fail closed); otherwise any `NotRepo` → `git_unscoped` (allow, as
/// before); otherwise the successful `Files`/`Lines` sets drive the existing
/// test-evidence logic.
pub fn classify(cfg: &Config, changed: &ChangeScan, added: &AddedScan) -> Verdict {
    let impl_set = build_globset(&cfg.impl_globs);
    let test_set = build_globset(&cfg.test_path_globs);
    let markers = build_markers(&cfg.test_markers);

    // A git command errored in a real repo → the changeset is undetermined.
    // Fail closed rather than let a collapsed-empty scan read as "no changes".
    // Checked first so a Failed scan is never masked by a NotRepo companion.
    if matches!(changed, ChangeScan::Failed) || matches!(added, AddedScan::Failed) {
        return Verdict {
            added_impl_lines: 0,
            test_marker_added: false,
            test_file_changed: false,
            impl_files: Vec::new(),
            git_unscoped: false,
            git_scan_failed: true,
        };
    }

    let (ChangeScan::Files(changed), AddedScan::Lines(added)) = (changed, added) else {
        // At least one side is NotRepo (and neither is Failed): no git scope,
        // allow the stop exactly as before.
        return Verdict {
            added_impl_lines: 0,
            test_marker_added: false,
            test_file_changed: false,
            impl_files: Vec::new(),
            git_unscoped: true,
            git_scan_failed: false,
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

    Verdict {
        added_impl_lines,
        test_marker_added,
        test_file_changed,
        impl_files,
        git_unscoped: false,
        git_scan_failed: false,
    }
}

/// Run the gate against a real project root. `git_scan_failed` is set (via
/// `classify`) when either the changed-files or the added-lines scan reports
/// `Failed`, so an errored git command fails the gate closed.
pub fn evaluate(cfg: &Config, root: &Path) -> Verdict {
    let changed = git::changed_files(root);
    let added = git::added_lines(root);
    classify(cfg, &changed, &added)
}

/// The reason injected back into the model when the stop is blocked.
pub fn block_reason(v: &Verdict, attempt: u32, max: u32) -> String {
    // Undetermined changeset (a git command errored): a DISTINCT, loud reason.
    // We are not allowing the stop blindly on an empty/collapsed scan; the
    // existing escape hatches still apply so the turn is never trapped.
    if v.git_scan_failed {
        return format!(
            "🔴 tdd: couldn't determine what changed — a `git` command failed (attempt \
             {attempt}/{max}). Not allowing the stop blindly on an undetermined changeset \
             (that would let untested code through). Fix the git error (see `tdd status`), \
             or create `.tdd-skip` in the project root with a one-line reason to skip once, \
             or set TDD_DISABLE=1 to disable entirely."
        );
    }
    let sample = if v.impl_files.is_empty() {
        String::new()
    } else {
        let shown: Vec<&str> = v.impl_files.iter().take(6).map(String::as_str).collect();
        format!("\n  implementation changed: {}", shown.join(", "))
    };
    format!(
        "🔴 tdd: write a test first — {} new implementation line(s) added with no \
         accompanying test (attempt {attempt}/{max}).{sample}\n\n\
         Add a test that exercises this change (a `#[test]`, `def test_…`, `func Test…`, \
         `it(...)`, or a file under tests/), then finish. Prefer test-first: run \
         `tdd red --task <id>` to capture the failing test before you implement, and \
         `tdd green --task <id>` once it passes.\n\n\
         Genuinely no test needed (pure refactor/rename/docs)? Create `.tdd-skip` in the \
         project root with a one-line reason (consumed once). Disable entirely: TDD_DISABLE=1.",
        v.added_impl_lines
    )
}

/// Compact human report for manual `tdd gate` / `tdd status` runs.
pub fn human_report(v: &Verdict, cfg: &Config) -> String {
    if v.git_scan_failed {
        return "🔴 git scan FAILED (a git command errored) — tdd gate BLOCKS the stop \
                (undetermined changeset, failing closed)"
            .to_string();
    }
    if v.git_unscoped {
        return "(no git repo — tdd gate allows the stop)".to_string();
    }
    let mut s = String::new();
    s.push_str(&format!("added impl lines: {}\n", v.added_impl_lines));
    s.push_str(&format!(
        "test evidence:    {}\n",
        if v.has_test_evidence() {
            if v.test_file_changed && v.test_marker_added {
                "yes (test file + inline test)"
            } else if v.test_file_changed {
                "yes (test file changed)"
            } else {
                "yes (inline test added)"
            }
        } else {
            "none"
        }
    ));
    if v.blocks(cfg) {
        s.push_str("\n🔴 would BLOCK: implementation added without a test");
    } else {
        s.push_str("\n✓ would allow the stop");
    }
    s
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
        assert_eq!(v.added_impl_lines, 1);
        assert!(!v.has_test_evidence());
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
        assert!(v.test_marker_added);
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
        assert!(v.test_file_changed);
        assert!(!v.blocks(&cfg));
    }

    #[test]
    fn docs_only_change_allowed() {
        let cfg = Config::default();
        let changed = files(&["README.md"]);
        let added = lines(vec![added("README.md", "# hello")]);
        let v = classify(&cfg, &changed, &added);
        assert_eq!(v.added_impl_lines, 0);
        assert!(!v.blocks(&cfg));
    }

    #[test]
    fn no_git_never_blocks() {
        let cfg = Config::default();
        let v = classify(&cfg, &ChangeScan::NotRepo, &AddedScan::NotRepo);
        assert!(v.git_unscoped);
        assert!(!v.git_scan_failed);
        assert!(!v.blocks(&cfg));
    }

    #[test]
    fn blank_added_lines_dont_count() {
        let cfg = Config::default();
        let changed = files(&["src/lib.rs"]);
        let added = lines(vec![added("src/lib.rs", "   ")]);
        let v = classify(&cfg, &changed, &added);
        assert_eq!(v.added_impl_lines, 0);
        assert!(!v.blocks(&cfg));
    }

    // ── fail-closed on an undetermined changeset ───────────────────────────
    //
    // The fail-open this fix closes: a `git` command errored inside a real
    // repo, the scan collapsed to empty, and the gate read that as "nothing
    // changed → allow". A `Failed` scan must BLOCK (undetermined ≠ clean),
    // exactly like a failing checker/reviewer subprocess already does.

    /// A `Failed` changed-files scan → `git_scan_failed` → BLOCK, even though
    /// nothing else is set (0 impl lines, no test). This is the regression.
    #[test]
    fn failed_change_scan_blocks_it_does_not_allow() {
        let cfg = Config::default();
        let v = classify(&cfg, &ChangeScan::Failed, &AddedScan::Lines(Vec::new()));
        assert!(
            v.git_scan_failed,
            "a failed git command must mark the scan failed"
        );
        assert!(!v.git_unscoped, "Failed is not the no-scope NotRepo case");
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
        assert!(v.git_scan_failed);
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
            reason.contains(".tdd-skip"),
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
        assert!(!v.git_scan_failed, "an empty SUCCESS is not a failure");
        assert!(
            !v.git_unscoped,
            "a clean repo is in scope, just with no changes"
        );
        assert!(
            !v.blocks(&cfg),
            "a genuinely clean repo must still be allowed"
        );
    }
}
