//! freshness — the **C2 deterministic signals** (DESIGN §12 "C2 鮮度の決定的信号").
//!
//! A cheap floor evaluated before any LLM gate. [`check`] runs four
//! deterministic signals over the charter and the repo; `stale = true` if ANY
//! trips, with a human-readable reason recorded per tripped signal.
//!
//! This module is deliberately standalone (only depends on [`Charter`] /
//! [`Config`] and `git` / `std::fs`) so the future SessionStart `nudge` hook can
//! reuse it without pulling in the gather/gates machinery. No LLM, no network.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use wait_timeout::ChildExt;

use crate::charter::Charter;
use crate::config::Config;

/// Hard timeout for any `git` subprocess spawned from this module. A repo on a
/// network mount, a huge history, or a lock contention shouldn't be able to
/// hang the hook turn — fail-soft to `None` instead (DESIGN: deterministic
/// signals must never block).
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Result of the C2 deterministic floor. `reasons` is empty iff `!stale`.
#[derive(Debug, Clone, Default)]
pub struct Freshness {
    pub stale: bool,
    pub reasons: Vec<String>,
}

impl Freshness {
    fn trip(&mut self, reason: impl Into<String>) {
        self.stale = true;
        self.reasons.push(reason.into());
    }
}

/// Seconds per day, for the elapsed-days signal.
const SECS_PER_DAY: u64 = 86_400;

/// Run the C2 deterministic signals. `charter_path` is the on-disk charter file
/// (used for the commit-divergence and mtime checks); `charter` is its parsed
/// form (used for DoD-ref and next_action checks).
pub fn check(repo_root: &Path, charter_path: &Path, charter: &Charter, cfg: &Config) -> Freshness {
    let mut f = Freshness::default();

    commit_divergence(&mut f, repo_root, charter_path, cfg);
    elapsed_days(&mut f, repo_root, charter_path, cfg);
    if cfg.freshness.check_dod_refs {
        dod_refs_missing(&mut f, repo_root, charter);
    }
    next_action_divergence(&mut f, repo_root, charter);

    f
}

/// Signal 1 — commit divergence: commits since the charter file was last
/// committed > `stale_commits`. If the charter isn't committed yet, that itself
/// is a reason. Skips silently in a non-git repo (no signal, not a trip).
fn commit_divergence(f: &mut Freshness, repo_root: &Path, charter_path: &Path, cfg: &Config) {
    // Non-git repo: this signal contributes nothing.
    if git_stdout(repo_root, &["rev-parse", "--git-dir"]).is_none() {
        return;
    }

    let path_arg = charter_path.to_string_lossy().to_string();
    // The commit that last touched the charter file.
    let last = git_stdout(
        repo_root,
        &["log", "-n", "1", "--format=%H", "--", &path_arg],
    )
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());

    let Some(last_commit) = last else {
        // File exists but git has never recorded it (or it's untracked).
        if charter_path.exists() {
            f.trip("charter not yet committed");
        }
        return;
    };

    let range = format!("{last_commit}..HEAD");
    if let Some(count) = git_stdout(repo_root, &["rev-list", "--count", &range])
        .and_then(|s| s.trim().parse::<u32>().ok())
    {
        if count > cfg.freshness.stale_commits {
            f.trip(format!(
                "{count} commits since charter last touched (> stale_commits={})",
                cfg.freshness.stale_commits
            ));
        }
    }
}

/// Signal 2 — elapsed days: wall-clock since the charter's last touch >
/// `stale_days`. Prefers the charter file's git author date; falls back to the
/// filesystem mtime. Uses `SystemTime::now()` (allowed in a normal binary).
fn elapsed_days(f: &mut Freshness, repo_root: &Path, charter_path: &Path, cfg: &Config) {
    let now = SystemTime::now();

    // Prefer the last-commit unix time for the charter file.
    let path_arg = charter_path.to_string_lossy().to_string();
    let committed = git_stdout(
        repo_root,
        &["log", "-n", "1", "--format=%ct", "--", &path_arg],
    )
    .and_then(|s| s.trim().parse::<u64>().ok())
    .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs));

    // Fall back to filesystem mtime.
    let last_touch = committed.or_else(|| std::fs::metadata(charter_path).ok()?.modified().ok());

    let Some(last_touch) = last_touch else {
        return; // can't determine a time => no signal.
    };

    if let Ok(elapsed) = now.duration_since(last_touch) {
        let days = elapsed.as_secs() / SECS_PER_DAY;
        if days > cfg.freshness.stale_days as u64 {
            f.trip(format!(
                "{days} days since charter last touched (> stale_days={})",
                cfg.freshness.stale_days
            ));
        }
    }
}

