//! Persist audit results: the Markdown report, the advancing baseline ref, and
//! the sentinel that signals "a human should look at this".

use crate::config::Config;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub struct Paths {
    pub report: PathBuf,
    pub last_ref: PathBuf,
    pub sentinel: PathBuf,
}

/// Compute the output paths (report dir + sentinel are repo-root-relative).
pub fn paths(cfg: &Config, repo_root: &Path, date: &str) -> Paths {
    let report_dir = repo_root.join(&cfg.output.report_dir);
    Paths {
        report: report_dir.join(format!("{date}.md")),
        last_ref: report_dir.join(".last-ref"),
        sentinel: repo_root.join(&cfg.output.sentinel),
    }
}

/// Read the recorded baseline ref, if any.
pub fn read_last_ref(paths: &Paths) -> Option<String> {
    std::fs::read_to_string(&paths.last_ref)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Write the report body. Advancing the baseline is a SEPARATE step
/// ([`advance_baseline`]) so a run that leaves findings pending can persist the
/// report without moving the baseline past the unfixed drift.
pub fn write_report(paths: &Paths, body: &str) -> Result<()> {
    if let Some(dir) = paths.report.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating report dir {}", dir.display()))?;
    }
    std::fs::write(&paths.report, format!("{}\n", body.trim_end()))
        .with_context(|| format!("writing report {}", paths.report.display()))?;
    Ok(())
}

/// Advance the recorded baseline (`.last-ref`) to `head`. Called only when a run
/// reaches a clean state (no findings and no pending sentinel) so unfixed drift
/// stays in scope for the next run until a human `ack`s it.
pub fn advance_baseline(paths: &Paths, head: &str) -> Result<()> {
    std::fs::write(&paths.last_ref, format!("{head}\n"))
        .with_context(|| format!("writing last-ref {}", paths.last_ref.display()))?;
    Ok(())
}

/// Whether a sentinel is currently raised (pending human review).
pub fn sentinel_pending(paths: &Paths) -> bool {
    paths.sentinel.exists()
}

/// Sentinel marker for "current_head() could not be resolved at raise time"
/// (e.g. `scope::current_head()` errored — git unavailable/corrupt worktree —
/// while a finding was being raised). CA-specguard-005: this must NOT be a
/// value that can equal-check itself away once git recovers. Unlike a real
/// hash, [`has_new_commits`] treats this exact marker as *never* satisfiable
/// (no ordinary HEAD, including this literal string again, counts as a "new
/// commit" against it) — an ack without `--force` can never clear a sentinel
/// raised with a poisoned `raised_at`, regardless of the current HEAD.
pub const POISONED_RAISED_AT: &str = "UNRESOLVABLE-HEAD-AT-RAISE";

/// Raise the sentinel (findings need human review). Mirrors the reference
/// runner's format so existing SessionStart hooks can parse it.
/// `raised_at` is the git HEAD at the time of the raise; `ack` uses it to
/// verify a fix commit was made before clearing the sentinel. If `raised_at`
/// is [`POISONED_RAISED_AT`] (HEAD could not be resolved when raising), the
/// sentinel can only ever be cleared with `--force` (see [`has_new_commits`]).
pub fn write_sentinel(
    paths: &Paths,
    date: &str,
    report_rel: &str,
    summary: &str,
    raised_at: &str,
) -> Result<()> {
    if let Some(dir) = paths.sentinel.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let summary = if summary.trim().is_empty() {
        "(要約なし)"
    } else {
        summary.trim()
    };
    let body =
        format!("date: {date}\nreport: {report_rel}\nsummary: {summary}\nraised_at: {raised_at}\n");
    std::fs::write(&paths.sentinel, body)
        .with_context(|| format!("writing sentinel {}", paths.sentinel.display()))?;
    Ok(())
}

