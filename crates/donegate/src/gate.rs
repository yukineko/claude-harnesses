//! The gate: pick the checks that apply to what changed, run them, and turn the
//! outcomes into a verdict plus a model-facing block reason.

use std::path::Path;

use globset::{Glob, GlobSetBuilder};

use crate::config::{Check, Config};
use crate::runner::{self, Outcome};

pub struct Verdict {
    /// Checks that actually ran (in order).
    pub ran: Vec<Outcome>,
    /// Names of checks skipped because nothing they watch changed.
    pub skipped: Vec<String>,
    /// True when no git repo was found, so `when_changed` scoping was bypassed.
    pub git_unscoped: bool,
    /// True when the scan was UNDETERMINED (git unspawnable, or a sub-command
    /// exited non-zero) rather than out of scope. Kept apart from
    /// `git_unscoped` because reporting an undetermined scan as "no git repo"
    /// states something false about the environment — `status` already made
    /// this distinction; the gate's own report did not.
    pub git_scan_failed: bool,
}

impl Verdict {
    /// Required (non-optional) checks that failed — these block the stop.
    pub fn blocking(&self) -> Vec<&Outcome> {
        self.ran
            .iter()
            .filter(|o| !o.passed && !o.optional)
            .collect()
    }

    /// Optional checks that failed — surfaced as warnings only.
    pub fn warnings(&self) -> Vec<&Outcome> {
        self.ran
            .iter()
            .filter(|o| !o.passed && o.optional)
            .collect()
    }

    pub fn all_green(&self) -> bool {
        self.blocking().is_empty()
    }
}

/// Does this check apply, given the changed-file set? A check with no
/// `when_changed` always applies; with one, it applies iff a changed path
/// matches. When `changed` is `None` (no git) every check applies.
fn applies(check: &Check, changed: &Option<Vec<String>>) -> bool {
    let Some(globs) = &check.when_changed else {
        return true;
    };
    let Some(files) = changed else {
        return true; // can't scope without git → run it
    };
    let mut b = GlobSetBuilder::new();
    let mut any = false;
    for g in globs {
        if let Ok(glob) = Glob::new(g) {
            b.add(glob);
            any = true;
        }
    }
    if !any {
        return true;
    }
    let set = match b.build() {
        Ok(s) => s,
        Err(_) => return true,
    };
    files.iter().any(|f| set.is_match(f))
}

pub fn evaluate(cfg: &Config, root: &Path) -> Verdict {
    // donegate is RESTRICTIVE: both "not a repo" and "could not determine" map to
    // "no usable scope" → every check applies. Only a SUCCESSFUL scan narrows the
    // check set. Crucially `Failed` (a git sub-command errored) must land here, not
    // in a `Some(vec![])` "nothing changed" that would silently skip every
    // `when_changed` check and pass the Stop gate on an undetermined tree.
    let scan = crate::git::changed_files(root);
    // Both non-`Files` scans bypass scoping, but they are NOT the same fact and
    // must not share one flag: `git_unscoped` alone made the report below say
    // "no git repo" about a repo whose git had merely failed.
    let git_unscoped = scan == crate::git::ChangeScan::NotRepo;
    let git_scan_failed = scan == crate::git::ChangeScan::Failed;
    let changed: Option<Vec<String>> = match scan {
        crate::git::ChangeScan::Files(v) => Some(v),
        crate::git::ChangeScan::NotRepo | crate::git::ChangeScan::Failed => None,
    };
    let tmp_dir = cfg.state_dir.join("tmp");

    let mut ran = Vec::new();
    let mut skipped = Vec::new();
    for check in &cfg.checks {
        if applies(check, &changed) {
            ran.push(runner::run_check(
                check,
                root,
                cfg.default_timeout_secs,
                cfg.output_tail_lines,
                &tmp_dir,
            ));
        } else {
            skipped.push(check.name.clone());
        }
    }

    Verdict {
        ran,
        skipped,
        git_unscoped,
        git_scan_failed,
    }
}