/// Signal 3 — DoD ref missing: scan each `definition_of_done` item for path-like
/// tokens (containing `/` or ending in a file extension) and flag any that don't
/// exist on disk. A vanished referenced path is a strong stale signal.
fn dod_refs_missing(f: &mut Freshness, repo_root: &Path, charter: &Charter) {
    for item in &charter.definition_of_done {
        for token in item.split_whitespace() {
            let tok = token.trim_matches(|c: char| {
                matches!(c, '`' | '"' | '\'' | '(' | ')' | ',' | '.' | ':' | ';')
            });
            if tok.is_empty() || !looks_like_path(tok) {
                continue;
            }
            // Re-trim trailing '.' was stripped above; check existence relative
            // to the repo root (and as-is for absolute paths).
            let candidate = if Path::new(tok).is_absolute() {
                std::path::PathBuf::from(tok)
            } else {
                repo_root.join(tok)
            };
            if !candidate.exists() {
                f.trip(format!("DoD references missing path `{tok}`"));
            }
        }
    }
}

/// A token "looks like a path" if it contains a `/` separator or ends in a
/// short alphanumeric file extension (e.g. `main.rs`, `config.toml`). Kept
/// conservative to avoid flagging prose words with a trailing period.
fn looks_like_path(tok: &str) -> bool {
    if tok.contains('/') {
        return true;
    }
    if let Some((stem, ext)) = tok.rsplit_once('.') {
        return !stem.is_empty()
            && !ext.is_empty()
            && ext.len() <= 5
            && ext.chars().all(|c| c.is_ascii_alphanumeric());
    }
    false
}

/// Signal 4 — next_action divergence (soft heuristic).
///
/// Rule: if `next_action` is non-empty, tokenize it and the recent commit
/// subjects, then measure overlap. Tokens are word-level for ASCII and
/// **char-level for CJK** (repo convention — CJK text isn't whitespace-
/// delimited). If there ARE recent commits but NONE of them shares any
/// significant token with `next_action`, the project "clearly moved on to
/// unrelated work" → flag drift. When there are no commits, or any commit does
/// share a token, we stay silent (a soft floor, not a hard claim).
fn next_action_divergence(f: &mut Freshness, repo_root: &Path, charter: &Charter) {
    let na = charter.next_action.trim();
    if na.is_empty() {
        return;
    }
    let Some(log) = git_stdout(repo_root, &["log", "--oneline", "-n", "10"]) else {
        return; // non-git or no log => no signal.
    };
    let subjects: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    if subjects.is_empty() {
        return; // no recent work to diverge from.
    }

    let na_tokens = tokenize(na);
    if na_tokens.is_empty() {
        return;
    }

    let any_related = subjects.iter().any(|subj| {
        let toks = tokenize(subj);
        na_tokens.iter().any(|t| toks.contains(t))
    });

    if !any_related {
        f.trip(format!(
            "recent commits share no token with next_action ({:?}) — work may have moved on",
            na
        ));
    }
}

/// Tokenize for the overlap heuristic: ASCII runs are lowercased word tokens
/// (>=3 chars, to drop stopword-ish noise); CJK characters become one token
/// each (char-level, per repo convention). Returns a de-duped set-like Vec.
fn tokenize(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut Vec<String>| {
        if word.len() >= 3 {
            let w = word.to_lowercase();
            if !out.contains(&w) {
                out.push(w);
            }
        }
        word.clear();
    };
    for ch in s.chars() {
        if is_cjk(ch) {
            flush(&mut word, &mut out);
            let c = ch.to_string();
            if !out.contains(&c) {
                out.push(c);
            }
        } else if ch.is_alphanumeric() {
            word.push(ch);
        } else {
            flush(&mut word, &mut out);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Rough CJK detection (Han / Hiragana / Katakana ranges) for char-level tokens.
fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3040..=0x30FF      // Hiragana + Katakana
        | 0x3400..=0x4DBF    // CJK Ext A
        | 0x4E00..=0x9FFF    // CJK Unified
        | 0xF900..=0xFAFF    // CJK Compatibility
    )
}

/// Run `git <args>` in `repo_root`, returning trimmed stdout on success, or
/// `None` for any failure (non-git, missing git, non-zero exit, or timeout).
fn git_stdout(repo_root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo_root).args(args);
    run_bounded(cmd, GIT_TIMEOUT)
}

