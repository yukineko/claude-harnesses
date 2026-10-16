// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Drift audit B1: `[loop] max_iters` is parsed into `Config.loop_max_iters`
//! but nothing enforces it, so the iteration cap exists only as skill prose.
//! With max_iters = 2, iteration 3 of a still-failing cycle must be refused.

use std::process::{Command, Stdio};

#[test]
fn loop_refuses_iteration_beyond_configured_max_iters() {
    let base = std::env::temp_dir().join(format!("condukt-b1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(home.join(".condukt")).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    // always-failing test so "all tests pass" can't be the reason to stop
    std::fs::write(
        home.join(".condukt/config.toml"),
        "[test]\ncommand = \"echo FAILED 7 && exit 1\"\n\n[loop]\nbuild_command = \"true\"\nmax_iters = 2\n",
    )
    .unwrap();
    let run = |iter: &str, prev: &str| {
        Command::new(env!("CARGO_BIN_EXE_condukt"))
            .args([
                "loop",
                "--module",
                "client",
                "--iteration",
                iter,
                "--prev-failures",
                prev,
            ])
            .current_dir(&repo)
            .env("HOME", &home)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    // control: iteration 1 is allowed to proceed (no stop, exit 0)
    let ok = run("1", "9");
    let okj: serde_json::Value = serde_json::from_slice(&ok.stdout)
        .unwrap_or_else(|e| panic!("control output not json: {e}: {ok:?}"));
    assert_eq!(okj["stop"], false, "control: iteration 1 must run: {okj}");

    let o = run("3", "9");
    let refused = !o.status.success()
        || serde_json::from_slice::<serde_json::Value>(&o.stdout)
            .map(|j| j["stop"] == true)
            .unwrap_or(false);
    assert!(
        refused,
        "iteration 3 > max_iters=2 was NOT refused (exit {:?}); stdout: {}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout)
    );
}
