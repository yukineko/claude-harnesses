#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (2026-11-07): `condukt shadow-run finish` discards the shadow worktree
//! (shadow_run::finish -> worktree::discard) without first persisting the
//! worktree's tdd proofs, unlike `worktree discard` / reap / cleanup, which go
//! through proof_archive::discard. A shadow worktree holding `.tdd/*.red.json`
//! / `*.green.json` loses them at discard.
//!
//! Asserts only file existence and the exit code. The proofs are plain files
//! named like tdd's (proof_files matches on the `.red.json` / `.green.json`
//! suffix), so no `tdd` binary is needed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(dir: &Path, args: &[&str]) {
    let out = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn find_named(roots: &[&Path], name: &str, skip: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack: Vec<PathBuf> = roots.iter().map(|p| p.to_path_buf()).collect();
    while let Some(d) = stack.pop() {
        if d.starts_with(skip) {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if e.file_name() != ".git" {
                    stack.push(e.path());
                }
            } else if e.file_name() == name {
                found.push(e.path());
            }
        }
    }
    found
}

#[test]
#[ignore = "RED: shadow-run finish discards without persisting tdd proofs 2026-11-07"]
fn shadow_run_finish_keeps_a_durable_copy_of_the_tdd_proofs() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let repo = base.join("repo");
    let home = base.join("home");
    let wt_base = home.join(".condukt").join("wtbase");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&wt_base).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t.t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    std::fs::write(repo.join(".gitignore"), ".tdd/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "seed"]);

    let run = |args: &[&str]| -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_condukt"));
        harness_core::git_env::scrub(&mut cmd);
        cmd.args(args)
            .current_dir(&repo)
            .env("HOME", &home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-shadow-proofs")
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env("CONDUKT_WORKTREE_BASE", &wt_base)
            .env("CONDUKT_SHADOW_RUN_DIR", base.join("shadow-flag"))
            .env("HARNESS_PROGRESS_WINDOW_SECS", "0")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .unwrap()
    };

    assert!(run(&["shadow-run", "enable"]).status.success());
    let ex = run(&[
        "shadow-run",
        "exec",
        "--run",
        "run-shadow",
        "--topic",
        "t5",
        "--branch",
        "b-t5",
        "--model",
        "m",
    ]);
    assert!(ex.status.success(), "exec: {ex:?}");
    let wt = PathBuf::from(String::from_utf8_lossy(&ex.stdout).trim());
    assert!(wt.is_dir(), "apparatus: shadow worktree {wt:?} must exist");
    std::fs::create_dir_all(wt.join(".tdd")).unwrap();
    std::fs::write(wt.join(".tdd/t5.red.json"), "{\"red\":true}").unwrap();
    std::fs::write(wt.join(".tdd/t5.green.json"), "{\"green\":true}").unwrap();

    let fin = run(&[
        "shadow-run",
        "finish",
        "--path",
        wt.to_str().unwrap(),
        "--branch",
        "b-t5",
        "--run",
        "run-shadow",
        "--title",
        "x",
        "--model",
        "m",
        "--pass",
    ]);
    assert!(fin.status.success(), "finish: {fin:?}");
    assert!(
        !wt.exists(),
        "apparatus: finish must have discarded the worktree"
    );

    let red = find_named(&[&home, &repo], "t5.red.json", &wt);
    let green = find_named(&[&home, &repo], "t5.green.json", &wt);
    assert!(
        !red.is_empty() && !green.is_empty(),
        "no durable copy of the tdd proofs survives shadow-run finish \
         (red copies: {red:?}, green copies: {green:?})"
    );
}
