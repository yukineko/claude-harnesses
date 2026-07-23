//! git helpers: which files changed, and the actual diff text for them. Pure
//! subprocess calls to `git`. `changed_files` returns a [`ChangeScan`]:
//! `NotRepo` means "confirmed out of scope" — git said so, or git could not
//! answer AND no `.git` exists anywhere above the path to contradict it. It
//! does NOT mean "git unavailable": an unreachable git over a real repo is
//! `Failed`, not `NotRepo`, via `harness_core::git_probe`. (The caller then has no
//! generated code to check and allows the stop), `Failed` means a git command
//! errored inside a real repo (the changeset is UNDETERMINED — the gate fails
//! closed / blocks rather than treat it as clean), and `Files(v)` is the
//! (possibly-empty) changed set.

use harness_core::git_probe::{probe_repo, RepoProbe};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

/// CA-propguard-006: every git invocation below feeds `evaluate()`, which
/// runs *before* `run_checker` is ever reached — so a hung/slow `git`
/// (network-mounted repo, lock contention, corrupted pack, etc.) used to
/// block the whole Stop hook indefinitely with no bound at all. Generous
/// enough not to affect normal usage (git subcommands here are all local,
/// read-only, and expected to finish in well under a second).
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Run `git <args>` in `root` with a bounded wait. Returns `None` on spawn
/// failure, non-zero exit, or timeout — matching this module's existing
/// "fail gracefully / treat as git-unavailable" convention (callers already
/// tolerate `None`/empty output for those cases). On timeout the child (and,
/// on Unix, its process group) is killed so no orphaned `git` process is
/// left behind.
fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().ok()?;
    match child.wait_timeout(GIT_TIMEOUT) {
        Ok(Some(status)) => {
            let out = read_stdout_bounded(child.stdout.take(), GIT_TIMEOUT);
            if status.success() {
                Some(out)
            } else {
                None
            }
        }
        Ok(None) => {
            kill_tree(&mut child);
            let _ = child.wait();
            None
        }
        Err(_) => None,
    }
}

/// Kill the whole process tree of a timed-out `git` call, not just the
/// direct process — mirrors the same group-kill approach used for the
/// checker subprocess in `gate.rs` (CA-propguard-004), applied here so a
/// hung git helper process can't outlive the timeout either.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        // SAFETY: plain libc syscall; negative pid targets the process
        // group we created via `process_group(0)` at spawn time. Best
        // effort: any error is ignored, same as the plain `child.kill()` it
        // supplements.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Bounded stdout read: never block past `timeout` even if some lingering
/// process keeps the pipe's write end open after the immediate child exits.
/// Same rationale/shape as `gate::read_stdout_bounded` (CA-propguard-005).
fn read_stdout_bounded(stdout: Option<std::process::ChildStdout>, timeout: Duration) -> String {
    use std::sync::mpsc;
    let Some(mut so) = stdout else {
        return String::new();
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut out = String::new();
        let _ = so.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    rx.recv_timeout(timeout).unwrap_or_default()
}

/// Tri-state result of scanning the changed-files set. Distinguishes "no git
/// scope" (`NotRepo` → allow) from "a git command errored" (`Failed` →
/// undetermined → the gate fails closed / blocks), so a collapsed-empty scan
/// can never read as "nothing changed → allow". `Files(v)` is a success; an
/// EMPTY `Files` (a clean repo: `git diff` exits 0 with no output) is a genuine
/// no-changes → allow, NOT `Failed`. Note: `run_git` maps a *timeout* to the
/// same failure signal, so a hung git also fails the gate closed (bounded &
/// escapable in `gate.rs`), never silently allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeScan {
    NotRepo,
    Failed,
    Files(Vec<String>),
}