/// Render one outcome as an indented block for the model / terminal.
fn render_outcome(o: &Outcome) -> String {
    let mark = if o.passed { "✓" } else { "✗" };
    let detail = if o.timed_out {
        "timed out".to_string()
    } else if let Some(err) = &o.spawn_error {
        err.clone()
    } else {
        match o.exit_code {
            Some(c) => format!("exit {c}"),
            None => "killed".to_string(),
        }
    };
    let mut s = format!(
        "{mark} {} ({detail}, {:.1}s)\n    $ {}",
        o.name, o.duration_secs, o.cmd
    );
    if !o.passed {
        let tail = o.output_tail.trim_end();
        if !tail.is_empty() {
            for line in tail.lines() {
                s.push_str("\n    ");
                s.push_str(line);
            }
        }
    }
    s
}

/// The reason string injected back into the model when the stop is blocked.
pub fn block_reason(v: &Verdict, attempt: u32, max: u32) -> String {
    let failing = v.blocking();
    let mut out = format!(
        "🚦 donegate: not done yet — {} required check(s) failed (attempt {attempt}/{max}). \
         Fix them, then finish.\n",
        failing.len()
    );
    for o in &failing {
        out.push('\n');
        out.push_str(&render_outcome(o));
        out.push('\n');
    }
    let warns = v.warnings();
    if !warns.is_empty() {
        out.push_str("\n(optional, not blocking — but worth a look:)\n");
        for o in &warns {
            out.push_str(&format!("  ⚠ {} ({})\n", o.name, o.status()));
        }
    }
    out.push_str(
        "\nWhen they pass, donegate will let you stop. To finish anyway, create a file \
         `.donegate-skip` in the project root with a one-line reason (consumed once). \
         To disable entirely: set DONEGATE_DISABLE=1.",
    );
    out
}

