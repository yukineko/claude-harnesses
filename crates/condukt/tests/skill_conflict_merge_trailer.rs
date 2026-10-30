// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! SKILL.md's "merge pre-flight conflict" step 2 must produce a merge commit
//! that `condukt worktree merge` accepts (i.e. one carrying the task trailer).
//! The test executes the `git ...` spans that step prescribes, in order, in a
//! worktree whose branch conflicts with main, resolving the conflict after the
//! merge span; a step that prescribes no commit span falls back to a plain
//! `git commit --no-edit` (what the old prose implied).

use std::path::Path;
use std::process::{Command, Output};

fn sh(dir: &Path, script: &str, task: &str) -> Output {
    harness_core::git_env::scrub(&mut Command::new("sh"))
        .args(["-c", script])
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("TASK_ID", task)
        .output()
        .unwrap()
}

fn ok(dir: &Path, script: &str) -> String {
    let o = sh(dir, script, "");
    assert!(o.status.success(), "{script}: {o:?}");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn step2_git_spans() -> Vec<String> {
    let skill = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/condukt/SKILL.md"),
    )
    .unwrap();
    let start = skill
        .find("  2. 衝突が軽微で自動解消可能な場合")
        .expect("step 2");
    let rest = &skill[start..];
    let end = rest.find("  3. ").expect("step 3");
    rest[..end]
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|s| s.starts_with("git "))
        .map(str::to_string)
        .collect()
}

#[test]
fn skill_conflict_step_yields_a_mergeable_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    ok(
        &repo,
        "git init -q -b main && git config user.email t@t.t && git config user.name t",
    );
    std::fs::write(repo.join("f.txt"), "base\n").unwrap();
    ok(&repo, "git add . && git commit -q -m init");

    let condukt = |args: &[&str], dir: &Path| {
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
            .args(args)
            .current_dir(dir)
            .env("HOME", &home)
            .env("CLAUDE_CODE_SESSION_ID", "sess-skill-trailer")
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("CONDUKT_DISABLE")
            .output()
            .unwrap()
    };
    let out = condukt(
        &[
            "worktree", "create", "--run", "runA", "--topic", "t1", "--branch", "t1",
        ],
        &repo,
    );
    assert!(out.status.success(), "{out:?}");
    let wt = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let branch = ok(&wt, "git branch --show-current");
    let task = ok(
        &wt,
        &format!("git config --get branch.{branch}.conduktTask"),
    );

    // Conflicting edits: main moves on, the task commits (trailered) on the same line.
    std::fs::write(repo.join("f.txt"), "main\n").unwrap();
    ok(&repo, "git commit -q -am main-side");
    std::fs::write(wt.join("f.txt"), "task\n").unwrap();
    ok(
        &wt,
        &format!("git commit -q -am task-side --trailer 'Condukt-Task: {task}'"),
    );

    let spans = step2_git_spans();
    let mi = spans
        .iter()
        .position(|s| s.starts_with("git merge"))
        .expect("step 2 prescribes a git merge");
    let mut committed = false;
    for (i, s) in spans.iter().enumerate() {
        let _ = sh(&wt, s, &task); // the conflicting merge exits non-zero
        if i == mi {
            std::fs::write(wt.join("f.txt"), "resolved\n").unwrap();
            ok(&wt, "git add f.txt");
        }
        committed |= s.starts_with("git commit");
    }
    if !committed {
        ok(&wt, "git commit -q --no-edit");
    }

    let merged = condukt(&["worktree", "merge", "--branch", &branch], &repo);
    assert!(
        merged.status.success(),
        "merge refused after following SKILL step 2: {}",
        String::from_utf8_lossy(&merged.stderr)
    );
}
