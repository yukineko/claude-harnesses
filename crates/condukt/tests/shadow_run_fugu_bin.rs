// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `condukt shadow-run finish` must spawn the fugu-router resolved by
//! `harness_core::plugin_bin::resolve` (plugin cache
//! `~/.claude/plugins/cache/yukineko/fugu-router/<version>/bin/` first, PATH
//! second), not a bare `fugu-router` from PATH. HOME is isolated and both
//! fugu-routers are stubs, so the user's real `~/.fugu-router` is never
//! touched.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_condukt");

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed");
}

/// Write an executable stub `fugu-router` at `path` that appends its argv to `log`.
fn write_stub(path: &Path, log: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let script = format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n", log.display());
    std::fs::write(path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn finish_spawns_the_resolved_plugin_binary_not_the_path_one() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let flag = tmp.path().join("flag");
    let repo = tmp.path().join("repo");
    let wt_base = tmp.path().join("wt-base");
    let plugin_dir = home.join(".claude/plugins/cache/yukineko/fugu-router/0.1.19");
    let path_dir = tmp.path().join("pathbin");
    let log_a = tmp.path().join("log-a.txt"); // plugin-resolved stub
    let log_b = tmp.path().join("log-b.txt"); // PATH stub
    for d in [&home, &flag, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    write_stub(&plugin_dir.join("bin/fugu-router"), &log_a);
    write_stub(&path_dir.join("fugu-router"), &log_b);

    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "T"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);

    let sys_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![path_dir.clone()];
    paths.extend(std::env::split_paths(&sys_path));
    let path_env = std::env::join_paths(paths).unwrap();

    let run = |args: &[&str]| {
        Command::new(BIN)
            .args(args)
            .current_dir(&repo)
            .env("HOME", &home)
            .env("PATH", &path_env)
            .env("CONDUKT_SHADOW_RUN_DIR", &flag)
            .env("CONDUKT_WORKTREE_BASE", &wt_base)
            .output()
            .unwrap()
    };

    assert!(run(&["shadow-run", "enable"]).status.success());
    let exec = run(&[
        "shadow-run",
        "exec",
        "--topic",
        "t9-shadow",
        "--branch",
        "shadow/t9-haiku",
        "--model",
        "haiku",
    ]);
    assert!(
        exec.status.success(),
        "exec failed: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    let wt = String::from_utf8_lossy(&exec.stdout).trim().to_string();

    let fin = run(&[
        "shadow-run",
        "finish",
        "--path",
        &wt,
        "--branch",
        "shadow/t9-haiku",
        "--title",
        "t9 shadow attempt",
        "--model",
        "haiku",
        "--pass",
        "--cost",
        "0.05",
        "--duration",
        "3.2",
    ]);
    assert!(
        fin.status.success(),
        "finish failed: {}",
        String::from_utf8_lossy(&fin.stderr)
    );

    let a = std::fs::read_to_string(&log_a).unwrap_or_default();
    let b = std::fs::read_to_string(&log_b).unwrap_or_default();
    assert!(
        a.lines()
            .any(|l| l.starts_with("record") && l.contains("--class shadow-run")),
        "plugin-resolved fugu-router must receive `record --class shadow-run`; log A={a:?} log B={b:?}"
    );
    assert!(
        b.trim().is_empty(),
        "bare PATH fugu-router must NOT be spawned by shadow-run finish; log B={b:?}"
    );
}
