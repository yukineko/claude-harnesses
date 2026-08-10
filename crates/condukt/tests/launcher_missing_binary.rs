// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `bin/condukt` (the POSIX launcher) — what happens when NO per-platform
//! binary is bundled for the host.
//!
//! The launcher used to answer that with a bare `exit 0` for the whole surface.
//! For condukt that is not merely silence: several subcommands carry their
//! verdict IN THE EXIT CODE, and 0 is the permissive value of that vocabulary.
//! The sharpest is `policy answer`, whose documented contract is
//! `0=auto, 2=escalate, 3=block` (src/main.rs:1089-1091) — so a missing binary
//! answered every autonomy gate with `auto`, the most permissive verdict
//! available, while printing nothing. The /condukt skill's own `case $?` treats
//! any OTHER code as the safe side ("旧バイナリ … / 不正入力 → 安全側 =
//! AskUserQuestion", skills/condukt/SKILL.md), so exiting non-zero is both
//! restrictive and already handled by the consumer.

use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn launcher_src() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("bin")
        .join("condukt")
}

fn host_binary_name() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    };
    let ext = if os == "windows" { ".exe" } else { "" };
    format!("condukt-{os}-{arch}{ext}")
}

fn run(args: &[&str], stdin: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("condukt");
    std::fs::copy(launcher_src(), &launcher).expect("copy launcher");

    let mut child = Command::new("sh")
        .arg(&launcher)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ------------------------------------------------- exit-code-as-verdict CLI

/// The one that matters most: `0` means `auto` here, so the old launcher
/// self-answered every autonomy gate with the most permissive verdict.
#[test]
fn policy_answer_missing_binary_never_exits_zero_which_would_mean_auto() {
    let r = run(
        &[
            "policy",
            "answer",
            "--risk",
            "high",
            "--reversible",
            "low",
            "--confidence",
            "low",
            "--question",
            "q",
            "--option",
            "a",
        ],
        "",
    );
    assert_ne!(
        r.code, 0,
        "exit 0 IS `auto` in this contract (0=auto, 2=escalate, 3=block): {r:?}",
    );
    assert!(
        r.stdout.trim().is_empty(),
        "a launcher that could not decide must not print a decision word: {r:?}",
    );
}

/// The other exit-code verdicts: `gate check` exits non-zero on escalate,
/// `state gate` exits 1 when the run is incomplete, `verify`/`state
/// check-oracle` gate on 0. All of them read 0 as "proceed".
#[test]
fn exit_code_verdict_subcommands_do_not_report_success_they_did_not_earn() {
    for args in [
        vec!["gate", "check", "--run", "r1", "--task", "t1"],
        vec!["state", "gate", "--run", "r1"],
        vec!["state", "check-oracle", "--run", "r1"],
        vec!["state", "list"],
        vec![
            "policy",
            "decide",
            "--risk",
            "high",
            "--reversible",
            "low",
            "--confidence",
            "low",
        ],
        vec![],
    ] {
        let r = run(&args, "");
        assert_ne!(
            r.code,
            0,
            "`condukt {}` must not exit 0 without having run: {r:?}",
            args.join(" "),
        );
    }
}

// ----------------------------------------------------------------- the hooks

/// PostToolUse. The binary answers with `{"decision":"block"}`, but an
/// UNBOUNDED block on every Edit/Write/MultiEdit would trap the session — Stop
/// hooks have `stop_hook_active` to bound a fail-closed block and PostToolUse
/// has nothing equivalent. The repo's two existing PostToolUse fallbacks
/// (bin/fetchguard `scan`, bin/ctxrot `toolguard`) both resolve this the same
/// way: mark the result UNCHECKED via `additionalContext` rather than stay
/// silent. This test pins that, not silence.
#[test]
fn editgate_missing_binary_marks_the_edit_unchecked_instead_of_staying_silent() {
    let r = run(&["editgate"], r#"{"tool_name":"Edit","tool_input":{}}"#);
    assert_eq!(r.code, 0, "PostToolUse: {r:?}");
    assert!(
        !r.stdout.trim().is_empty(),
        "silence is what 'this edit compiles' looks like: {r:?}",
    );
    let v: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be hook JSON ({e}): {}", r.stdout));
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"], "PostToolUse",
        "{v}"
    );
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default();
    assert!(
        ctx.contains("condukt") && ctx.to_uppercase().contains("UNCHECKED"),
        "the edit must be marked unchecked, not compiling: {ctx}",
    );
}

/// SessionStart. `hooks::restore` injects raw stdout as context and is silent
/// when there is nothing to resume, so silence reads as "no open runs and no
/// orphan worktrees" — which is exactly what could not be established.
#[test]
fn restore_missing_binary_says_the_run_state_was_not_read() {
    let r = run(&["restore"], r#"{"cwd":"/tmp"}"#);
    assert_eq!(r.code, 0, "SessionStart cannot block: {r:?}");
    assert!(
        !r.stdout.trim().is_empty(),
        "silence reads as 'nothing to resume': {r:?}",
    );
    let s = r.stdout.to_uppercase();
    assert!(
        s.contains("NOT") && r.stdout.contains("condukt"),
        "it must say the run state was NOT read: {r:?}",
    );
}

/// Stop. `state record-run` is a pure recorder, not a gate: nothing downstream
/// reads its absence as "clean" — the gates that consume run-state
/// (`state gate`, `state check-oracle`) see an UNrecorded task as incomplete,
/// which is the restrictive direction on its own. It keeps exit 0 so the Stop
/// hook is not reported as failed on every turn, but it must not be silent.
#[test]
fn state_record_run_exits_zero_but_still_says_it_did_not_run() {
    let r = run(&["state", "record-run", "--all"], r#"{"session_id":"s1"}"#);
    assert_eq!(r.code, 0, "{r:?}");
    assert!(
        !r.stderr.trim().is_empty(),
        "exit 0 with no diagnostic is indistinguishable from a clean run: {r:?}",
    );
}

// ------------------------------------------------------------- anti-vacuity

#[test]
fn control_launcher_still_execs_the_host_binary_when_it_is_present() {
    let dir = tempfile::tempdir().expect("tempdir");
    let launcher = dir.path().join("condukt");
    std::fs::copy(launcher_src(), &launcher).expect("copy launcher");

    let stub = dir.path().join(host_binary_name());
    std::fs::write(&stub, "#!/bin/sh\necho \"STUB-RAN:$*\"\nexit 0\n").expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let out = Command::new("sh")
        .arg(&launcher)
        .args(["policy", "answers"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn sh");
    assert_eq!(
        out.status.code(),
        Some(0),
        "with the host binary present the launcher must exec it and pass its \
         exit code through: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "STUB-RAN:policy answers",
        "argv must reach the host binary unchanged",
    );
}
