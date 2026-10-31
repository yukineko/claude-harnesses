// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: propguard's production git spawns (`git::run_git_bin`, reached via
//! `changed_files` / `probe_repo` from `propguard check`) honour an inherited
//! `GIT_DIR`. A Stop hook run from a git hook (or a shell exporting GIT_DIR)
//! then judges the WRONG repo.
//!
//! Setup: repo A is clean and is the hook's cwd. Repo B has an unrelated dirty
//! file. `GIT_DIR=<B>/.git` is exported to the child only (no process-global
//! env mutation). The verdict for cwd A must equal the verdict of the same call
//! with the git env scrubbed (control): A is clean, so the gate allows.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    harness_core::git_env::scrub(&mut cmd);
    let st = cmd
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("git spawns");
    assert!(
        st.status.success(),
        "fixture git {args:?} failed: {}",
        String::from_utf8_lossy(&st.stderr)
    );
}

fn make_repo(root: &Path, name: &str, file: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    std::fs::write(dir.join(file), "fn f() {}\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

fn run_check(cwd: &Path, home: &Path, git_dir: Option<&Path>) -> (i32, String) {
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": match git_dir { Some(_) => "gitdir-1", None => "gitdir-0" },
        "stop_hook_active": false,
        "cwd": cwd.to_string_lossy(),
    })
    .to_string();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_propguard"));
    harness_core::git_env::scrub(&mut cmd);
    cmd.arg("check")
        .current_dir(cwd)
        .env("HOME", home)
        .env("PROPGUARD_STATE_DIR", home.join(".propguard-state"))
        .env(
            "PROPGUARD_CRITERIA",
            "idempotent; never panic; stable output schema",
        )
        .env_remove("PROPGUARD_DISABLE");
    if let Some(g) = git_dir {
        // Re-applied AFTER scrub so only this child sees it.
        cmd.env("GIT_DIR", g);
    }
    let mut child = cmd
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

#[test]
fn inherited_git_dir_must_not_make_a_clean_repo_judged_as_another_repos_changes() {
    let root = std::env::temp_dir().join(format!(
        "propguard-inherited-gitdir-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let a = make_repo(&root, "repo-a", "a.rs");
    let b = make_repo(&root, "repo-b", "b.rs");
    // B is dirty; A stays clean.
    std::fs::write(b.join("b.rs"), "fn f() { panic!() }\n").unwrap();
    std::fs::write(b.join("new_b.rs"), "fn g() {}\n").unwrap();

    let (ctl_code, ctl_out) = run_check(&a, &home, None);
    let (env_code, env_out) = run_check(&a, &home, Some(&b.join(".git")));
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(ctl_code, 0, "control: hook exits 0; stdout: {ctl_out:?}");
    assert!(
        !ctl_out.contains("\"decision\":\"block\"") && !ctl_out.contains("\"decision\": \"block\""),
        "control (scrubbed, clean repo A) must allow; stdout: {ctl_out:?}"
    );
    assert_eq!(env_code, 0, "hook exits 0; stdout: {env_out:?}");
    assert!(
        !env_out.contains("\"decision\":\"block\"") && !env_out.contains("\"decision\": \"block\""),
        "with GIT_DIR pointing at unrelated dirty repo B, the verdict for clean repo A must \
         equal the scrubbed control (allow). Production git followed the inherited GIT_DIR \
         and judged B's changes as A's. control stdout: {ctl_out:?}; GIT_DIR stdout: {env_out:?}"
    );
}

fn is_block(stdout: &str) -> bool {
    stdout.contains("\"decision\":\"block\"") || stdout.contains("\"decision\": \"block\"")
}

/// Kill test for the repo probe in `git::scan_changed`: it must be the
/// scrubbed probe. The cwd is a plain directory that is NOT a git repo, while
/// an inherited `GIT_DIR` names an unrelated dirty repo B. The scrubbed probe
/// answers "not a repo" from the filesystem, so the gate allows (no git scope)
/// exactly like the control without `GIT_DIR`. An unscrubbed probe would let
/// the inherited `GIT_DIR` make the non-repo cwd look like a repo, and the
/// (scrubbed) change scan inside it would then fail — a block for a directory
/// that has no repository at all.
#[test]
fn inherited_git_dir_must_not_make_a_non_repo_cwd_judged_as_a_repo() {
    let root = std::env::temp_dir().join(format!(
        "propguard-inherited-gitdir-nonrepo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let home_ctl = root.join("home-ctl");
    let home_env = root.join("home-env");
    std::fs::create_dir_all(&home_ctl).unwrap();
    std::fs::create_dir_all(&home_env).unwrap();

    let b = make_repo(&root, "repo-b", "b.rs");
    std::fs::write(b.join("b.rs"), "fn f() { panic!() }\n").unwrap();
    std::fs::write(b.join("new_b.rs"), "fn g() {}\n").unwrap();

    // Plain non-repo directory with a source file in it.
    let c = root.join("not-a-repo");
    std::fs::create_dir_all(&c).unwrap();
    std::fs::write(c.join("c.rs"), "fn h() {}\n").unwrap();

    let (ctl_code, ctl_out) = run_check(&c, &home_ctl, None);
    let (env_code, env_out) = run_check(&c, &home_env, Some(&b.join(".git")));
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(ctl_code, 0, "control: hook exits 0; stdout: {ctl_out:?}");
    assert!(
        !is_block(&ctl_out),
        "control (no GIT_DIR, non-repo cwd) must allow; stdout: {ctl_out:?}"
    );
    assert_eq!(env_code, 0, "hook exits 0; stdout: {env_out:?}");
    assert!(
        !is_block(&env_out),
        "with GIT_DIR pointing at unrelated repo B, the verdict for non-repo cwd C must equal \
         the control (allow). control stdout: {ctl_out:?}; GIT_DIR stdout: {env_out:?}"
    );
}
