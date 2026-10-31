#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `reversible::probe` follows an inherited `GIT_DIR`.
//!
//! The target file lives in a directory that is NOT inside any repo and holds
//! the only copy of its bytes. Truncating it is unrecoverable, so the verdict
//! must be restrictive. A separate repo B (unrelated, tracking its own
//! `f.txt` with the same content) exists elsewhere. When the hook is launched
//! with `GIT_DIR=<B>/.git` the production git probes (`probe_repo` and
//! `git status`) must still judge the target's own directory, so the verdict
//! must equal the scrubbed control run.

#[path = "support/git_env.rs"]
mod git_env;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) {
    let out = git_env::pinned(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn run(base: &Path, proj: &Path, command: &str, git_dir: Option<&Path>) -> String {
    let envs: Vec<(&str, &Path)> = git_dir.map(|g| ("GIT_DIR", g)).into_iter().collect();
    run_with_env(base, proj, command, &envs)
}

fn run_with_env(base: &Path, proj: &Path, command: &str, envs: &[(&str, &Path)]) -> String {
    let payload = serde_json::json!({
        "session_id": "reversible-probe-gitdir",
        "cwd": proj.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    git_env::scrub(&mut cmd);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CLAUDE_PROJECT_DIR", proj)
        .env("BLASTGUARD_APPROVALS_DIR", base.join("store"))
        .env("HOME", base.join("home"))
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(base);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn truncating_non_repo_file_verdict_ignores_inherited_git_dir() {
    // Physical location must be OUTSIDE every git repo (neutral_base lives under
    // the workspace `target/`, i.e. inside the outer repo), so the control run
    // really is a non-repo file.
    let base: PathBuf = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join("bg-reversible-probe-gitdir")
        .join(format!("{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let proj = base.join("src/proj");
    let victim_dir = proj.join("plain");
    let repo_b = base.join("repo-b");
    for d in [&proj, &victim_dir, &repo_b, &base.join("home")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(victim_dir.join("f.txt"), "x\n").unwrap();
    std::fs::write(repo_b.join("f.txt"), "x\n").unwrap();
    git(&repo_b, &["init", "-q"]);
    git(&repo_b, &["add", "f.txt"]);
    git(&repo_b, &["commit", "-q", "-m", "b"]);

    let command = format!(": > {}/f.txt", victim_dir.display());
    let control = run(&base, &proj, &command, None);
    let with_git_dir = run(&base, &proj, &command, Some(&repo_b.join(".git")));
    let _ = std::fs::remove_dir_all(&base);

    assert!(
        !control.trim().is_empty(),
        "control (GIT_* scrubbed) must be restrictive for an unrecoverable file; got Allow"
    );
    assert_eq!(
        control, with_git_dir,
        "verdict changed when GIT_DIR pointed at an unrelated repo: the probe judged the wrong repo.\n\
         control    = {control}\nwith GIT_DIR = {with_git_dir}"
    );
}

/// Site B in isolation: the victim's parent IS a real repo A, so the repo probe
/// (site A, scrubbed) finds A with or without the inherited variables and the
/// control verdict is whatever policy says for a file inside a work tree.
/// What is left to judge is the `git status` call. A hook-style
/// `GIT_DIR=<B>/.git` + `GIT_WORK_TREE=<B>` leaking into it makes git refuse
/// the path ("outside repository", exit 128), which resolves restrictively, so
/// the verdict differs from the control run unless `git status` is scrubbed.
#[test]
fn verdict_inside_repo_ignores_inherited_git_dir_for_status() {
    let base: PathBuf = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join("bg-reversible-probe-gitdir-status")
        .join(format!("{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let proj = base.join("src/proj");
    let repo_a = proj.join("repo-a");
    let repo_b = base.join("repo-b");
    for d in [&repo_a, &repo_b, &base.join("home")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(repo_a.join("f.txt"), "x\n").unwrap();
    std::fs::write(repo_b.join("f.txt"), "x\n").unwrap();
    git(&repo_a, &["init", "-q"]);
    git(&repo_b, &["init", "-q"]);
    git(&repo_b, &["add", "f.txt"]);
    git(&repo_b, &["commit", "-q", "-m", "b"]);

    let command = format!(": > {}/f.txt", repo_a.display());
    let control = run(&base, &proj, &command, None);
    let with_git_dir = run_with_env(
        &base,
        &proj,
        &command,
        &[
            ("GIT_DIR", &repo_b.join(".git")),
            ("GIT_WORK_TREE", &repo_b),
        ],
    );
    let _ = std::fs::remove_dir_all(&base);

    assert_eq!(
        control, with_git_dir,
        "git status judged the wrong repo when GIT_DIR/GIT_WORK_TREE pointed at an unrelated repo.\n\
         control    = {control}\nwith GIT_DIR = {with_git_dir}"
    );
}