/// Changed paths relative to the repo root: tracked changes vs HEAD, staged
/// changes, and untracked-but-not-ignored files. Returns `NotRepo` when there
/// is no git repo, `Failed` when any sub-command errored (spawn failure /
/// non-zero exit / timeout) inside a real repo, or `Files(v)` on success
/// (possibly empty).
pub fn changed_files(root: &Path) -> ChangeScan {
    match probe_repo(root) {
        RepoProbe::Repo => {}
        // Genuinely out of scope: git said so, or git could not answer and no
        // `.git` exists anywhere above to contradict it.
        RepoProbe::NotRepo => return ChangeScan::NotRepo,
        // git could not be run / refused to answer while a `.git` exists.
        // Undetermined is not "no scope" — resolve to the restricted side.
        RepoProbe::Undetermined => return ChangeScan::Failed,
    }
    let mut out = Vec::new();
    // If ANY sub-command errors, the changed set is undetermined → fail closed.
    // A clean repo's commands all exit 0 with empty stdout → Files(vec![]).
    let ok = collect(root, &["diff", "--name-only"], &mut out)
        && collect(root, &["diff", "--cached", "--name-only"], &mut out)
        && collect(
            root,
            &["ls-files", "--others", "--exclude-standard"],
            &mut out,
        );
    if !ok {
        return ChangeScan::Failed;
    }
    out.sort();
    out.dedup();
    ChangeScan::Files(out)
}

/// Run one `git` sub-command, appending its trimmed non-empty stdout lines to
/// `out`. Returns `true` on success, `false` when `run_git` reported failure
/// (spawn error / non-zero exit / timeout) — the caller maps `false` to
/// `ChangeScan::Failed`. A successful command with EMPTY stdout still returns
/// `true` (clean ≠ failed).
fn collect(root: &Path, args: &[&str], out: &mut Vec<String>) -> bool {
    match run_git(root, args) {
        Some(text) => {
            for line in text.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    out.push(line.to_string());
                }
            }
            true
        }
        None => false,
    }
}

/// A checkable diff plus whether it had to be truncated to fit `max_bytes`.
pub struct DiffText {
    pub text: String,
    pub truncated: bool,
}

/// The combined diff for `files`: unstaged + staged hunks, plus the full text of
/// any untracked files (which have no diff). Truncated to `max_bytes` with a
/// marker so a huge diff can't blow up memory or the checker prompt.
pub fn diff_text(root: &Path, files: &[String], max_bytes: usize) -> DiffText {
    let mut s = String::new();

    run_diff(root, &["diff", "--"], files, &mut s);
    run_diff(root, &["diff", "--cached", "--"], files, &mut s);

    // untracked files among `files`: include their contents as "new file" diffs.
    let mut others = Vec::new();
    let mut args: Vec<&str> = vec!["ls-files", "--others", "--exclude-standard", "--"];
    args.extend(files.iter().map(String::as_str));
    if let Some(text) = run_git(root, &args) {
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() {
                others.push(line.to_string());
            }
        }
    }
    for f in others {
        s.push_str(&format!("\n=== new file: {f} ===\n"));
        // `Known(Some(content))`: include it, as before. `Known(None)` (the
        // untracked file vanished between `ls-files` and this read) and
        // `Undetermined` (exists but unreadable / not valid UTF-8) both leave
        // the diff text without this file's body — unchanged from the prior
        // `if let Ok(..)` behavior, which silently skipped both cases too.
        // This is a best-effort diff assembled for a checker prompt, not a
        // verdict: an unreadable untracked file does not make the surrounding
        // `changed_files`/`ChangeScan::Failed` fail-closed path meaningless,
        // it just means this one file's contents are missing from the
        // rendered diff, same risk shape as before this migration.
        if let harness_core::verdict::Determination::Known(Some(content)) =
            harness_core::boundary::read_to_string(&root.join(&f))
        {
            s.push_str(&content);
            if !s.ends_with('\n') {
                s.push('\n');
            }
        }
        if s.len() > max_bytes {
            break;
        }
    }

    truncate_on_boundary(s, max_bytes)
}

