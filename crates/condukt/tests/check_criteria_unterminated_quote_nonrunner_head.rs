// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! GH 1518 follow-up, found 2026-11-04 by the independent verifier (opus) of
//! condukt run run-20261103-233558-54331 task gh1518-unterminated-quote at
//! worktree commit 53a45c7f. Written by the orchestrator, not by the
//! implementer.
//!
//! User ruling 2026-11-03: an unterminated quote yields NO mechanical command
//! (undetermined, fail-closed). 53a45c7f honours that only when the
//! unparseable backtick span's first token is a criteria runner. When the span
//! starts with something else (`FOO=1 cargo test ...`, `env cargo test ...`,
//! `cd sub && cargo test ...`) the span is skipped and the prose fallback
//! (`lower.contains("cargo test")`) extracts `cargo test -p demo` — a
//! different command from the one written — which can then skip the verifier.
//!
//! Property pinned: a done_criteria with an unparseable backtick span that
//! names a runner anywhere in it yields no mechanical command, and
//! `skip_verifier` is never true.

use std::path::Path;
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

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
    assert!(out.status.success(), "git {args:?} failed");
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

/// Run `state check-criteria` on a one-task decomposition whose done_criteria
/// is `criteria`; return the parsed stdout JSON.
fn check_criteria(tag: &str, criteria: &str) -> serde_json::Value {
    let tmp =
        std::env::temp_dir().join(format!("condukt-cc-unterm-nr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let home = tmp.join("home");
    let repo = tmp.join("repo");
    std::fs::create_dir_all(home.join(".condukt/state")).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@t.t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("x.txt"), "x\n").unwrap();
    git(&repo, &["add", "x.txt"]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    let decomp = serde_json::json!({
        "goal": "g",
        "tasks": [{
            "id": "t1", "title": "t", "touched_files": ["x.txt"], "deps": [],
            "class": "serial", "done_criteria": criteria
        }]
    });
    let decomp_path = repo.join("decomp.json");
    std::fs::write(&decomp_path, decomp.to_string()).unwrap();
    let init = condukt(
        &repo,
        &home,
        &["state", "init", "--file", decomp_path.to_str().unwrap()],
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
    let _ = std::fs::remove_dir_all(&tmp);
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("check-criteria stdout not JSON ({e}): {stdout}"))
}

#[test]
fn unparseable_span_with_non_runner_head_yields_no_mechanical_command() {
    let cases = [
        (
            "envassign",
            "`FOO=1 cargo test -p demo -- \"never_closed` exits 0",
        ),
        (
            "envcmd",
            "`env cargo test -p demo -- \"never_closed` exits 0",
        ),
        (
            "cdchain",
            "`cd sub && cargo test -p demo -- \"never_closed` exits 0",
        ),
    ];
    let mut failures = Vec::new();
    for (tag, criteria) in cases {
        let v = check_criteria(tag, criteria);
        let cmd = v
            .get("cmd")
            .filter(|c| !c.is_null())
            .or_else(|| v.get("evidence").and_then(|e| e.get("cmd")))
            .filter(|c| !c.is_null());
        let skip = v.get("skip_verifier").and_then(serde_json::Value::as_bool) == Some(true);
        if cmd.is_some() || skip {
            failures.push(format!(
                "{tag}: criteria {criteria:?} produced cmd={cmd:?} skip_verifier={skip}: {v}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "an unterminated-quote span still yielded a mechanical command (user ruling 2026-11-03: it must yield none):\n{}",
        failures.join("\n")
    );
}
