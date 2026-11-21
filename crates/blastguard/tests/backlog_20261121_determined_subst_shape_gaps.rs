#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED for shape gaps next to backlog 28019c35 / d766b5fe, named by the
//! independent verifiers of run-20261121-114321-94421 (2026-11-21). The fixes
//! compute a determined `$( )` program only for `bash -c W` (flag `-c` alone,
//! first) and for `eval W` (exactly one operand, bare `eval`). The same
//! harmful program spelled with another flag layout, a `--`, a second operand
//! or a `builtin` / `command` prefix stays Ask, while system harm must be Deny
//! through any wrapper.
//!
//! Every case runs in main and subagent mode and asserts the decision only.
//!
//! DOES NOT PROVE: reason text; real shell behaviour (nothing is executed).

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
        "bg-subst-shape-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "determined-subst-shape-gaps-20261121",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
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
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

fn expect_deny(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let d = decide(cmd, &home, subagent);
        if d != "deny" {
            bad.push(format!("subagent={subagent} => {d}"));
        }
    }
    assert!(
        bad.is_empty(),
        "expected Deny for {cmd:?}:\n{}",
        bad.join("\n")
    );
}

// ------------------------------------------- shell -c flag layouts

#[test]
fn dash_c_shape_bundled_ec_is_deny() {
    expect_deny(r#"bash -ec "$(echo 'rm -rf /etc')""#, "c-ec");
}

#[test]
fn dash_c_shape_flag_before_c_is_deny() {
    expect_deny(r#"bash -x -c "$(echo 'rm -rf /etc')""#, "c-x");
}

#[test]
fn dash_c_shape_double_dash_is_deny() {
    expect_deny(r#"bash -c -- "$(echo 'rm -rf /etc')""#, "c-dd");
}

// ------------------------------------------- eval operand shapes / prefixes

#[test]
fn eval_shape_second_operand_is_deny() {
    expect_deny(r#"eval "$(echo 'rm -rf /etc' | cat)" x"#, "e-two");
}

#[test]
fn eval_shape_double_dash_is_deny() {
    expect_deny(r#"eval -- "$(echo 'rm -rf /etc' | cat)""#, "e-dd");
}

#[test]
fn eval_shape_builtin_prefix_is_deny() {
    expect_deny(r#"builtin eval "$(echo 'rm -rf /etc' | cat)""#, "e-builtin");
}

#[test]
fn eval_shape_command_prefix_is_deny() {
    expect_deny(r#"command eval "$(echo 'rm -rf /etc' | cat)""#, "e-command");
}

// ------------------------------------------- harmless same-shape controls
// (added at integration of run-20261121-225214-62742): the raise-only
// readings must not turn a harmless computed program into a Deny.

/// Main mode only: a subagent turns every Ask into a Deny, so only the main
/// session tells a raised Deny apart from a kept Ask.
fn expect_not_deny(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let d = decide(cmd, &home, false);
    assert_ne!(d, "deny", "harmless {cmd:?} (main session) => {d}");
}

#[test]
fn control_harmless_dash_c_shapes_are_not_deny() {
    for (i, cmd) in [
        r#"bash -ec "$(echo ls)""#,
        r#"bash -x -c "$(echo ls)""#,
        r#"bash -c -- "$(echo ls)""#,
        r#"bash -o pipefail -c "$(echo ls)""#,
    ]
    .iter()
    .enumerate()
    {
        expect_not_deny(cmd, &format!("hc{i}"));
    }
}

#[test]
fn control_harmless_eval_shapes_are_not_deny() {
    for (i, cmd) in [
        r#"eval "$(echo ls | cat)" x"#,
        r#"eval -- "$(echo ls | cat)""#,
        r#"builtin eval "$(echo echo hi | cat)""#,
        r#"command eval "$(echo ls | cat)""#,
    ]
    .iter()
    .enumerate()
    {
        expect_not_deny(cmd, &format!("he{i}"));
    }
}
