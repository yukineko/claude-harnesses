use std::path::{Path, PathBuf};

use harness_core::config::home;
use harness_core::projkey::repo_root;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct BacklogItem {
    pub id: String,
    /// The task's human title. Backlog serializes this as `title`; we keep the
    /// field name `text` for the consumer but map it from the real JSON key.
    /// (`#[serde(default)]` so a future omission degrades to "" rather than a
    /// parse failure that would drop the whole item.)
    #[serde(rename = "title", default)]
    pub text: String,
    #[serde(default)]
    pub status: String,
}

/// Find outstanding (pending) backlog items for the repo containing `cwd`.
/// Returns an empty vec if the `backlog` binary is not found or there is no
/// pending work. Fail-soft throughout — autoflow must never break a turn.
pub fn find_open(cwd: &Path) -> Vec<BacklogItem> {
    let binary = match find_backlog_binary() {
        Some(b) => b,
        None => return vec![],
    };

    let project = repo_project_path(cwd);

    // The `backlog` binary's subcommand is `list` (NOT `backlog list` — that was
    // the historical bug: autoflow shelled to `session-insights backlog …`, a
    // subcommand session-insights never had, so this always failed and autoflow
    // saw an empty queue). `--status pending` filters server-side to ready work;
    // `--json` yields a machine-readable array.
    let output = std::process::Command::new(&binary)
        .args([
            "list",
            "--project",
            &project,
            "--status",
            "pending",
            "--json",
        ])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            // Non-zero exit: surface it rather than silently reporting "no work"
            // (which would let autoflow conclude the queue is empty on a tooling
            // error). Still fail-soft to an empty vec — never break the turn.
            eprintln!(
                "autoflow: backlog list exited {}: {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return vec![];
        }
        Err(e) => {
            eprintln!("autoflow: could not run backlog list: {e}");
            return vec![];
        }
    };

    let items: Vec<BacklogItem> = serde_json::from_slice(&output.stdout).unwrap_or_default();

    // Server already filtered to status=pending; re-assert client-side as a
    // belt-and-braces guard (a failed task is deferred ~2 days, so surfacing it
    // here would re-drive it immediately and churn).
    items
        .into_iter()
        .filter(|i| i.status == "pending")
        .collect()
}

/// Locate the `backlog` binary: PATH first, then the plugin cache.
pub(crate) fn find_backlog_binary() -> Option<PathBuf> {
    if std::process::Command::new("backlog")
        .arg("--version")
        .output()
        .is_ok()
    {
        return Some(PathBuf::from("backlog"));
    }

    // ~/.claude/plugins/cache/yukineko/backlog/<version>/bin/backlog
    let base = home()
        .join(".claude")
        .join("plugins")
        .join("cache")
        .join("yukineko")
        .join("backlog");

    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&base)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("bin").join("backlog"))
        .filter(|p| p.exists())
        .collect();

    candidates.sort();
    candidates.pop()
}

/// The repo root as a stable, *unique* project filter for `backlog list`.
///
/// The previous `repo_basename` returned only the directory name, with a
/// constant `"unknown"` fallback for a rootless path. Both are predictable
/// collisions: every repo sharing a basename (e.g. two checkouts named `app`),
/// and every non-git directory (all → `"unknown"`), addressed one another's
/// backlog state. We instead use the canonical absolute path, which is unique
/// per repo and matches how tasks are stored (`backlog add --project "$PWD"`,
/// a full path) under `project_matches`'s exact/prefix rule. Canonicalize
/// failure falls back to the raw absolute path — still unique, never a constant.
pub(crate) fn repo_project_path(cwd: &Path) -> String {
    let root = repo_root(cwd);
    root.canonicalize()
        .unwrap_or(root)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two non-git directories that share a basename used to both collapse to the
    // same `--project` value (the basename, or the constant "unknown"), so one
    // repo's autoflow saw the other's backlog. The path-based key keeps them
    // distinct. (These paths don't exist, so canonicalize falls back to the raw
    // path — exactly the rootless/non-git case the old fallback mishandled.)
    #[test]
    fn same_basename_distinct_paths_do_not_collide() {
        let a = repo_project_path(Path::new("/tmp/aaa/app"));
        let b = repo_project_path(Path::new("/var/bbb/app"));
        assert_ne!(a, b, "same-basename repos must get distinct project keys");
        assert!(!a.is_empty() && !b.is_empty());
        // Never the old constant fallback.
        assert_ne!(a, "unknown");
        assert_ne!(b, "unknown");
    }

    // The key matches how tasks are stored: `backlog add --project "$PWD"` uses a
    // full path, and backlog's `project_matches` accepts an exact or prefix hit.
    #[test]
    fn key_is_the_full_path_for_a_non_git_dir() {
        // A path with no .git ancestor → repo_root returns the path itself.
        let p = Path::new("/tmp/some/non-git-dir");
        assert_eq!(repo_project_path(p), "/tmp/some/non-git-dir");
    }

    // The producer/consumer contract: `backlog list --json` emits an array of
    // tasks whose human title is keyed `title` (NOT `text`) plus extra fields we
    // don't model. BacklogItem must map `title` → `text` and ignore the rest, or
    // autoflow surfaces empty/blank work. This is the exact shape printed by the
    // backlog binary (see the crate's integration test).
    #[test]
    fn backlog_item_parses_real_list_json_shape() {
        let json = r#"[{"id":"834167c8","title":"Smoke task","project":"/smoke/proj","tags":["p1"],"status":"pending","notes":"","created_at":1782711396,"updated_at":1782711396,"defer_until":null,"weight":0.0}]"#;
        let items: Vec<BacklogItem> = serde_json::from_str(json).expect("must parse backlog json");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "834167c8");
        assert_eq!(items[0].text, "Smoke task", "title must map to text");
        assert_eq!(items[0].status, "pending");
    }
}
