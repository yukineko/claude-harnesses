/// The finding→backlog bridge: `overwatch review-queue --to-backlog`.
///
/// Continuous-Audit re-records every CONFIRMED review finding into the
/// overwatch findings store each round; this bridge closes the "discover→fix"
/// loop by forwarding each *not-yet-bridged* finding to the backlog (a
/// `backlog add`), so `/flow` can auto-repair it. The findings store is the
/// confirmed-findings ingestion point (`record-finding`), so every finding read
/// here is treated as confirmed.
///
/// Idempotency is enforced on `finding_id` via the `bridged_findings.jsonl`
/// ledger, NOT by the backlog's own duplicate guard (which hashes on
/// title+project). A finding recurring across audit rounds collapses to one
/// row via [`review_queue::dedup_findings`] and, once bridged, is never
/// forwarded again.
///
/// **Fail-soft (never-break-a-turn):** a missing/empty/corrupt findings store,
/// an absent `backlog` binary, or a non-zero `backlog add` are each warned and
/// skipped — the command as a whole always succeeds (exit 0). The existing
/// `review-queue` (no flag) behaviour and the systemic/rollback streams are
/// untouched.
use crate::review_queue;
use crate::store;
use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

/// Resolve the `backlog` binary: an explicit `OVERWATCH_BACKLOG_BIN` override
/// (used to wire a specific binary, and by tests) wins; otherwise the first
/// `backlog` on `PATH`. Returns `None` when neither resolves (fail-soft: the
/// caller then skips the bridge without erroring).
fn resolve_backlog_bin() -> Option<String> {
    if let Ok(p) = std::env::var("OVERWATCH_BACKLOG_BIN") {
        if !p.is_empty() {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join("backlog");
        if cand.is_file() {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    None
}

/// Map a reviewer severity to a backlog priority tier: `high` (any case) → p1,
/// everything else (incl. unknown/absent) → p2.
fn severity_to_priority(severity: Option<&str>) -> &'static str {
    match severity.map(|s| s.to_ascii_lowercase()) {
        Some(s) if s == "high" => "p1",
        _ => "p2",
    }
}

/// Read the confirmed findings, forward each not-yet-bridged one to the backlog,
/// and record the successful bridges. Always returns `Ok(())` (fail-soft).
pub fn to_backlog() -> Result<()> {
    let cwd = std::env::current_dir()?;
    run_in(&cwd)
}

/// The bridge over an explicit project dir (factored out so the logic is
/// testable without depending on the process cwd).
fn run_in(cwd: &Path) -> Result<()> {
    // 1. Confirmed findings, collapsed to newest-per-id.
    let findings = store::read_review_findings(cwd).unwrap_or_default();
    let deduped = review_queue::dedup_findings(&findings);

    // 2. Already-bridged set (idempotency key = finding_id).
    let already: HashSet<String> = store::read_bridged_findings(cwd)
        .unwrap_or_default()
        .into_iter()
        .collect();

    // 3. Backlog binary (fail-soft when absent).
    let backlog = match resolve_backlog_bin() {
        Some(b) => b,
        None => {
            eprintln!(
                "overwatch: WARNING backlog binary not found (OVERWATCH_BACKLOG_BIN unset, none on PATH) — skipping bridge, continuing"
            );
            println!(
                "{}",
                serde_json::json!({
                    "bridged": 0,
                    "considered": deduped.len(),
                    "skipped": "backlog-unavailable"
                })
            );
            return Ok(());
        }
    };

    let project = cwd.to_string_lossy().into_owned();
    let mut bridged_now = 0usize;

    for f in &deduped {
        if already.contains(&f.finding_id) {
            continue; // idempotent: already forwarded in a prior round.
        }
        let priority = severity_to_priority(f.severity.as_deref());
        let notes = format!(
            "finding-id:{} file:{} severity:{}",
            f.finding_id,
            f.file.as_deref().unwrap_or("(none)"),
            f.severity.as_deref().unwrap_or("(none)"),
        );

        let status = std::process::Command::new(&backlog)
            .arg("add")
            .arg("--title")
            .arg(&f.summary)
            .arg("--project")
            .arg(&project)
            .arg("--priority")
            .arg(priority)
            .arg("--notes")
            .arg(&notes)
            .status();

        match status {
            Ok(s) if s.success() => match store::append_bridged_finding(cwd, &f.finding_id) {
                Ok(()) => bridged_now += 1,
                Err(e) => {
                    // The backlog task WAS added but we could not persist the
                    // idempotency key. Warn; a future round may re-add it (the
                    // backlog's title+project guard is the backstop there).
                    eprintln!(
                        "overwatch: WARNING bridged finding {} but could not record it (continuing): {e}",
                        f.finding_id
                    );
                }
            },
            Ok(s) => {
                eprintln!(
                    "overwatch: WARNING `backlog add` failed for finding {} (exit {:?}) — continuing",
                    f.finding_id,
                    s.code()
                );
            }
            Err(e) => {
                eprintln!(
                    "overwatch: WARNING could not spawn backlog for finding {} (continuing): {e}",
                    f.finding_id
                );
            }
        }
    }

    println!(
        "{}",
        serde_json::json!({ "bridged": bridged_now, "considered": deduped.len() })
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_maps_high_to_p1_else_p2() {
        assert_eq!(severity_to_priority(Some("high")), "p1");
        assert_eq!(severity_to_priority(Some("HIGH")), "p1");
        assert_eq!(severity_to_priority(Some("medium")), "p2");
        assert_eq!(severity_to_priority(Some("low")), "p2");
        assert_eq!(severity_to_priority(None), "p2");
    }

    #[test]
    fn backlog_bin_override_env_wins() {
        std::env::set_var("OVERWATCH_BACKLOG_BIN", "/some/fake/backlog");
        assert_eq!(resolve_backlog_bin().as_deref(), Some("/some/fake/backlog"));
        std::env::remove_var("OVERWATCH_BACKLOG_BIN");
    }
}
