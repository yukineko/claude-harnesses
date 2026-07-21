//! RED wiring test for backlog 7d3db473 (tdd's share of it).
//!
//! `tdd::git::is_git_repo` collapses "git could not be run" into "not a git
//! repo", and `tdd::gate::classify` maps `ChangeScan::NotRepo` to
//! `git_unscoped` → ALLOW. So a Stop hook running without `git` on PATH lets
//! every untested change through, in a real repo.
//!
//! After the fix the probe must answer `Undetermined` there, `changed_files` /
//! `added_lines` must report their BLOCKING variant (`ChangeScan::Failed` /
//! `AddedScan::Failed`), and the gate must BLOCK.
//!
//! These drive the real binary in a CHILD process, so the `PATH` used to make
//! git unspawnable is set only for that child — no process-global env mutation,
//! hence no race with the in-crate tests that spawn git. (tdd is a bin-only
//! crate with no lib target, so an integration test cannot call
//! `git::changed_files` directly; the `Failed` mapping is pinned here through
//! its observable consequence — the distinct git-scan block reason.)

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn unique_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("tdd-probe-{}-{}-{}", tag, std::process::id(), n));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

/// Run `tdd gate` with `payload` on stdin, `cwd`/HOME isolated, and `path` as
/// the child's PATH. Returns (exit_code, stdout).
fn run_gate(cwd: &Path, home: &Path, path: &Path, payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_tdd");
    let mut child = Command::new(bin)
        .arg("gate")
        .current_dir(cwd)
        .env("HOME", home)
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn stop_payload(cwd: &Path) -> String {
    format!(
        r#"{{"hook_event_name":"Stop","session_id":"probe-wiring","stop_hook_active":false,"cwd":{}}}"#,
        serde_json::to_string(&cwd.to_string_lossy()).unwrap()
    )
}

/// Apparatus: the empty PATH really makes git unspawnable *for the child*.
#[test]
fn empty_path_dir_has_no_git() {
    let root = unique_dir("apparatus");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    assert!(!empty_bin.join("git").exists(), "PATH dir must have no git");
    let probe = Command::new("/usr/bin/env")
        .arg("git")
        .arg("--version")
        .env("PATH", &empty_bin)
        .output();
    let unreachable = match probe {
        Ok(o) => !o.status.success(),
        Err(_) => true,
    };
    assert!(
        unreachable,
        "with PATH={} a child must not be able to find git; otherwise the block below could \
         pass for the wrong reason",
        empty_bin.display()
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// THE fail-open: git unreachable, inside a directory that HAS a `.git`.
/// The gate must block, and with the *git-scan* reason — not the panic
/// barrier's generic fail-closed block (which would mean the gate crashed, a
/// different defect) and not the missing-test reason.
#[test]
fn unspawnable_git_in_a_repo_blocks_the_stop() {
    let root = unique_dir("unspawnable");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(work.join(".git")).unwrap();
    // Something that WOULD be implementation-without-a-test if git could speak;
    // the point is that the gate must not need git's answer to refuse.
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("src").join("lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();

    let (code, stdout) = run_gate(&work, &home, &empty_bin, &stop_payload(&work));

    assert_eq!(code, 0, "a Stop hook must always exit 0; stdout: {stdout}");
    assert!(
        stdout.contains("\"decision\":\"block\"") || stdout.contains("\"decision\": \"block\""),
        "with git unreachable in a real repo the changed set is UNDETERMINED — the gate must \
         block, not allow. An allow here is the production fail-open (untested code ships \
         whenever a hook runs without git on PATH). stdout was: {stdout:?}"
    );
    assert!(
        stdout.contains("couldn't determine what changed"),
        "the block must come from the git-scan-undetermined path (tdd::gate::block_reason's \
         git_scan_failed branch), not from the panic barrier's generic fail-closed block — \
         otherwise this test would pass on a crashing gate. stdout was: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Over-block guard: with git unreachable but NO `.git` anywhere, the directory
/// is genuinely out of scope and the stop must still be allowed.
#[test]
fn unspawnable_git_without_a_dot_git_still_allows() {
    let root = unique_dir("unspawnable-bare");
    let empty_bin = root.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    if has_dot_git_ancestor(&work) {
        eprintln!("SKIPPED unspawnable_git_without_a_dot_git_still_allows: .git ancestor present");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    let (code, stdout) = run_gate(&work, &home, &empty_bin, &stop_payload(&work));

    assert_eq!(code, 0, "Stop hook must exit 0");
    assert!(
        !stdout.contains("\"decision\":\"block\""),
        "no git AND no .git anywhere: nothing suggests a repo, so the gate has no scope and \
         must keep allowing. Blocking here would trap every non-repo session. stdout: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Preserved behaviour with a REAL git: a genuine non-repo directory still
/// allows the stop.
#[test]
fn genuine_non_repo_with_real_git_still_allows() {
    let Some(path) = real_path() else {
        eprintln!("SKIPPED genuine_non_repo_with_real_git_still_allows: no PATH");
        return;
    };
    let root = unique_dir("real-nonrepo");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    if has_dot_git_ancestor(&work) {
        eprintln!("SKIPPED genuine_non_repo_with_real_git_still_allows: .git ancestor present");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    let (code, stdout) = run_gate(&work, &home, Path::new(&path), &stop_payload(&work));

    assert_eq!(code, 0, "Stop hook must exit 0");
    assert!(
        !stdout.contains("\"decision\":\"block\""),
        "a real git answering 'not a work tree' is authoritative — the allow path must survive \
         the fix. stdout: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

fn real_path() -> Option<std::ffi::OsString> {
    std::env::var_os("PATH")
}

/// Independent of the code under test: is there a `.git` at or above `dir`?
fn has_dot_git_ancestor(dir: &Path) -> bool {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if std::fs::symlink_metadata(d.join(".git")).is_ok() {
            return true;
        }
        cur = d.parent();
    }
    false
}
