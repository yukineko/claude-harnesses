// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Black-box boundary tests for backlog f71ac81a: reviewgate's git subprocess
//! calls (`git::changed_files` / `git::diff_text`) had no timeout, so a hung
//! `git` hung the Stop gate forever instead of resolving to a block.
//!
//! reviewgate is bin-only, so the binary is driven as a CHILD process with a
//! fake `git` first on PATH. The fake answers `rev-parse` (and, in the
//! control/clamp cases, everything) by exec'ing the REAL git, and hangs only on
//! the sub-commands under test — `harness_core::git_probe::probe_repo` runs
//! `rev-parse` with no timeout, so hanging that would hang the test for a
//! reason outside this bug.
//!
//! Every run is bounded by a wall-clock cap: on expiry the child is killed and
//! the test FAILS (never hangs the suite). The seam is
//! `REVIEWGATE_GIT_TIMEOUT_MS` (accepted 1..=10000, anything else -> the
//! production 10 s bound).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const WALL_CAP: Duration = Duration::from_secs(10);
const BLOCK_REASON_SCAN_FAILED: &str = "変更内容を特定できませんでした";

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "reviewgate-gittimeout-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

/// The real git, found on the ambient PATH (the fake dir is not on it yet).
fn real_git() -> String {
    let out = Command::new("which")
        .arg("git")
        .output()
        .expect("`which git` runs");
    assert!(out.status.success(), "no real git on PATH");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new(real_git())
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} failed");
}

