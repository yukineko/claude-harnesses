//! `harvest`: turn merges into a repo's `main` into episodes automatically.
//!
//! Episodes used to come only from `condukt state record-run`, so work that
//! never went through a condukt run left no learning signal at all (measured
//! 2026-10-01: 139 merges into main after the last recorded episode on
//! 2026-09-24). `harvest` runs from the `SessionEnd` hook and reads the git
//! history instead, which every session produces whether or not it used
//! condukt.
//!
//! The outcome label is decided only once an observation window has closed:
//! a merge is `pass = false` when a `fix`/`revert` commit touching the same
//! (non-noise) files lands on the branch within `window_days` after it. A merge
//! younger than the window is not recorded yet — it is picked up by a later
//! run. That keeps the store append-only: an episode is written once with its
//! final label and never rewritten, which matters because `sync` merges two
//! machines' stores with `merge=union` (a rewritten line would survive next to
//! its old self).
//!
//! Harvested merge SHAs are remembered per repository (keyed by the git common
//! dir, so every worktree of one repo shares a cursor) in
//! `~/.fugu-router/harvest-cursor.json`. An unreadable cursor is an error, not
//! an empty cursor: reading it as empty would re-harvest every merge in the
//! lookback and duplicate them all. Failures are written to
//! `~/.fugu-router/harvest-error.json`, which the `UserPromptSubmit` hook
//! surfaces — the `SessionEnd` hook's own exit code and stderr reach nobody.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{config, store};

const GIT_TIMEOUT: Duration = Duration::from_secs(10);
const DAY: u64 = 86_400;

/// Run `harvest`, leaving a durable failure marker when it cannot finish and
/// clearing it when it does.
pub fn cmd_harvest(
    cfg: &config::Config,
    repo: Option<PathBuf>,
    window_days: u64,
    lookback_days: u64,
) -> Result<()> {
    match harvest(cfg, repo, window_days, lookback_days) {
        Ok(n) => {
            clear_marker();
            if n > 0 {
                eprintln!("fugu-router: harvested {n} merge episode(s)");
            }
            Ok(())
        }
        Err(e) => {
            record_marker(&format!("{e:#}"));
            Err(e)
        }
    }
}

fn harvest(
    cfg: &config::Config,
    repo: Option<PathBuf>,
    window_days: u64,
    lookback_days: u64,
) -> Result<usize> {
    let start = repo.unwrap_or_else(|| PathBuf::from("."));
    // Not a git repository → nothing to harvest. This is the one soft case:
    // there is no history to read, so there is nothing that went unrecorded.
    let Some(top) = git_opt(&start, &["rev-parse", "--show-toplevel"])? else {
        return Ok(0);
    };
    let top = PathBuf::from(top.trim());
    let Some(branch) = ["main", "master"].into_iter().find(|b| {
        git_opt(
            &top,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}"),
            ],
        )
        .ok()
        .flatten()
        .is_some()
    }) else {
        return Ok(0);
    };
    let key = git(
        &top,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?
    .trim()
    .to_string();

    let cursor_path = cursor_path();
    let mut cursor = load_cursor(&cursor_path)?;

    let now = store::now_secs();
    let window = window_days.saturating_mul(DAY);
    let oldest = now.saturating_sub(lookback_days.saturating_mul(DAY));
    let newest = now.saturating_sub(window);

    let log = git(
        &top,
        &[
            "log",
            "--first-parent",
            "--merges",
            &format!("--since=@{oldest}"),
            "--format=%H%x1f%ct%x1f%s",
            branch,
        ],
    )?;
    // Oldest first, so the cursor advances in history order.
    let mut merges: Vec<(String, u64, String)> = Vec::new();
    for line in log.lines().filter(|l| !l.is_empty()) {
        let mut parts = line.splitn(3, '\x1f');
        let (Some(sha), Some(ct), Some(subject)) = (parts.next(), parts.next(), parts.next())
        else {
            bail!("unparseable `git log` line in {}: {line:?}", top.display());
        };
        let ct: u64 = ct
            .parse()
            .with_context(|| format!("commit time of {sha} is not a number: {ct:?}"))?;
        merges.push((sha.to_string(), ct, subject.to_string()));
    }
    merges.reverse();

    let mut recorded = 0usize;
    for (sha, t, subject) in merges {
        if t < oldest || t > newest {
            continue; // outside the lookback, or the window has not closed yet
        }
        if cursor.get(&key).is_some_and(|s| s.contains(&sha)) {
            continue;
        }
        if let Some(ep) = episode_for_merge(&top, branch, &sha, t, &subject, window)? {
            store::append(&cfg.store_path(), &ep)
                .with_context(|| format!("appending the episode for merge {sha} to the store"))?;
            recorded += 1;
        }
        // Mark only after the append landed (a crash in between duplicates one
        // episode, which `import --dedup` can repair; the reverse order would
        // drop it, which nothing can).
        cursor.entry(key.clone()).or_default().insert(sha);
        save_cursor(&cursor_path, &cursor)?;
    }
    Ok(recorded)
}

