// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! F-H (file-script-findings, 2026-11-03): `verify::parse_argv` is
//! `shlex::split(s).unwrap_or_else(|| s.split_whitespace()...)`. A done_criteria
//! backtick span with an UNTERMINATED quote is therefore not "undetermined":
//! it becomes a different argv (the stray `"` stays glued to a token) that
//! `condukt state check-criteria` extracts and, when it exits 0, accepts as the
//! mechanical evidence that skips the verifier. CLAUDE.md 3: unparseable is
//! undetermined, never a best-effort guess.
//!
//! Expected: no mechanical command is extracted from an unparseable span
//! (`cmd` is null/absent and `skip_verifier` is not true).

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

#[test]
fn unterminated_quote_in_criteria_command_is_not_extracted_as_a_command() {
    let tmp = std::env::temp_dir().join(format!("condukt-cc-unterm-{}", std::process::id()));
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
    std::fs::write(
        &decomp,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"t","touched_files":["x.txt"],"deps":[],"class":"serial","done_criteria":"`cargo test -p demo -- \"never_closed` passes"}]}"#,
    )
    .unwrap();
    let init = condukt(&repo, &home, &["state", "init", "--file", decomp.to_str().unwrap()]);
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

    let cc = condukt(&repo, &home, &["state", "check-criteria", "--run", &rid, "--task", "t1"]);
    let stdout = String::from_utf8_lossy(&cc.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&tmp);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("check-criteria stdout not JSON ({e}): {stdout}"));
    let cmd = v
        .get("cmd")
        .filter(|c| !c.is_null())
        .or_else(|| v.get("evidence").and_then(|e| e.get("cmd")));
    assert!(
        cmd.is_none(),
        "an unparseable (unterminated-quote) command was extracted as argv {cmd:?} instead of being undetermined: {stdout}"
    );
}
