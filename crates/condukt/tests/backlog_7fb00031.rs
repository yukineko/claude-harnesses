// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 7fb00031: when `worktree::merge` hits a conflict it records the
//! conflict to overwatch's review surface, but a failed
//! `append_merge_conflict` is only `eprintln!`ed "(continuing)" and the merge
//! still returns `Conflict(id)` — so the CLI prints "merge conflict recorded
//! (<id>)" and exits 0 although nothing was recorded. The conflict is invisible
//! in `overwatch review-queue`, and the stdout claim is false.
//!
//! Fault: the overwatch `merge_conflicts.jsonl` ledger exists and is READABLE
//! (so the pre-merge "is it held?" scan and the dedup scan both succeed) but is
//! read-only, so the append itself fails.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn fnv1a32(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Mirrors `harness_core::projkey::project_key`.
fn project_key(root: &Path) -> String {
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let full = canon.to_string_lossy();
    let base = canon
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".into());
    let sani: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{}-{:08x}", sani, fnv1a32(&full))
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "backlog 7fb00031: open defect, remove ignore when fixed"]
fn unrecorded_conflict_is_not_reported_as_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f.txt"), "base\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    run_git(&repo, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(repo.join("f.txt"), "theirs\n").unwrap();
    run_git(&repo, &["commit", "-q", "-am", "theirs"]);
    run_git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f.txt"), "ours\n").unwrap();
    run_git(&repo, &["commit", "-q", "-am", "ours"]);

    // A readable but read-only (empty) conflict ledger.
    let store = home
        .join(".overwatch")
        .join(project_key(&repo))
        .join("overwatch");
    std::fs::create_dir_all(&store).unwrap();
    let ledger = store.join("merge_conflicts.jsonl");
    std::fs::write(&ledger, "").unwrap();
    std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(0o444)).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args(["worktree", "merge", "--branch", "feat"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("CLAUDE_CODE_SESSION_ID", "sess-7fb00031")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("could not record merge conflict"),
        "fixture precondition: the conflict record must actually fail; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !(out.status.success() && stdout.contains("recorded")),
        "the conflict was NOT recorded to the review surface, yet the merge \
         exited 0 claiming it was: stdout={stdout:?} stderr={stderr:?}"
    );
}