/// Build the episode for one mature merge, or `None` when no commit on the
/// merged side names a model we route between (the merge is then skipped, but
/// still marked harvested by the caller).
fn episode_for_merge(
    top: &Path,
    branch: &str,
    sha: &str,
    t: u64,
    subject: &str,
    window: u64,
) -> Result<Option<store::Episode>> {
    let range = format!("{sha}^1..{sha}");
    let side = git(
        top,
        &[
            "log",
            "--no-merges",
            "--format=%at%x1f%(trailers:key=Co-Authored-By,valueonly,separator=%x1e)",
            &range,
        ],
    )?;
    let mut votes: HashMap<&'static str, usize> = HashMap::new();
    let mut earliest: Option<u64> = None;
    for line in side.lines().filter(|l| !l.is_empty()) {
        let (at, trailers) = line.split_once('\x1f').unwrap_or((line, ""));
        if let Ok(at) = at.trim().parse::<u64>() {
            earliest = Some(earliest.map_or(at, |e| e.min(at)));
        }
        for trailer in trailers.split('\x1e') {
            if let Some(m) = model_of(trailer) {
                *votes.entry(m).or_default() += 1;
            }
        }
    }
    // Majority; a tie resolves in fixed tier order so the result is
    // deterministic.
    let Some(model) = ["opus", "sonnet", "haiku"]
        .into_iter()
        .filter(|m| votes.get(m).copied().unwrap_or(0) > 0)
        .max_by_key(|m| (votes[m], std::cmp::Reverse(tier_rank(m))))
    else {
        return Ok(None);
    };

    let files: Vec<String> = git(top, &["diff", "--name-only", &format!("{sha}^1"), sha])?
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let signal: BTreeSet<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|f| !is_noise(f))
        .collect();

    let pass = !followed_by_fix(top, branch, sha, t, window, &signal)?;
    let duration_secs = earliest
        .and_then(|e| t.checked_sub(e))
        .filter(|d| *d > 0)
        .map(|d| d as f64);

    Ok(Some(store::Episode {
        ts: store::now_secs(),
        title: subject.to_string(),
        touched_files: files,
        class: "merge".to_string(),
        model: model.to_string(),
        role: "worker".to_string(),
        pass,
        duration_secs,
        ..Default::default()
    }))
}

/// Whether a `fix`/`revert` commit reachable from `branch`, not part of the
/// merge, committed in `(t, t + window]`, touches any of `signal`.
fn followed_by_fix(
    top: &Path,
    branch: &str,
    sha: &str,
    t: u64,
    window: u64,
    signal: &BTreeSet<&str>,
) -> Result<bool> {
    if signal.is_empty() {
        return Ok(false);
    }
    let until = t.saturating_add(window);
    let later = git(
        top,
        &[
            "log",
            "--no-merges",
            &format!("--since=@{t}"),
            &format!("--until=@{until}"),
            "--format=%H%x1f%ct%x1f%s",
            branch,
            &format!("^{sha}"),
        ],
    )?;
    for line in later.lines().filter(|l| !l.is_empty()) {
        let mut parts = line.splitn(3, '\x1f');
        let (Some(c), Some(ct), Some(subject)) = (parts.next(), parts.next(), parts.next()) else {
            bail!("unparseable `git log` line: {line:?}");
        };
        let ct: u64 = ct
            .parse()
            .with_context(|| format!("commit time of {c} is not a number: {ct:?}"))?;
        if ct <= t || ct > until || !is_fix_subject(subject) {
            continue;
        }
        let touched = git(
            top,
            &["diff-tree", "--no-commit-id", "--name-only", "-r", c],
        )?;
        if touched.lines().any(|f| signal.contains(f)) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn tier_rank(m: &str) -> u8 {
    match m {
        "opus" => 0,
        "sonnet" => 1,
        _ => 2,
    }
}

fn model_of(trailer: &str) -> Option<&'static str> {
    let t = trailer.to_lowercase();
    ["opus", "sonnet", "haiku"]
        .into_iter()
        .find(|m| t.contains(m))
}

/// `^fix\b` / `^revert\b`, case-insensitive (covers `fix(scope):` and
/// `Revert "…"`; rejects `fixture`, `reverted`).
fn is_fix_subject(subject: &str) -> bool {
    let s = subject.trim_start().to_lowercase();
    ["fix", "revert"].into_iter().any(|w| {
        s.strip_prefix(w).is_some_and(|rest| {
            rest.chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
        })
    })
}

/// Paths every change touches (version lockstep), which would otherwise make
/// any later fix look related to any merge.
fn is_noise(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    base == "Cargo.lock"
        || base == "Cargo.toml"
        || path.ends_with(".claude-plugin/plugin.json")
        || path.ends_with(".claude-plugin/marketplace.json")
}

// ---- git -------------------------------------------------------------------

/// Run git in `dir`; non-zero exit is an error.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    match git_opt(dir, args)? {
        Some(out) => Ok(out),
        None => bail!("git {args:?} failed in {}", dir.display()),
    }
}

