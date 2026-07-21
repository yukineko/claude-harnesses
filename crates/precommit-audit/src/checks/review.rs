//! Subagent review contract (Check 4). Claude Code specific: a `/precommit`
//! command writes `<review_path>` with the diff hash and a `pass` verdict; this
//! check blocks until that artifact matches the current tree. Disabled by
//! default; opt-in per project.

use super::Ctx;
use crate::git;
use crate::model::Issue;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Canonical diff hash. Must match what `/precommit` computes in Python:
///   sha256( diff_HEAD_lines_joined_by_LF + "\n"
///           + "---UNTRACKED---\n"
///           + sorted(untracked minus artifact/marker) joined by LF )
pub fn diff_hash(root: &Path, audit_dir: &str, review_rel: &str) -> Result<String, String> {
    let diff = git::diff_head(root)?;
    // Normalize to LF and ensure a single trailing newline, matching the
    // PowerShell `($lines -join "`n") + "`n"` construction.
    let mut d: String = diff
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .collect::<Vec<_>>()
        .join("\n");
    d.push('\n');

    let artifact = norm_rel(review_rel);
    let marker = format!("{}/.audit-blocked", audit_dir.trim_end_matches('/'));
    let mut untracked: Vec<String> = git::untracked(root)?
        .into_iter()
        .map(|u| u.replace('\\', "/"))
        .filter(|u| u != &artifact && u != &marker)
        .collect();
    untracked.sort();

    let content = format!("{d}---UNTRACKED---\n{}", untracked.join("\n"));
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

fn norm_rel(p: &str) -> String {
    p.replace('\\', "/")
}

/// Returns Some(issue) when the review artifact is missing or stale.
pub fn check(ctx: &Ctx) -> Option<Issue> {
    let rc = &ctx.cfg.review_contract;
    if !rc.enabled {
        return None;
    }
    // Only require review when non-docs source files changed.
    let needs_review = ctx.sources().into_iter().any(|s| {
        let s = crate::classify::norm(s);
        !s.starts_with("docs/") && !s.contains("/docs/")
    });
    if !needs_review {
        return None;
    }

    let want = match diff_hash(ctx.root, &ctx.cfg.audit_dir, &rc.path) {
        Ok(w) => w,
        Err(e) => {
            return Some(Issue::block(
                "SUBAGENT REVIEW REQUIRED",
                format!(
                    "SUBAGENT REVIEW REQUIRED: could not compute the diff hash ({e}); run /precommit to walk the checklist."
                ),
            ));
        }
    };

    let review_path = ctx.root.join(&rc.path);
    let ok = std::fs::read_to_string(&review_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .map(|v| {
            v.get("diff_hash").and_then(|h| h.as_str()) == Some(want.as_str())
                && v.get("verdict").and_then(|h| h.as_str()) == Some("pass")
        })
        .unwrap_or(false);

    if ok {
        None
    } else {
        Some(Issue::block(
            "SUBAGENT REVIEW REQUIRED",
            format!(
                "SUBAGENT REVIEW REQUIRED: {} missing or stale. Run /precommit to walk the full checklist; it must write {} with \"verdict\": \"pass\" and the current diff hash.",
                rc.path, rc.path
            ),
        ))
    }
}
