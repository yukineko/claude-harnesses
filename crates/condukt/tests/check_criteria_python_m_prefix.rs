// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog a44c2fd2: `condukt state check-criteria` dropped the `python3 -m`
//! prefix of a prose pytest command (the prose fallback of
//! `verify::mechanical_cmd` returned bare `["pytest"]`), so a criteria the task
//! met was run as a different command than the one it named.
//!
//! Port of scripts/tests/condukt-check-criteria-python-m-prefix.sh, but it
//! drives the binary BUILT FROM THIS TREE (`CARGO_BIN_EXE_condukt`), not an
//! installed one, so it is RED before the fix and GREEN after.
//!
//! No skip path: if python3 / pytest / git is unusable the test FAILS (cannot
//! determine is not pass).

use std::path::Path;
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

/// Drop every inherited `GIT_*` variable (a git hook exports GIT_DIR etc.,
/// which would redirect fixture git calls at the outer repo; see 8dd8ac7e).
/// Local on purpose: this file must compile at revs older than harness_core::git_env.
fn scrub(cmd: &mut Command) -> &mut Command {
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    cmd
}

fn git(repo: &Path, args: &[&str]) {
    let out = scrub(&mut Command::new("git"))
        .current_dir(repo)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn condukt(repo: &Path, home: &Path, args: &[&str]) -> Output {
    scrub(&mut Command::new(bin()))
        .current_dir(repo)
        .env("HOME", home)
        .env("CONDUKT_AUTONOMOUS", "0")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn condukt")
}

#[test]
fn prose_python_m_pytest_keeps_prefix_and_target() {
    // Environment precondition: fail loudly, never skip.
    let py = Command::new("python3")
        .args(["-m", "pytest", "--version"])
        .output()
        .expect("python3 must be runnable (cannot determine otherwise)");
    assert!(
        py.status.success(),
        "python3 -m pytest unusable; cannot determine: {}",
        String::from_utf8_lossy(&py.stderr)
    );

    let tmp = std::env::temp_dir().join(format!("condukt-cc-pym-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let home = tmp.join("home");
    let repo = tmp.join("repo");
    std::fs::create_dir_all(home.join(".condukt/state")).unwrap();
    std::fs::create_dir_all(repo.join("scripts")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@t.t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(
        repo.join("scripts/x.py"),
        "def test_ok():\n    assert True\n",
    )
    .unwrap();
    git(&repo, &["add", "scripts/x.py"]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    let decomp = repo.join("decomp.json");
    std::fs::write(
        &decomp,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"add a test","touched_files":["scripts/x.py"],"deps":[],"class":"serial","done_criteria":"the new test exists and python3 -m pytest scripts/x.py passes"}]}"#,
    )
    .unwrap();

    let init = condukt(
        &repo,
        &home,
        &["state", "init", "--file", decomp.to_str().unwrap()],
    );
    let init_out = format!(
        "{}{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );
    let rid = init_out
        .lines()
        .map(str::trim)
        .rfind(|l| l.starts_with("run-"))
        .unwrap_or_else(|| panic!("state init produced no run id: {init_out}"))
        .to_string();

    let cc = condukt(
        &repo,
        &home,
        &["state", "check-criteria", "--run", &rid, "--task", "t1"],
    );
    let stdout = String::from_utf8_lossy(&cc.stdout).into_owned();
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("check-criteria stdout not JSON ({e}): {stdout}"));
    let cmd = v
        .get("cmd")
        .filter(|c| !c.is_null())
        .or_else(|| v.get("evidence").and_then(|e| e.get("cmd")))
        .unwrap_or_else(|| panic!("no extracted cmd in: {stdout}"));
    let argv: Vec<&str> = cmd
        .as_array()
        .unwrap_or_else(|| panic!("cmd is not an array: {cmd}"))
        .iter()
        .map(|a| a.as_str().expect("argv element is a string"))
        .collect();

    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        argv.starts_with(&["python3", "-m", "pytest"]),
        "extracted command must keep the `python3 -m pytest` prefix, got {argv:?}"
    );
    assert!(
        argv.contains(&"scripts/x.py"),
        "extracted command must keep the scripts/x.py target, got {argv:?}"
    );
}