/// Run git in `dir`; `Ok(None)` on a non-zero exit, `Err` when git could not
/// be run at all (spawn failure / timeout).
fn git_opt(dir: &Path, args: &[&str]) -> Result<Option<String>> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    let out = crate::spawn_and_wait_timeout(cmd, GIT_TIMEOUT)
        .with_context(|| format!("running git {args:?} in {}", dir.display()))?;
    if out.status.success() {
        Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
    } else {
        Ok(None)
    }
}

// ---- cursor ----------------------------------------------------------------

type Cursor = BTreeMap<String, BTreeSet<String>>;

fn cursor_path() -> PathBuf {
    config::home_dir()
        .join(".fugu-router")
        .join("harvest-cursor.json")
}

fn load_cursor(path: &Path) -> Result<Cursor> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Cursor::new()),
        Err(e) => bail!("could not read the harvest cursor {}: {e}", path.display()),
    };
    serde_json::from_str(&raw).with_context(|| {
        format!(
            "the harvest cursor {} did not parse; refusing to treat it as empty \
             (that would re-harvest and duplicate every merge in the lookback)",
            path.display()
        )
    })
}

fn save_cursor(path: &Path, cursor: &Cursor) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let body = serde_json::to_string(cursor).context("serializing the harvest cursor")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

// ---- failure marker ----------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct HarvestFailure {
    at: u64,
    detail: String,
}

fn marker_path() -> PathBuf {
    config::home_dir()
        .join(".fugu-router")
        .join("harvest-error.json")
}

fn record_marker(detail: &str) {
    let path = marker_path();
    let body = match serde_json::to_string(&HarvestFailure {
        at: store::now_secs(),
        detail: detail.to_string(),
    }) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("fugu-router: could not serialize the harvest-failure marker: {e}");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "fugu-router: could not create {} for the harvest-failure marker: {e}",
                parent.display()
            );
            return;
        }
    }
    if let Err(e) = std::fs::write(&path, body) {
        eprintln!(
            "fugu-router: could not write the harvest-failure marker to {}: {e}",
            path.display()
        );
    }
}

fn clear_marker() {
    let path = marker_path();
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "fugu-router: could not clear the harvest-failure marker at {}: {e}",
            path.display()
        ),
    }
}

/// The notice to surface, if the last `harvest` failed. An unreadable or
/// unparseable marker still returns a notice — its presence already says the
/// last harvest failed.
pub fn pending() -> Option<String> {
    let path = marker_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(format!(
                "fugu-router: the last merge harvest failed — a failure marker exists at {} \
                 but is unreadable ({e}). Merges are not becoming routing episodes. \
                 Run `fugu-router harvest` to see the real error.",
                path.display()
            ))
        }
    };
    Some(match serde_json::from_str::<HarvestFailure>(&raw) {
        Ok(f) => format!(
            "fugu-router: the last merge harvest failed (unix {}): {}. Merges are not \
             becoming routing episodes. Run `fugu-router harvest` to see the full error.",
            f.at, f.detail
        ),
        Err(e) => format!(
            "fugu-router: the last merge harvest failed — the failure marker at {} did not \
             parse ({e}). Run `fugu-router harvest` to see the real error.",
            path.display()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_subjects() {
        for s in ["fix: a", "Fix(core): b", "Revert \"x\"", "revert: y", "fix"] {
            assert!(is_fix_subject(s), "{s}");
        }
        for s in ["fixture: a", "reverted b", "feat: fix later", "prefix"] {
            assert!(!is_fix_subject(s), "{s}");
        }
    }

    #[test]
    fn noise_paths() {
        assert!(is_noise("Cargo.lock"));
        assert!(is_noise("crates/x/Cargo.toml"));
        assert!(is_noise("crates/x/.claude-plugin/plugin.json"));
        assert!(is_noise(".claude-plugin/marketplace.json"));
        assert!(!is_noise("crates/x/src/main.rs"));
    }
}
