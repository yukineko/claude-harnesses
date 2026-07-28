use std::path::{Path, PathBuf};

use harness_core::config::home;
use harness_core::projkey::repo_root;
use harness_core::verdict::Determination;
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
///
/// Three answers, not two. `Known(vec![])` means "asked the queue, it is empty"
/// — the only answer that entitles the caller to conclude there is no work left
/// (it latches `Phase::Done` for the rest of the session). Every way of *failing
/// to ask* — the `backlog` invocation not spawning, exiting non-zero, printing
/// output we cannot parse, or answering in a status vocabulary we do not
/// recognise — is `Undetermined`, because an empty vec returned for those would
/// be indistinguishable from an observed-empty queue at the call site.
///
/// The one deliberate exception is `backlog` not being installed, which stays
/// `Known(vec![])`: with no binary there is no queue to have work in. That is an
/// observation, not a failure to observe (the same carve-out
/// `lock::backlog_driver_active` documents).
pub fn find_open(cwd: &Path) -> Determination<Vec<BacklogItem>> {
    let binary = match find_backlog_binary() {
        Determination::Known(Some(b)) => b,
        // No backlog installed ⇒ no queue ⇒ genuinely nothing pending.
        Determination::Known(None) => return Determination::Known(vec![]),
        // We could not even tell whether backlog exists, so we certainly cannot
        // tell whether its queue is empty.
        Determination::Undetermined(why) => return Determination::Undetermined(why),
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
            // Non-zero exit: the queue was never reported. The diagnostic goes to
            // stderr AND the verdict says "undetermined" — reporting an empty vec
            // here is what let autoflow conclude "no work" on a tooling error.
            let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
            eprintln!("autoflow: backlog list exited {}: {}", o.status, stderr);
            return Determination::undetermined(format!(
                "`backlog list` exited {}: {stderr}",
                o.status
            ));
        }
        Err(e) => {
            eprintln!("autoflow: could not run backlog list: {e}");
            return Determination::undetermined(format!("could not run `backlog list`: {e}"));
        }
    };

    let items: Vec<BacklogItem> = match serde_json::from_slice(&output.stdout) {
        Ok(items) => items,
        // Unparseable stdout is not an observation of an idle queue (the same
        // rule `lock::driver_active_from_status` applies to its own stdout).
        Err(e) => {
            return Determination::undetermined(format!(
                "could not parse `backlog list --json` output: {e}"
            ))
        }
    };

    // Server already filtered to status=pending; re-assert client-side as a
    // belt-and-braces guard (a failed task is deferred ~2 days, so surfacing it
    // here would re-drive it immediately and churn).
    let listed = items.len();
    let pending: Vec<BacklogItem> = items
        .into_iter()
        .filter(|i| i.status == "pending")
        .collect();

    // The server was ASKED for `--status pending`, so it answering with items
    // that none of them match means the two sides disagree about the status
    // vocabulary (audit §4.2). The client filter then empties a non-empty
    // answer, which is a failure to interpret the reply — not an observation
    // that the queue is empty.
    if pending.is_empty() && listed > 0 {
        return Determination::undetermined(format!(
            "`backlog list --status pending` returned {listed} item(s), none with status \"pending\" \
             — the status vocabulary does not match, so the queue could not be read"
        ));
    }
    Determination::Known(pending)
}

/// Locate the `backlog` binary: PATH first, then the plugin cache.
///
/// `Known(None)` is the *observation* that backlog is not installed (no plugin
/// cache directory at all). An enumerable-but-failing cache directory — the
/// read denied, an entry unreadable, a candidate whose existence cannot be
/// tested — is `Undetermined`: collapsing it into the same `None` is what let a
/// merely unreadable directory read as "backlog is not installed" and start an
/// unattended auto-loop next to a live driver (audit §4.5, the one permissive-A
/// path).
pub(crate) fn find_backlog_binary() -> Determination<Option<PathBuf>> {
    if std::process::Command::new("backlog")
        .arg("--version")
        .output()
        .is_ok()
    {
        return Determination::Known(Some(PathBuf::from("backlog")));
    }

    // ~/.claude/plugins/cache/yukineko/backlog/<version>/bin/backlog
    let base = home()
        .join(".claude")
        .join("plugins")
        .join("cache")
        .join("yukineko")
        .join("backlog");

    let dir = match std::fs::read_dir(&base) {
        Ok(d) => d,
        // No cache dir ⇒ backlog was never installed here. An observation.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Determination::Known(None),
        // Anything else (permission denied, IO error) ⇒ we did not get to look.
        Err(e) => {
            return Determination::undetermined(format!(
                "could not enumerate {}: {e}",
                base.display()
            ))
        }
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not read an entry of {}: {e}",
                    base.display()
                ))
            }
        };
        let candidate = entry.path().join("bin").join("backlog");
        // `exists()` folds "not there" and "cannot tell" into one `false`;
        // `try_exists()` keeps them apart.
        match candidate.try_exists() {
            Ok(true) => candidates.push(candidate),
            Ok(false) => {}
            Err(e) => {
                return Determination::undetermined(format!(
                    "could not test {}: {e}",
                    candidate.display()
                ))
            }
        }
    }

    candidates.sort();
    Determination::Known(candidates.pop())
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
