// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `condukt loop --max-iters` overrides `[loop] max_iters`, and a cap that
//! cannot be determined (config.toml unparseable, no flag) refuses the
//! iteration instead of running it uncapped. Fixture style follows
//! drift_b1_loop_cap.rs: an always-failing test command so "all tests pass"
//! can never be the reason to stop.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn fixture(tag: &str, config: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("condukt-loopcap-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(home.join(".condukt")).unwrap();
    assert!(harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    std::fs::write(home.join(".condukt/config.toml"), config).unwrap();
    (repo, home)
}

fn run(repo: &Path, home: &Path, extra: &[&str]) -> (Output, serde_json::Value) {
    let o = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(["loop", "--module", "client", "--prev-failures", "9"])
        .args(extra)
        .current_dir(repo)
        .env("HOME", home)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let j: serde_json::Value = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("loop output not json: {e}: {o:?}"));
    (o, j)
}

const FAILING: &str =
    "[test]\ncommand = \"echo FAILED 7 && exit 1\"\n\n[loop]\nbuild_command = \"true\"\n";

#[test]
fn max_iters_flag_lowers_the_configured_cap() {
    let (repo, home) = fixture("lower", &format!("{FAILING}max_iters = 10\n"));
    let (_, ok) = run(&repo, &home, &["--iteration", "1", "--max-iters", "1"]);
    assert_eq!(ok["stop"], false, "iteration 1 within cap 1 must run: {ok}");

    let (o, j) = run(&repo, &home, &["--iteration", "2", "--max-iters", "1"]);
    assert_eq!(j["stop"], true, "{j}");
    assert!(
        j["stop_reason"].as_str().unwrap().contains("max_iters 1"),
        "{j}"
    );
    assert!(j["failure_count"].is_null(), "refused cycle ran: {j}");
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn max_iters_flag_raises_the_configured_cap() {
    let (repo, home) = fixture("raise", &format!("{FAILING}max_iters = 2\n"));
    let (o, j) = run(&repo, &home, &["--iteration", "3", "--max-iters", "5"]);
    assert_eq!(j["stop"], false, "iteration 3 within --max-iters 5: {j}");
    assert!(j["failure_count"].is_u64(), "cycle did not run: {j}");
    assert!(o.status.success());
}

#[test]
fn unparseable_config_refuses_instead_of_running_uncapped() {
    let (repo, home) = fixture("broken", "[loop\nmax_iters = 2\n");
    let (o, j) = run(&repo, &home, &["--iteration", "1"]);
    assert_eq!(j["stop"], true, "{j}");
    assert!(
        j["stop_reason"]
            .as_str()
            .unwrap()
            .starts_with("max iters undetermined"),
        "{j}"
    );
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn loop_without_module_is_rejected() {
    let (repo, home) = fixture("nomodule", FAILING);
    let o = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
        .args(["loop", "--iteration", "1"])
        .current_dir(&repo)
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!o.status.success(), "{o:?}");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("--module"),
        "{o:?}"
    );
}