/// A real repo with one committed file and one uncommitted edit.
fn make_repo(root: &Path) -> PathBuf {
    let work = root.join("work");
    std::fs::create_dir_all(work.join("src")).unwrap();
    git(&work, &["init", "-q"]);
    std::fs::write(work.join("src").join("lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();
    git(&work, &["add", "src/lib.rs"]);
    git(&work, &["commit", "-qm", "init"]);
    std::fs::write(work.join("src").join("lib.rs"), "pub fn f() -> u8 { 2 }\n").unwrap();
    work
}

#[derive(Clone, Copy)]
enum Hang {
    /// Delegate every call to the real git.
    Nothing,
    /// Hang on every `diff` / `ls-files`, including the name-only scans.
    AllScans,
    /// Answer the name-only scans (nonempty change list) but hang the
    /// diff-content fetch (`diff --`, `diff --cached --`, `ls-files ... --`).
    DiffFetchOnly,
}

/// Write a fake `git` into `dir` and return `dir`.
fn fake_git(dir: &Path, hang: Hang) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let real = real_git();
    let body = match hang {
        Hang::Nothing => format!("exec '{real}' \"$@\"\n"),
        Hang::AllScans => format!(
            "case \"$1\" in\n  diff|ls-files) exec /bin/sleep 20 ;;\nesac\nexec '{real}' \"$@\"\n"
        ),
        Hang::DiffFetchOnly => format!(
            "case \"$1\" in\n  diff|ls-files)\n    for a in \"$@\"; do\n      if [ \"$a\" = \"--name-only\" ]; then\n        if [ \"$1\" = diff ] && [ \"$2\" = \"--name-only\" ]; then echo src/lib.rs; fi\n        exit 0\n      fi\n    done\n    case \"$*\" in\n      \"ls-files --others --exclude-standard\") exit 0 ;;\n    esac\n    exec /bin/sleep 20 ;;\nesac\nexec '{real}' \"$@\"\n"
        ),
    };
    let path = dir.join("git");
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir.to_path_buf()
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    elapsed: Duration,
    timed_out: bool,
}

fn run_review(work: &Path, home: &Path, fake_dir: &Path, timeout_ms: Option<&str>) -> Run {
    let ambient = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![fake_dir.to_path_buf()];
    paths.extend(std::env::split_paths(&ambient));
    let path = std::env::join_paths(paths).unwrap();
    let payload = format!(
        r#"{{"hook_event_name":"Stop","session_id":"git-timeout-boundary","stop_hook_active":false,"cwd":{}}}"#,
        serde_json::to_string(&work.to_string_lossy()).unwrap()
    );
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_reviewgate"));
    cmd.arg("review")
        .current_dir(work)
        .env("HOME", home)
        .env("PATH", path)
        .env_remove("REVIEWGATE_GIT_TIMEOUT_MS")
        .env_remove("REVIEWGATE_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(v) = timeout_ms {
        cmd.env("REVIEWGATE_GIT_TIMEOUT_MS", v);
    }
    let start = Instant::now();
    let mut child = cmd.spawn().expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let mut timed_out = false;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None if start.elapsed() > WALL_CAP => {
                timed_out = true;
                let _ = child.kill();
                break;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let elapsed = start.elapsed();
    let out = child.wait_with_output().expect("collect output");
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        elapsed,
        timed_out,
    }
}

fn is_block(stdout: &str) -> bool {
    stdout.contains("\"decision\":\"block\"") || stdout.contains("\"decision\": \"block\"")
}

fn assert_scan_failed_block(r: &Run) {
    assert!(
        !r.timed_out,
        "reviewgate did not return within {WALL_CAP:?} (killed after {:?}): a hung git must \
         resolve to a bounded block, not hang the Stop gate. stdout: {:?} stderr: {:?}",
        r.elapsed, r.stdout, r.stderr
    );
    assert_eq!(r.code, Some(0), "Stop hook exits 0; stderr: {:?}", r.stderr);
    assert!(
        is_block(&r.stdout),
        "a git that timed out leaves the change set UNDETERMINED -> must block, not allow. \
         stdout: {:?} stderr: {:?}",
        r.stdout,
        r.stderr
    );
    assert!(
        r.stdout.contains(BLOCK_REASON_SCAN_FAILED),
        "the block must be the git-scan-failed block, not the panic barrier's generic \
         fail-closed block. stdout: {:?} stderr: {:?}",
        r.stdout,
        r.stderr
    );
}

fn assert_no_scan_failure(r: &Run) {
    assert!(
        !r.timed_out,
        "fast git must not hang: killed after {:?}. stdout: {:?} stderr: {:?}",
        r.elapsed, r.stdout, r.stderr
    );
    assert!(
        !r.stdout.contains(BLOCK_REASON_SCAN_FAILED),
        "a fast, working git must not produce git-scan-failed. stdout: {:?} stderr: {:?}",
        r.stdout,
        r.stderr
    );
    assert!(
        !r.stderr.contains("diff content fetch failed"),
        "a fast, working git must not fail the diff fetch. stderr: {:?}",
        r.stderr
    );
}

/// Case 1: every diff/ls-files call hangs (incl. the name-only scans).
#[test]
fn hung_git_scan_resolves_to_git_scan_failed_block_within_cap() {
    let root = unique_dir("allscans");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = make_repo(&root);
    let fake = fake_git(&root.join("fakebin"), Hang::AllScans);

    let r = run_review(&work, &home, &fake, Some("300"));

    assert_scan_failed_block(&r);
    let _ = std::fs::remove_dir_all(&root);
}

/// Case 2: name-only scans answer with a nonempty list; only the diff-content
/// fetch hangs. It must block (not allow) and say it timed out.
#[test]
fn hung_diff_fetch_blocks_and_reports_timed_out() {
    let root = unique_dir("difffetch");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = make_repo(&root);
    let fake = fake_git(&root.join("fakebin"), Hang::DiffFetchOnly);

    let r = run_review(&work, &home, &fake, Some("300"));

    assert_scan_failed_block(&r);
    assert!(
        r.stderr.contains("timed out after"),
        "the diff-fetch failure must name the timeout. stderr: {:?}",
        r.stderr
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Case 3, control: a fake git that delegates everything to real git on a repo
/// with a real change must NOT yield git-scan-failed.
#[test]
fn control_delegating_git_does_not_scan_fail() {
    let root = unique_dir("control");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = make_repo(&root);
    let fake = fake_git(&root.join("fakebin"), Hang::Nothing);

    let r = run_review(&work, &home, &fake, Some("300"));

    assert_no_scan_failure(&r);
    assert_eq!(r.code, Some(0));
    let _ = std::fs::remove_dir_all(&root);
}

/// Case 4, seam clamping: out-of-range / unparsable values fall back to the
/// production bound, never to 0 — a fast git must still succeed.
#[test]
fn bad_timeout_values_do_not_fail_a_fast_git() {
    for (i, val) in ["0", "garbage", "999999", "", "-5"].iter().enumerate() {
        let root = unique_dir(&format!("clamp{i}"));
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let work = make_repo(&root);
        let fake = fake_git(&root.join("fakebin"), Hang::Nothing);

        let r = run_review(&work, &home, &fake, Some(val));

        assert!(
            !r.timed_out
                && !r.stdout.contains(BLOCK_REASON_SCAN_FAILED)
                && !r.stderr.contains("diff content fetch failed"),
            "REVIEWGATE_GIT_TIMEOUT_MS={val:?} broke a fast git (must fall back to the \
             production bound, not 0). timed_out={} stdout: {:?} stderr: {:?}",
            r.timed_out,
            r.stdout,
            r.stderr
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
