#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Scout 2026-11-12 S3/S4: false positives on read-only commands.
//!
//! The commands are VERBATIM from the recorded subagent transcripts
//! (agent-a02fd793e70de4811.jsonl line 162; agent-af2980d2532fbc893.jsonl line
//! 92), JSON-decoded. Only substitutions: the `cd` target is a temp dir, and
//! (S4) the `/Users/yuki` HOME prefix is the temp HOME.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-scout-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

/// Run the hook as a subagent (non-empty `agent_id`). Returns (decision, reason).
fn run(command: &str, cwd: &std::path::Path, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "scout-20261112",
        "cwd": cwd,
        "agent_id": "test-subagent",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn tmp_cwd(home: &Home) -> std::path::PathBuf {
    let d = home.0.join("wt");
    std::fs::create_dir_all(&d).unwrap();
    d
}

// cd target replaced by CWD placeholder; rest verbatim.
const S3: &str = r##"cd CWD && grep -rlnE "gh pr create|gh\", *\"pr\"|\"pr\", *\"create\"" crates/*/src crates/*/skills 2>/dev/null | head; grep -rnE "push|origin" crates/ship/src/*.rs 2>/dev/null | head -3; echo ---fde-run; grep -niE "test command|run the|cargo test|npm test|pytest|install" crates/fde/skills/fde/SKILL.md | head -6"##;

// At HEAD the deny reason is "unrecognised `install` option `-6`" (recorded in the
// failure message of the RED below, which prints the reason).
#[test]
fn s3_grep_pattern_install_then_head_dash_6_is_allowed() {
    let home = unique_home("s3b");
    let cwd = tmp_cwd(&home);
    let cmd = S3.replace("CWD", cwd.to_str().unwrap());
    let (d, r) = run(&cmd, &cwd, &home);
    assert_eq!(d, "allow", "read-only grep chain: {d} | {r}");
}

// cd target -> CWD, HOME prefix -> HOMEDIR; rest verbatim (real newlines in the python program).
const S4: &str = r##"cd CWD; python3 -c "
import json;d=json.load(open('HOMEDIR/.claude/settings.json'));e=d.get('enabledPlugins',{})
print(sorted(k for k,v in e.items() if v)); print('DISABLED',sorted(k for k,v in e.items() if not v))"; echo; ls crates | while read c; do grep -q "\"$c@" ~/.claude/settings.json || echo "absent: $c"; done"##;

#[test]
#[ignore = "scout 2026-11-12 S4: RED, open defect. Remove the ignore when fixed"]
fn s4_python_c_read_only_open_is_allowed() {
    let home = unique_home("s4");
    let cwd = tmp_cwd(&home);
    let cmd = S4
        .replace("CWD", cwd.to_str().unwrap())
        .replace("HOMEDIR", home.0.to_str().unwrap());
    let (d, r) = run(&cmd, &cwd, &home);
    assert_eq!(
        d, "allow",
        "read-only python3 -c json.load(open(..)): {d} | {r}"
    );
}

// Controls for S4: a fix that allows the read must NOT also allow WRITES to the
// HOME-level protected config path.
fn write_control(mode: &str) {
    let home = unique_home("s4w");
    let cwd = tmp_cwd(&home);
    let cmd = format!(
        "python3 -c \"open('{}/.claude/settings.json','{mode}').write('x')\"",
        home.0.to_str().unwrap()
    );
    let (d, r) = run(&cmd, &cwd, &home);
    assert_eq!(
        d, "deny",
        "python3 write (mode {mode}) to HOME settings.json: {d} | {r}"
    );
}

#[test]
fn s4_control_python_c_write_mode_w_to_home_settings_is_denied() {
    write_control("w");
}

#[test]
fn s4_control_python_c_write_mode_a_to_home_settings_is_denied() {
    write_control("a");
}