/// Spawn `cmd`, wait up to `timeout` for it to finish, and return its stdout
/// on success. Fail-soft on every edge case: spawn failure, non-zero exit, or
/// timeout (in which case the child is killed) all return `None` rather than
/// panicking or propagating an error. This keeps deterministic-signal
/// gathering from ever hanging a hook turn.
fn run_bounded(mut cmd: Command, timeout: Duration) -> Option<String> {
    use std::process::Stdio;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            if !status.success() {
                return None;
            }
            let mut out = Vec::new();
            if let Some(mut s) = child.stdout.take() {
                use std::io::Read;
                let _ = s.read_to_end(&mut out);
            }
            Some(String::from_utf8_lossy(&out).to_string())
        }
        Ok(None) => {
            // Timed out: kill the child so it doesn't linger, then bail.
            let _ = child.kill();
            let _ = child.wait();
            None
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deliberately slow "command" (sleeps longer than the timeout) to
    /// verify `run_bounded` actually kills and returns `None` on timeout,
    /// rather than blocking the test (and, in production, the hook turn).
    #[cfg(unix)]
    #[test]
    fn run_bounded_times_out_on_slow_command_and_returns_none() {
        let mut cmd = Command::new("sleep");
        cmd.arg("5"); // much longer than the timeout below.
        let start = std::time::Instant::now();
        let result = run_bounded(cmd, Duration::from_millis(200));
        let elapsed = start.elapsed();
        assert!(result.is_none(), "timed-out command must yield None");
        assert!(
            elapsed < Duration::from_secs(2),
            "run_bounded should return promptly after timeout, took {elapsed:?}"
        );
    }

    #[test]
    fn run_bounded_returns_stdout_on_fast_command() {
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        let result = run_bounded(cmd, Duration::from_secs(5));
        assert_eq!(
            result.map(|s| s.trim().to_string()),
            Some("hello".to_string())
        );
    }

    #[test]
    fn git_stdout_returns_none_for_nonexistent_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Not a git repo: rev-parse should fail (non-zero exit) => None.
        assert!(git_stdout(dir.path(), &["rev-parse", "--git-dir"]).is_none());
    }

    #[test]
    fn dod_missing_path_trips_when_check_enabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let charter = Charter {
            north_star: "x".to_string(),
            definition_of_done: vec!["the file src/does_not_exist.rs must compile".to_string()],
            ..Charter::default()
        };
        let cfg = Config::default(); // check_dod_refs = true
        let charter_path = Charter::project_path(dir.path());

        let f = check(dir.path(), &charter_path, &charter, &cfg);
        assert!(f.stale, "missing DoD path should mark stale");
        assert!(f.reasons.iter().any(|r| r.contains("does_not_exist.rs")));
    }

    #[test]
    fn dod_present_path_does_not_trip_on_that_signal() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "// ok\n").unwrap();
        let charter = Charter {
            north_star: "x".to_string(),
            definition_of_done: vec!["src/lib.rs exists".to_string()],
            ..Charter::default()
        };
        let charter_path = Charter::project_path(dir.path());
        let f = check(dir.path(), &charter_path, &charter, &Config::default());
        // The DoD-ref signal must not have produced a reason about lib.rs.
        assert!(!f.reasons.iter().any(|r| r.contains("lib.rs")));
    }

    #[test]
    fn looks_like_path_heuristic() {
        assert!(looks_like_path("src/main.rs"));
        assert!(looks_like_path("config.toml"));
        assert!(looks_like_path("a/b"));
        assert!(!looks_like_path("compiles"));
        assert!(!looks_like_path("done"));
    }

    #[test]
    fn tokenize_handles_cjk_char_level() {
        let toks = tokenize("ゴール gather rs");
        assert!(toks.contains(&"ゴ".to_string()));
        assert!(toks.contains(&"gather".to_string()));
        // "rs" is < 3 chars => dropped as ASCII noise.
        assert!(!toks.contains(&"rs".to_string()));
    }
}