/// A compact human report for manual `donegate gate` runs.
pub fn human_report(v: &Verdict) -> String {
    let mut out = String::new();
    if v.git_unscoped {
        out.push_str("(no git repo — all checks ran unscoped)\n");
    }
    if v.git_scan_failed {
        out.push_str("(git state undetermined — all checks ran unscoped, fail-closed)\n");
    }
    for o in &v.ran {
        out.push_str(&render_outcome(o));
        out.push('\n');
    }
    if !v.skipped.is_empty() {
        out.push_str(&format!(
            "skipped (no matching changes): {}\n",
            v.skipped.join(", ")
        ));
    }
    let blocking = v.blocking();
    if blocking.is_empty() {
        out.push_str("\n✓ all required checks green");
    } else {
        out.push_str(&format!("\n✗ {} required check(s) failed", blocking.len()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, when: Option<Vec<&str>>) -> Check {
        Check {
            name: name.to_string(),
            cmd: "true".to_string(),
            when_changed: when.map(|v| v.into_iter().map(String::from).collect()),
            timeout_secs: None,
            optional: false,
            workdir: None,
        }
    }

    #[test]
    fn unconditional_check_always_applies() {
        let c = check("build", None);
        assert!(applies(&c, &Some(vec![])));
        assert!(applies(&c, &None));
    }

    #[test]
    fn scoped_check_matches_glob() {
        let c = check("test", Some(vec!["**/*.rs"]));
        assert!(applies(&c, &Some(vec!["src/main.rs".to_string()])));
        assert!(!applies(&c, &Some(vec!["README.md".to_string()])));
    }

    #[test]
    fn scoped_check_runs_when_git_absent() {
        let c = check("test", Some(vec!["**/*.rs"]));
        assert!(applies(&c, &None));
    }

    // ---------------------------------------------------------------------
    // NotRepo vs Failed must not share one flag, and the human report must not
    // state something false about the environment.
    //
    // The bug being pinned: `evaluate` set `git_unscoped = changed.is_none()`,
    // so BOTH `ChangeScan::NotRepo` and `ChangeScan::Failed` raised it, and
    // `human_report` then printed "(no git repo — all checks ran unscoped)"
    // for a repo whose git had merely FAILED. `main.rs`'s `status` printer
    // already distinguished the two; the gate's own report did not.
    // ---------------------------------------------------------------------

    use std::path::PathBuf;
    use std::process::Command;

    fn scratch_dir(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "donegate-gate-{}-{}-{}",
            tag,
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create scratch dir");
        p
    }

    /// Independent of the code under test; used only to skip cases that cannot
    /// be constructed on a machine whose temp dir sits inside a repository.
    fn has_dot_git_ancestor(dir: &Path) -> bool {
        let mut cur = Some(dir);
        while let Some(d) = cur {
            if std::fs::symlink_metadata(d.join(".git")).is_ok() {
                return true;
            }
            cur = d.parent();
        }
        false
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn cfg_for(state_dir: &Path, checks: Vec<Check>) -> Config {
        Config {
            enabled: true,
            max_attempts: 3,
            default_timeout_secs: 30,
            output_tail_lines: 10,
            reset_after_secs: 600,
            state_dir: state_dir.to_path_buf(),
            checks,
        }
    }

    fn verdict_flags(git_unscoped: bool, git_scan_failed: bool) -> Verdict {
        Verdict {
            ran: Vec::new(),
            skipped: Vec::new(),
            git_unscoped,
            git_scan_failed,
        }
    }

    /// End-to-end: a directory whose `.git` exists but is unusable makes the
    /// shared probe answer `Undetermined` → `ChangeScan::Failed`. `evaluate`
    /// must record that as `git_scan_failed`, and must NOT claim `git_unscoped`
    /// (which the report renders as "no git repo").
    #[test]
    fn evaluate_failed_scan_sets_scan_failed_not_unscoped() {
        if !git_available() {
            eprintln!(
                "skipping evaluate_failed_scan_sets_scan_failed_not_unscoped: git not available"
            );
            return;
        }
        let root = scratch_dir("failed");
        let work = root.join("work");
        // A `.git` DIRECTORY that is not a valid repository: git refuses to
        // answer, while the filesystem still shows evidence of a repo.
        std::fs::create_dir_all(work.join(".git")).expect("create .git");

        // Apparatus: the scan really is Failed here, so a pass below cannot be
        // for the wrong reason.
        let scan = crate::git::changed_files(&work);
        assert_eq!(
            scan,
            crate::git::ChangeScan::Failed,
            "apparatus: an unusable .git must make the scan UNDETERMINED on this machine; got \
             {scan:?}"
        );

        let cfg = cfg_for(&root, vec![check("scoped", Some(vec!["**/*.rs"]))]);
        let v = evaluate(&cfg, &work);

        assert!(
            v.git_scan_failed,
            "a Failed scan must set git_scan_failed so the human is told the git state was \
             undetermined"
        );
        assert!(
            !v.git_unscoped,
            "a Failed scan must NOT set git_unscoped — that flag is rendered as '(no git repo)', \
             a false statement about a directory that does have a .git"
        );
        // Restrictive branch is unchanged: scoping is still bypassed.
        assert_eq!(v.skipped, Vec::<String>::new());
        assert_eq!(
            v.ran.len(),
            1,
            "an undetermined scan must keep checks applicable"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// End-to-end: a genuine non-repo sets `git_unscoped` and NOT
    /// `git_scan_failed` (the fix must not start crying "undetermined" for a
    /// directory that is simply out of scope).
    #[test]
    fn evaluate_notrepo_scan_sets_unscoped_not_scan_failed() {
        let root = scratch_dir("notrepo");
        if has_dot_git_ancestor(&root) {
            eprintln!(
                "SKIPPED evaluate_notrepo_scan_sets_unscoped_not_scan_failed: {} has a .git \
                 ancestor on this machine, so a genuine non-repo cannot be constructed here",
                root.display()
            );
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let work = root.join("work");
        std::fs::create_dir_all(&work).expect("create work dir");

        let scan = crate::git::changed_files(&work);
        assert_eq!(
            scan,
            crate::git::ChangeScan::NotRepo,
            "apparatus: this directory must be a confirmed non-repo; got {scan:?}"
        );

        let cfg = cfg_for(&root, vec![check("scoped", Some(vec!["**/*.rs"]))]);
        let v = evaluate(&cfg, &work);

        assert!(v.git_unscoped, "a NotRepo scan must set git_unscoped");
        assert!(
            !v.git_scan_failed,
            "a NotRepo scan is a determined answer — it must NOT be reported as an undetermined \
             git state"
        );
        assert_eq!(v.ran.len(), 1, "no scope ⇒ every check applies");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// End-to-end: a successful scan sets NEITHER flag (no over-report), and
    /// scoping actually narrows the check set.
    #[test]
    fn evaluate_successful_scan_sets_neither_flag() {
        if !git_available() {
            eprintln!("skipping evaluate_successful_scan_sets_neither_flag: git not available");
            return;
        }
        let root = scratch_dir("files");
        let work = root.join("work");
        std::fs::create_dir_all(&work).expect("create work dir");
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t.com"][..],
            &["config", "user.name", "t"][..],
        ] {
            assert!(Command::new("git")
                .current_dir(&work)
                .args(args)
                .status()
                .expect("git")
                .success());
        }
        std::fs::write(work.join("a.md"), "x\n").expect("write a.md");

        let scan = crate::git::changed_files(&work);
        assert_eq!(
            scan,
            crate::git::ChangeScan::Files(vec!["a.md".to_string()]),
            "apparatus: the scan must succeed here; got {scan:?}"
        );

        let cfg = cfg_for(&root, vec![check("scoped", Some(vec!["**/*.rs"]))]);
        let v = evaluate(&cfg, &work);

        assert!(!v.git_unscoped, "a successful scan is not 'no git repo'");
        assert!(
            !v.git_scan_failed,
            "a successful scan is not an undetermined git state"
        );
        assert_eq!(
            v.skipped,
            vec!["scoped".to_string()],
            "a successful scan must still scope checks out"
        );
        assert!(v.ran.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The load-bearing negative: the report for a FAILED scan must never tell
    /// the human there is no git repo. It must instead say the state was
    /// undetermined (asserted as a robust disjunction, not exact prose).
    #[test]
    fn human_report_failed_scan_never_claims_no_git_repo() {
        let report = human_report(&verdict_flags(false, true));
        let lower = report.to_lowercase();
        assert!(
            !lower.contains("no git repo"),
            "human_report must NOT claim 'no git repo' for a scan that merely FAILED — that is a \
             false statement about the environment in the one place a human reads when \
             debugging. Got:\n{report}"
        );
        assert!(
            lower.contains("undetermined")
                || lower.contains("could not")
                || lower.contains("fail-closed"),
            "human_report must tell the human the git state was undetermined. Got:\n{report}"
        );
    }

    /// The fix must not erase the correct existing message for a real non-repo.
    #[test]
    fn human_report_notrepo_scan_still_says_no_git_repo() {
        let report = human_report(&verdict_flags(true, false));
        let lower = report.to_lowercase();
        assert!(
            lower.contains("no git repo"),
            "a confirmed non-repo must still be reported as such. Got:\n{report}"
        );
        assert!(
            !lower.contains("undetermined"),
            "a confirmed non-repo is a determined answer; it must not be reported as \
             undetermined. Got:\n{report}"
        );
    }

    /// A successful scan reports neither environment caveat.
    #[test]
    fn human_report_successful_scan_reports_no_git_caveat() {
        let report = human_report(&verdict_flags(false, false));
        let lower = report.to_lowercase();
        assert!(!lower.contains("no git repo"), "got:\n{report}");
        assert!(!lower.contains("undetermined"), "got:\n{report}");
    }
}
