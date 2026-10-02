// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 0d6ac50b: after a task's recorded worktree is deleted (the normal
//! end of a merged run), `state set --status verified` is refused with
//! "failed to spawn tdd (No such file or directory ...) ... Install/provide
//! the `tdd` binary on PATH" — even though `tdd` IS on PATH. The ENOENT is
//! the spawn cwd (the gone worktree), so the refusal points the operator at
//! the wrong cause.
//!
//! Which closing behaviour is right (degrade, accept merge evidence, or an
//! explicit override) is an open decision on the ticket and is NOT pinned
//! here. What every option shares is that the refusal must not misattribute
//! the failure: it must name the gone worktree, not a missing `tdd`.
//!
//! Control: the same fake `tdd` with the worktree present promotes the task,
//! so the fake is a working checker and the only variable is the worktree.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

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

fn condukt(repo: &Path, home: &Path, bin: &Path, args: &[&str]) -> Output {
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(env!("CARGO_BIN_EXE_condukt"))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("PATH", path)
        .env("CLAUDE_CODE_SESSION_ID", "sess-0d6ac50b")
        .env("CONDUKT_DEFAULT_BRANCH", "main")
        .env_remove("CONDUKT_DISABLE")
        .output()
        .expect("spawn condukt")
}

fn ok(out: &Output, what: &str) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{what}: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "backlog 0d6ac50b: open defect, remove ignore when fixed"]
fn verify_refusal_after_worktree_removal_names_the_worktree_not_a_missing_tdd() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    let bin = tmp.path().join("bin");
    for d in [&repo, &home, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    // A working checker: valid F→P oracle, exit 0.
    let tdd = bin.join("tdd");
    std::fs::write(
        &tdd,
        "#!/bin/sh\necho '{\"has_green\":true,\"has_red\":true,\"transition\":\"fail_to_pass\",\"valid_fp_oracle\":true}'\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&tdd, std::fs::Permissions::from_mode(0o755)).unwrap();

    run_git(&repo, &["init", "-q", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "t@t.t"]);
    run_git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let dec = tmp.path().join("dec.json");
    std::fs::write(
        &dec,
        r#"{"goal":"g","tasks":[
            {"id":"live","title":"a","kind":"fix","reproduction_tests":"cargo test -p x","touched_files":["a.txt"],"deps":[],"class":"parallel","done_criteria":"d"},
            {"id":"gone","title":"b","kind":"fix","reproduction_tests":"cargo test -p x","touched_files":["b.txt"],"deps":[],"class":"parallel","done_criteria":"d"}]}"#,
    )
    .unwrap();
    ok(
        &condukt(
            &repo,
            &home,
            &bin,
            &[
                "state",
                "init",
                "--run",
                "runV",
                "--file",
                dec.to_str().unwrap(),
            ],
        ),
        "state init",
    );

    let wt_live = tmp.path().join("wt-live");
    let wt_gone = tmp.path().join("wt-gone");
    std::fs::create_dir_all(&wt_live).unwrap();
    std::fs::create_dir_all(&wt_gone).unwrap();
    for (task, wt) in [("live", &wt_live), ("gone", &wt_gone)] {
        ok(
            &condukt(
                &repo,
                &home,
                &bin,
                &[
                    "state",
                    "set",
                    "--run",
                    "runV",
                    "--task",
                    task,
                    "--status",
                    "done",
                    "--worktree",
                    wt.to_str().unwrap(),
                ],
            ),
            "state set done",
        );
    }

    // Control: worktree present -> the fake checker promotes the task.
    ok(
        &condukt(
            &repo,
            &home,
            &bin,
            &[
                "state", "set", "--run", "runV", "--task", "live", "--status", "verified",
            ],
        ),
        "control: verified with the worktree present",
    );

    // The run's worktree is removed after merge.
    std::fs::remove_dir_all(&wt_gone).unwrap();
    let out = condukt(
        &repo,
        &home,
        &bin,
        &[
            "state", "set", "--run", "runV", "--task", "gone", "--status", "verified",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        return; // closed: whichever option was chosen, the run is closable.
    }
    let wt_str = wt_gone.to_str().unwrap();
    assert!(
        stderr.contains(wt_str) && !stderr.contains("Install/provide the `tdd` binary on PATH"),
        "refusal misattributes the gone worktree ({wt_str}) to a missing tdd \
         (tdd IS on PATH at {}): {stderr}",
        tdd.display()
    );
}
