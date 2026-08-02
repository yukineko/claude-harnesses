//! Persist audit results: the Markdown report, the advancing baseline ref, and
//! the sentinel that signals "a human should look at this".

use crate::config::Config;
use anyhow::{Context, Result};
use harness_core::verdict::Determination;
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

/// Whether a sentinel is currently raised (pending human review) — as three
/// answers, not two.
///
/// `Path::exists()` cannot express the third one: it maps *every* failure of the
/// underlying `stat` — an unsearchable parent directory, EIO, a dangling
/// symlink, EACCES — to `false`, i.e. to "no human review is pending". That is
/// the permissive answer, and it is the one that lets [`advance_baseline`] retire
/// drift no human ever reviewed (observed in
/// `crates/specguard/tests/faultinject_sentinel.rs`).
///
/// So this returns a [`Determination<bool>`]:
///
/// * `Known(true)`  — the sentinel file is there; a review is pending.
/// * `Known(false)` — `NotFound` specifically: we looked, and there is none.
///   This is the only observation that authorises advancing the baseline.
/// * `Undetermined` — the sentinel's presence could not be observed at all.
///   A caller must hold (never advance, never report "nothing pending").
pub fn sentinel_pending(paths: &Paths) -> Determination<bool> {
    match std::fs::symlink_metadata(&paths.sentinel) {
        Ok(_) => Determination::Known(true),
        // The one error that is a real observation: we reached the parent
        // directory and the entry is genuinely absent.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Determination::Known(false),
        Err(e) => Determination::undetermined(format!(
            "cannot tell whether the sentinel {} exists: {e}",
            paths.sentinel.display()
        )),
    }
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
///
/// `covers` is the set of `overwatch` review-finding ids this raise surfaced —
/// the mapping `ack` needs to record a disposition per finding when it clears
/// the sentinel. Without it a clearance is unattributable: the sentinel is a
/// single boolean flag, so "a human handled this" could not be joined back to
/// *which* findings were handled, and the closure rate for spec findings was
/// not computable at all. Each id is written on its own `covers:` line rather
/// than as one delimited field because a shard label is user-supplied config
/// text and may contain any separator we would have picked.
pub fn write_sentinel(
    paths: &Paths,
    date: &str,
    report_rel: &str,
    summary: &str,
    raised_at: &str,
    covers: &[String],
) -> Result<()> {
    if let Some(dir) = paths.sentinel.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let summary = if summary.trim().is_empty() {
        "(要約なし)"
    } else {
        summary.trim()
    };
    let mut body =
        format!("date: {date}\nreport: {report_rel}\nsummary: {summary}\nraised_at: {raised_at}\n");
    for id in covers {
        let id = id.trim();
        if !id.is_empty() {
            body.push_str(&format!("covers: {id}\n"));
        }
    }
    std::fs::write(&paths.sentinel, body)
        .with_context(|| format!("writing sentinel {}", paths.sentinel.display()))?;
    Ok(())
}

/// The review-finding ids a sentinel says it covers (one per `covers:` line).
///
/// An EMPTY result is genuinely ambiguous and callers must not read it as
/// "nothing to dispose": it is equally "this sentinel predates the `covers:`
/// field" (every sentinel raised before this feature) and "the raise recorded
/// no findings". [`ack`](crate::main) therefore reports the empty case out loud
/// instead of clearing silently — a clearance that records nothing while
/// *looking* like a normal one is exactly how the closure rate went missing.
pub fn sentinel_covered_ids(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| line.strip_prefix("covers:"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect()
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
            &[],
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
        write_sentinel(&p, "2026-06-17", "r.md", "   ", "deadbeef", &[]).unwrap();
        assert!(fs::read_to_string(&p.sentinel)
            .unwrap()
            .contains("(要約なし)"));
    }

    #[test]
    fn sentinel_raised_at_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(&cfg(), tmp.path(), "2026-06-26");
        write_sentinel(&p, "2026-06-26", "r.md", "drift", "abc123def", &[]).unwrap();
        let s = fs::read_to_string(&p.sentinel).unwrap();
        assert_eq!(sentinel_raised_at(&s), Some("abc123def".to_string()));
    }

    #[test]
    fn covered_ids_round_trip_through_the_sentinel_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(&cfg(), tmp.path(), "2026-08-01");
        let ids = vec![
            "specguard:spec-drift:logging".to_string(),
            "specguard:audit-indeterminate:invariants".to_string(),
        ];
        write_sentinel(&p, "2026-08-01", "r.md", "drift", "abc123", &ids).unwrap();
        let s = fs::read_to_string(&p.sentinel).unwrap();
        assert_eq!(sentinel_covered_ids(&s), ids);
        // The pre-existing fields must survive the addition unchanged — `ack`
        // still resolves its fix-commit guard from the same sentinel.
        assert_eq!(sentinel_raised_at(&s), Some("abc123".to_string()));
    }

    /// A label carrying the separator we would otherwise have delimited on.
    /// One id per line is what makes this parse rather than split into two.
    #[test]
    fn a_label_containing_a_comma_survives_as_one_id() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(&cfg(), tmp.path(), "2026-08-01");
        let ids = vec!["specguard:spec-drift:auth, session".to_string()];
        write_sentinel(&p, "2026-08-01", "r.md", "drift", "abc123", &ids).unwrap();
        let s = fs::read_to_string(&p.sentinel).unwrap();
        assert_eq!(sentinel_covered_ids(&s), ids);
    }

    /// ANTI-VACUITY CONTROL: the reader must return ids only when the sentinel
    /// actually has them. An old-format sentinel yields an empty vec, which is
    /// the ambiguous case `ack` has to report out loud rather than treat as
    /// "nothing to dispose".
    #[test]
    fn an_old_format_sentinel_reports_no_covered_ids() {
        let old = "date: 2026-06-01\nreport: r.md\nsummary: drift\nraised_at: abc\n";
        assert!(sentinel_covered_ids(old).is_empty());
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