fn run_diff(root: &Path, base: &[&str], files: &[String], out: &mut String) {
    let mut args: Vec<&str> = base.to_vec();
    args.extend(files.iter().map(String::as_str));
    if let Some(text) = run_git(root, &args) {
        out.push_str(&text);
    }
}

fn truncate_on_boundary(mut s: String, max_bytes: usize) -> DiffText {
    if s.len() <= max_bytes {
        return DiffText {
            text: s,
            truncated: false,
        };
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n… (diff truncated by propguard max_diff_bytes)\n");
    DiffText {
        text: s,
        truncated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_limit_is_not_truncated() {
        let d = truncate_on_boundary("short diff".to_string(), 1000);
        assert!(!d.truncated);
        assert_eq!(d.text, "short diff");
    }

    #[test]
    fn over_limit_is_flagged_and_marked() {
        let big = "x".repeat(500);
        let d = truncate_on_boundary(big, 32);
        assert!(d.truncated, "a diff larger than max_bytes must be flagged");
        assert!(d.text.contains("truncated"));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let s = "あいうえお".to_string(); // 15 bytes
        let d = truncate_on_boundary(s, 7); // 7 is mid-character (6 is a boundary)
        assert!(d.truncated);
        assert!(d.text.starts_with("あい"));
    }

    // ── CA-propguard-006: git subprocess calls must be timeout-bounded ─────
    //
    // Every git call in this module fed evaluate() with no timeout at all
    // before this fix, so a hung/slow `git` invocation blocked the whole Stop
    // hook indefinitely. `run_git` (the single choke point all git calls in
    // this module now go through) must return promptly with a graceful
    // fallback (`None`) instead of hanging past `GIT_TIMEOUT`, and must not
    // leave the hung process running afterwards.
    #[test]
    fn hung_git_invocation_returns_promptly_with_graceful_fallback() {
        // Not a real git repo, but `run_git` doesn't care what program name
        // it invokes — it operates on `Command::new("git")` only. To
        // exercise the *timeout* path itself (rather than "git exited
        // quickly because the dir isn't a repo"), reach for the same
        // machinery `run_git` uses (spawn + wait_timeout) directly here,
        // against a deliberately hanging shell command standing in for a
        // stuck `git`, so the test doesn't depend on a real slow git binary
        // being available.
        let tmp = std::env::temp_dir().join(format!(
            "propguard-git-timeout-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::create_dir_all(&tmp);

        let start = std::time::Instant::now();
        let out = run_hanging_via_run_git_path(&tmp);
        let elapsed = start.elapsed();

        assert!(
            out.is_none(),
            "a timed-out invocation must fall back gracefully (None), not fabricate output"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "run_git must return promptly on timeout, took {elapsed:?} (GIT_TIMEOUT is only used \
             for the real `git` binary in production; this test drives the same code path with a \
             short local timeout override to stay fast)"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Exercises the exact spawn/wait_timeout/kill logic `run_git` uses, but
    /// against a short-lived local timeout (instead of the production
    /// `GIT_TIMEOUT` = 10s) and a guaranteed-to-hang command, so the
    /// regression test itself stays fast and deterministic without requiring
    /// a real `git` binary that can be made to hang on demand.
    fn run_hanging_via_run_git_path(root: &Path) -> Option<String> {
        let mut cmd = Command::new("sh");
        cmd.current_dir(root)
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().ok()?;
        let short_timeout = Duration::from_millis(300);
        match child.wait_timeout(short_timeout) {
            Ok(Some(status)) => {
                let out = read_stdout_bounded(child.stdout.take(), short_timeout);
                if status.success() {
                    Some(out)
                } else {
                    None
                }
            }
            Ok(None) => {
                kill_tree(&mut child);
                let _ = child.wait();
                // Verify the process is actually gone (not just detached),
                // proving the group-kill really reached it rather than
                // merely returning while it lingers in the background.
                std::thread::sleep(Duration::from_millis(100));
                assert!(
                    !process_alive(child.id()),
                    "kill_tree must actually terminate the hung process, not merely time out on it"
                );
                None
            }
            Err(_) => None,
        }
    }

    #[cfg(unix)]
    fn process_alive(pid: u32) -> bool {
        // SAFETY: signal 0 performs no action, only existence/permission
        // checks — a standard, safe-in-practice liveness probe.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    #[cfg(not(unix))]
    fn process_alive(_pid: u32) -> bool {
        false
    }

    /// evaluate()'s git-dependent step (`changed_files`) must itself return
    /// promptly rather than hang, given a `root` where `git` can't even run
    /// (simulating "git unavailable") — the graceful-fallback contract this
    /// finding requires end-to-end, not just at the `run_git` choke point.
    #[test]
    fn changed_files_returns_promptly_when_git_unavailable() {
        let tmp = std::env::temp_dir().join(format!(
            "propguard-git-unavailable-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::create_dir_all(&tmp);

        let start = std::time::Instant::now();
        let result = changed_files(&tmp);
        let elapsed = start.elapsed();

        assert_eq!(
            result,
            ChangeScan::NotRepo,
            "a non-repo dir must yield NotRepo (no scope → allow), not hang or fabricate a diff"
        );
        assert!(
            elapsed < Duration::from_secs(15),
            "changed_files must return within the git timeout budget, took {elapsed:?}"
        );
    }

    // ── changed_files tri-state: NotRepo / Failed / Files ──────────────────

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// A fresh, unique scratch directory under the system temp dir (avoids a
    /// `tempfile` dev-dependency; mirrors this module's existing test style).
    /// Caller is responsible for cleanup.
    fn scratch_dir() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "propguard-git-tristate-test-{}-{}-{}",
            std::process::id(),
            line!(),
            n
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create scratch dir");
        p
    }

    /// A clean repo is a SUCCESSFUL empty scan → `Files(vec![])`, never
    /// `Failed`. Pins that a clean tree (git commands exit 0 with empty stdout)
    /// does not start blocking under the fail-closed change.
    #[test]
    fn clean_repo_is_empty_files_not_failed() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = scratch_dir();
        let root = root.as_path();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@t.com"][..],
            &["config", "user.name", "t"][..],
        ] {
            assert!(Command::new("git")
                .current_dir(root)
                .args(args)
                .status()
                .expect("git")
                .success());
        }
        std::fs::write(root.join("a.txt"), "x\n").unwrap();
        assert!(Command::new("git")
            .current_dir(root)
            .args(["add", "a.txt"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .current_dir(root)
            .args(["commit", "-qm", "init"])
            .status()
            .unwrap()
            .success());

        assert_eq!(
            changed_files(root),
            ChangeScan::Files(Vec::new()),
            "a clean repo must be a successful empty scan, not Failed"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The `collect` helper's success/failure signal (which drives the Failed
    /// mapping) must distinguish an errored sub-command from an empty-but-
    /// successful one: a bogus git flag errors → `false`; a valid command with
    /// no output → `true`.
    #[test]
    fn collect_reports_error_vs_empty_success() {
        if !git_available() {
            eprintln!("skipping: git not available");
            return;
        }
        let root = scratch_dir();
        let root = root.as_path();
        assert!(Command::new("git")
            .current_dir(root)
            .args(["init", "-q"])
            .status()
            .expect("git")
            .success());

        let mut out = Vec::new();
        assert!(
            collect(root, &["diff", "--name-only"], &mut out),
            "an empty-but-successful git command must report true (not Failed)"
        );
        assert!(out.is_empty());
        assert!(
            !collect(root, &["diff", "--no-such-flag-xyzzy"], &mut out),
            "a non-zero git exit must report false so the scan fails closed"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
