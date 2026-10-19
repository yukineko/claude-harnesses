// Verifier-written (independent) tests for the abort_trial_merge MERGE_HEAD
// branches. Fault injection: a PATH `git` shim that fails `merge --abort`
// (and optionally the MERGE_HEAD probe) and delegates everything else.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn real_git() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap();
    std::env::split_paths(&path)
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH")
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Real content conflict on f.txt, writable (absent) ledger, shim installed.
fn run_with_shim(fail_probe: bool) -> (std::process::Output, PathBuf, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    let bin = tmp.path().join("shimbin");
    for d in [&repo, &home, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f.txt"), "base\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    run_git(&repo, &["checkout", "-q", "-b", "feat"]);
    run_git(&repo, &["config", "branch.feat.conduktBase", "main"]);
    run_git(&repo, &["config", "branch.feat.conduktTask", "r/t"]);
    std::fs::write(repo.join("f.txt"), "theirs\n").unwrap();
    run_git(
        &repo,
        &[
            "commit",
            "-q",
            "-am",
            "theirs",
            "--trailer",
            "Condukt-Task: r/t",
        ],
    );
    run_git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("f.txt"), "ours\n").unwrap();
    run_git(&repo, &["commit", "-q", "-am", "ours"]);

    let probe_arm = if fail_probe {
        "  rev-parse\\ -q\\ --verify\\ MERGE_HEAD*) echo 'shim: probe failure' >&2; exit 128;;\n"
    } else {
        ""
    };
    let shim = format!(
        "#!/bin/bash\ncase \"$*\" in\n  merge\\ --abort*) echo 'shim: abort failure' >&2; exit 128;;\n{probe_arm}esac\nexec '{}' \"$@\"\n",
        real_git().display()
    );
    let shim_path = bin.join("git");
    std::fs::write(&shim_path, shim).unwrap();
    std::fs::set_permissions(&shim_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());

    let out = Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args(["worktree", "merge", "--branch", "feat"])
        .current_dir(&repo)
        .env("PATH", path)
        .env("HOME", &home)
        .env("CLAUDE_CODE_SESSION_ID", "sess-verifier-abort")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .unwrap();
    (out, repo, tmp)
}

fn assert_fail_closed(out: &std::process::Output, repo: &Path) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "must exit nonzero: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stdout.contains("merge conflict recorded"),
        "stdout={stdout:?}"
    );
    assert!(stderr.contains("could NOT be aborted"), "stderr={stderr:?}");
    assert!(
        repo.join(".git/MERGE_HEAD").exists(),
        "fixture: merge must really be left in progress"
    );
}

#[test]
fn failed_abort_with_merge_head_present_fails_closed() {
    let (out, repo, _t) = run_with_shim(false);
    assert_fail_closed(&out, &repo);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("MERGE_HEAD is still present"),
        "stderr={stderr:?}"
    );
}

#[test]
fn failed_abort_with_failing_merge_head_probe_fails_closed() {
    let (out, repo, _t) = run_with_shim(true);
    assert_fail_closed(&out, &repo);
}
