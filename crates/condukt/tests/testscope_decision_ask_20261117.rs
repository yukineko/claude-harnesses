#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! R7 of spec testscope-confirmed-run (backlog 64fe4e19): Decision has a third
//! value, every consumer matches it exhaustively, and the module doc says so.
//!
//! Written by an independent test author, not the implementer.
//!
//! DOES NOT PROVE: that no other consumer of Decision exists (only the three
//! source files the spec names are scanned); R7(b) "state test bails on an Ask
//! prior" is a control only, because at the base revision `state test` already
//! bails on any prior, so Ask vs Deny is not observable through it.

mod testscope_confirm_common;
use testscope_confirm_common::*;

const STATE: &str = include_str!("../src/state.rs");
const MAIN: &str = include_str!("../src/main.rs");
const TESTSCOPE: &str = include_str!("../src/testscope.rs");

fn if_let_deny_lines(name: &str, src: &str) -> Vec<String> {
    src.lines()
        .enumerate()
        .filter(|(_, l)| l.contains("if let") && l.contains("Decision::Deny"))
        .map(|(i, l)| format!("{name}:{}: {}", i + 1, l.trim()))
        .collect()
}

#[test]
fn no_if_let_deny_consumer_remains() {
    let mut found = if_let_deny_lines("state.rs", STATE);
    found.extend(if_let_deny_lines("main.rs", MAIN));
    found.extend(if_let_deny_lines("testscope.rs", TESTSCOPE));
    assert!(found.is_empty(), "non-exhaustive consumers: {found:#?}");
}

fn module_doc() -> String {
    TESTSCOPE
        .lines()
        .take_while(|l| l.starts_with("//!"))
        .map(|l| l.trim_start_matches("//!").trim())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn module_doc_no_longer_says_cannot_determine_resolves_to_deny() {
    let doc = module_doc();
    assert!(!doc.is_empty());
    assert!(
        !doc.contains("an unwritable ledger) resolves to deny"),
        "old wording remains in the module doc"
    );
    assert!(
        doc.split(|c: char| !c.is_alphanumeric())
            .any(|w| w.eq_ignore_ascii_case("ask")),
        "module doc never mentions Ask"
    );
}

/// Control (passes at HEAD, see header): `state test` with only a pending prior
/// does not run the test command and exits non-zero with a stderr reason.
#[test]
fn control_state_test_with_pending_prior_bails_without_running() {
    use std::process::Command;
    let env = Env::new();
    let root = repo();
    let cfg = env.home.path().join(".condukt");
    std::fs::create_dir_all(&cfg).unwrap();
    let cmd = "cargo test -p x --test t";
    std::fs::write(
        cfg.join("config.toml"),
        format!("[test]\ncommand = \"{cmd}\"\n"),
    )
    .unwrap();
    let dec = serde_json::json!({"goal": "g", "tasks": [{
        "id": "t1", "title": "t1", "touched_files": ["a.txt"], "deps": [],
        "class": "parallel", "done_criteria": "d", "reproduction_tests": cmd}]});
    let p = env.home.path().join("runT.dec.json");
    std::fs::write(&p, dec.to_string()).unwrap();
    let bin = tempfile::tempdir().unwrap();
    let counter = bin.path().join("count.txt");
    let cargo = bin.path().join("cargo");
    std::fs::write(
        &cargo,
        format!("#!/bin/sh\necho x >> '{}'\nexit 0\n", counter.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_condukt"));
        harness_core::git_env::scrub(&mut c)
            .args(args)
            .current_dir(root.path())
            .env("HOME", env.home.path())
            .env("CONDUKT_TESTSCOPE_LEDGER", env.ledger())
            .env("CONDUKT_DEFAULT_BRANCH", "main")
            .env("CLAUDE_CODE_SESSION_ID", "s")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .output()
            .unwrap()
    };
    let init = run(&[
        "state",
        "init",
        "--run",
        "runT",
        "--file",
        p.to_str().unwrap(),
    ]);
    assert!(init.status.success(), "{init:?}");
    assert!(pre(&env, "1", cmd, root.path()).is_allow());
    let out = run(&["state", "test", "--run", "runT"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(!out.stderr.is_empty(), "no reason on stderr");
    assert!(!counter.exists(), "test command ran despite a prior");
}
