// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! GH 1518 extension: the no-backtick prose fallback at the end of
//! `verify::mechanical_cmd` (`lower.contains("cargo test")` etc.) never
//! tokenizes, so a criteria whose written runner command has an unterminated
//! quote still yields a (different, broader) command such as `cargo test -p demo`.
//! User ruling 2026-11-03: an unterminated quote yields NO mechanical command.
//!
//! Property: `condukt state check-criteria` extracts no command (`cmd` null/absent,
//! `skip_verifier` not true) for such prose. CONTROLS (balanced quotes, plain
//! prose) must still yield a command so "return None for everything" cannot pass.

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

/// Run `check-criteria` on a single-task run with the given done_criteria and
/// return the parsed stdout JSON.
fn check(tag: &str, criteria: &str) -> serde_json::Value {
    let tmp = std::env::temp_dir().join(format!(
        "condukt-cc-prose-unterm-{tag}-{}",
        std::process::id()
    ));
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

    let decomp = repo.join("decomp.json");
    let d = serde_json::json!({"goal":"g","tasks":[{"id":"t1","title":"t","touched_files":["x.txt"],"deps":[],"class":"serial","done_criteria":criteria}]});
    std::fs::write(&decomp, d.to_string()).unwrap();
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
    let _ = std::fs::remove_dir_all(&tmp);
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("check-criteria stdout not JSON ({e}): {stdout}"))
}

fn extracted_cmd(v: &serde_json::Value) -> Option<&serde_json::Value> {
    v.get("cmd")
        .filter(|c| !c.is_null())
        .or_else(|| v.get("evidence").and_then(|e| e.get("cmd")))
        .filter(|c| !c.is_null())
}

#[test]
fn unterminated_quote_in_prose_criteria_is_not_extracted_as_a_command() {
    let cases = [
        ("dq", "cargo test -p demo -- \"x exits 0"),
        ("sq", "cargo test -p demo -- 'x exits 0"),
        ("pytest", "pytest tests/a.py -k \"foo passes"),
    ];
    let mut failures = Vec::new();
    for (tag, criteria) in cases {
        let v = check(tag, criteria);
        let cmd = extracted_cmd(&v);
        if cmd.is_some() || v.get("skip_verifier").and_then(|s| s.as_bool()) == Some(true) {
            failures.push(format!(
                "{tag}: {criteria:?} extracted a mechanical command {cmd:?} instead of being undetermined: {v}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// CONTROLS: must stay green. Balanced-quote and plain prose still yield a command.
#[test]
fn balanced_or_plain_prose_criteria_still_yield_a_command() {
    let cases = [
        ("bal", "cargo test -p demo -- \"x\" exits 0"),
        ("plain", "cargo test -p demo passes"),
    ];
    let mut failures = Vec::new();
    for (tag, criteria) in cases {
        let v = check(tag, criteria);
        if extracted_cmd(&v).is_none() {
            failures.push(format!("{tag}: {criteria:?} yielded no command: {v}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