/// Extract the `raised_at` commit from a sentinel file's contents.
pub fn sentinel_raised_at(content: &str) -> Option<String> {
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("raised_at:") {
            let v = val.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// True when `current_head` differs from `raised_at`, meaning at least one new
/// commit was made after the sentinel was raised (i.e., a fix was committed).
///
/// CA-specguard-005: if `raised_at` is [`POISONED_RAISED_AT`] (HEAD could not
/// be resolved when the sentinel was raised), there is no real commit to
/// compare against, so this always reads as "no new commits" — even once git
/// recovers and `current_head` resolves to some real, differing hash. A
/// poisoned raise can only be cleared with `ack --force`, never by this
/// automatic check.
pub fn has_new_commits(raised_at: &str, current_head: &str) -> bool {
    let raised_at = raised_at.trim();
    if raised_at.is_empty() || raised_at == POISONED_RAISED_AT {
        return false;
    }
    raised_at != current_head.trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn cfg() -> Config {
        toml::from_str(
            r#"
            [project]
            name = "x"
            [output]
            report_dir = "reports/spec-audit"
            sentinel = ".specguard-pending"
            [[area]]
            name = "a"
            globs = ["a/**"]
            "#,
        )
        .unwrap()
    }

    #[test]
    fn writes_report_then_advances_baseline_separately() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(&cfg(), tmp.path(), "2026-06-17");
        write_report(&p, "# body").unwrap();
        assert_eq!(fs::read_to_string(&p.report).unwrap().trim_end(), "# body");
        // Report written, but baseline not advanced until the separate call.
        assert_eq!(read_last_ref(&p), None);
        advance_baseline(&p, "deadbeef").unwrap();
        assert_eq!(read_last_ref(&p), Some("deadbeef".to_string()));
    }

    #[test]
    fn sentinel_has_expected_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(&cfg(), tmp.path(), "2026-06-17");
        write_sentinel(
            &p,
            "2026-06-17",
            "reports/spec-audit/2026-06-17.md",
            "fix X",
            "abc123",
        )
        .unwrap();
        let s = fs::read_to_string(&p.sentinel).unwrap();
        assert!(s.contains("date: 2026-06-17"));
        assert!(s.contains("report: reports/spec-audit/2026-06-17.md"));
        assert!(s.contains("summary: fix X"));
        assert!(s.contains("raised_at: abc123"));
    }

    #[test]
    fn empty_summary_becomes_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(&cfg(), tmp.path(), "2026-06-17");
        write_sentinel(&p, "2026-06-17", "r.md", "   ", "deadbeef").unwrap();
        assert!(fs::read_to_string(&p.sentinel)
            .unwrap()
            .contains("(要約なし)"));
    }

    #[test]
    fn sentinel_raised_at_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(&cfg(), tmp.path(), "2026-06-26");
        write_sentinel(&p, "2026-06-26", "r.md", "drift", "abc123def").unwrap();
        let s = fs::read_to_string(&p.sentinel).unwrap();
        assert_eq!(sentinel_raised_at(&s), Some("abc123def".to_string()));
    }

    #[test]
    fn has_new_commits_true_when_head_differs() {
        assert!(has_new_commits("abc123", "def456"));
    }

    #[test]
    fn has_new_commits_false_when_same() {
        assert!(!has_new_commits("abc123", "abc123"));
    }

    #[test]
    fn has_new_commits_false_when_raised_at_empty() {
        assert!(!has_new_commits("", "abc123"));
    }

    #[test]
    fn has_new_commits_false_when_raised_at_poisoned() {
        // CA-specguard-005: a sentinel raised while current_head() errored
        // must never be clearable by the automatic "new commit" check, no
        // matter what the (now-healthy) current HEAD resolves to.
        assert!(!has_new_commits(POISONED_RAISED_AT, "abc123def"));
        assert!(!has_new_commits(POISONED_RAISED_AT, "some-other-real-hash"));
        // Even the degenerate case of the marker appearing as "current" HEAD
        // (impossible in practice, but must not accidentally toggle true).
        assert!(!has_new_commits(POISONED_RAISED_AT, POISONED_RAISED_AT));
    }

    #[test]
    fn sentinel_raised_at_missing_returns_none() {
        // Old sentinel without raised_at field
        let old_sentinel = "date: 2026-06-01\nreport: r.md\nsummary: drift\n";
        assert_eq!(sentinel_raised_at(old_sentinel), None);
    }
}
