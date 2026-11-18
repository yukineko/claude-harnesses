// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! SUBSET.md Table B option lists — user ruling 2026-11-18 (S3b). Independent
//! test author (CLAUDE.md §2(a)); verdicts only, through the real hook binary
//! in an interactive session (so Ask is observable as Ask).
//!
//! The ruling:
//! * options that change neither the write target nor the delete target are
//!   listed on their rows and stay Allow (`cp -f/-p/-v/-i/-n`, `sed -E/-n/-r/-s`
//!   with `-i`, `-v` on `mv`/`rm`/`ln`/`chmod`, …);
//! * `--help` / `--version` are accepted on every row; a GNU long spelling is
//!   its short option (`--append` = `-a`);
//! * an unlisted literal option without an argument is NOT exempt: it stays
//!   Undetermined → Ask. (The S3b implementer's exemption — "every word is a
//!   literal no-arg option" → Allow — was removed by this ruling.)
//!
//! Targets are non-existent sentinel paths in the Scratch class (`/private/tmp`).

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

const S: &str = "/private/tmp/bg-sentinel-tableb-20261118";

fn verdict(command: &str) -> (String, String) {
    let base = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("table_b_option_ruling_{}", std::process::id()));
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let payload = serde_json::json!({
        "session_id": "table-b-option-ruling",
        "cwd": "/private/tmp",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    hook.env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("BLASTGUARD_APPROVALS_DIR", base.join("store"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 for {command:?}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        return ("allow".into(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let h = &doc["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap().to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn expect_all(cases: &[String], want: &str) {
    let mut wrong = Vec::new();
    for cmd in cases {
        let (d, r) = verdict(cmd);
        if d != want {
            wrong.push(format!("{cmd:?} => {d} | {r}"));
        }
    }
    assert!(wrong.is_empty(), "expected {want}:\n{}", wrong.join("\n"));
}

/// Effect-neutral options of `cp`, `sed -i` and `chmod` (ruling 2026-11-18).
#[test]
fn effect_neutral_options_cp_sed_chmod_stay_allow() {
    let cases = [
        format!("cp -f {S}/a {S}/b"),
        format!("cp -p {S}/a {S}/b"),
        format!("cp -v {S}/a {S}/b"),
        format!("cp -i {S}/a {S}/b"),
        format!("cp -n {S}/a {S}/b"),
        format!("sed -i -E 's/a/b/' {S}/a"),
        format!("sed -n -i 's/a/b/p' {S}/a"),
        format!("sed -i -r 's/a/b/' {S}/a"),
        format!("sed -i -s 's/a/b/' {S}/a"),
        format!("chmod -v 644 {S}/a"),
    ];
    expect_all(&cases, "allow");
}

/// Effect-neutral options already on their rows, `--help` / `--version`, and
/// GNU long spellings of listed short options (ruling 2026-11-18).
#[test]
fn effect_neutral_listed_options_help_and_long_forms_stay_allow() {
    let cases = [
        format!("mv -v {S}/a {S}/b"),
        format!("rm -v {S}/a"),
        format!("ln -v -s {S}/a {S}/b"),
        format!("rm --verbose {S}/a"),
        format!("mv --verbose {S}/a {S}/b"),
        format!("tee --append {S}/a"),
        "cp --help".to_string(),
        "cp --version".to_string(),
        "rm --version".to_string(),
        "sed --help".to_string(),
    ];
    expect_all(&cases, "allow");
}

/// An unlisted, real, no-argument option with operands (`cp -X`: BSD "do not
/// copy extended attributes"; not in the `cp` row) is Undetermined → Ask.
#[test]
fn unlisted_no_arg_option_with_operands_asks() {
    expect_all(&[format!("cp -X {S}/a {S}/b")], "ask");
}

/// The same with no operand at all (`rm -P`: BSD "overwrite before
/// deleting"; not in the `rm` row). The implementer's exemption answered
/// Allow here; ruling 2026-11-18 removed it → Ask.
#[test]
fn unlisted_no_arg_option_alone_asks() {
    expect_all(&["rm -P".to_string()], "ask");
}
