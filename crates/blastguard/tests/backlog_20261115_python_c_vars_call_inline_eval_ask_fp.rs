#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, PRE-EXISTING at main HEAD, observed 2026-11-15): a
//! `python3 -c` program that merely CALLS the builtin `vars(...)` for display is
//! refused (ask in main, deny in a subagent) with "that program builds or decodes
//! the code it runs (`vars(`)", although nothing in the program builds, decodes or
//! runs code. The sibling builtin `locals()` / `globals()` called the same way is
//! allowed.
//!
//! Reproduced 2026-11-15 at main HEAD (blastguard 0.2.613, `git archive HEAD`
//! built into the scratchpad, scratch HOME, cwd=HOME/wt, binary run directly with
//! PreToolUse JSON on stdin, env -i). Observed (main / subagent):
//!   `python3 -c 'print(vars())'`                              ask / deny  "`python3` is invoked
//!       with an inline-eval flag, and that program builds or decodes the code it runs (`vars(`) ..."
//!   `python3 -c 'print(vars(object()))'`                      ask / deny  (same)
//!   `echo 'print(1)' | python3 -c 'print(vars())'`            ask / deny  (same)
//!   `echo 'print(1)' | python3 -c 'import argparse; print(vars(argparse.Namespace(a=1)))'`
//!                                                             ask / deny  (same)
//!   `python3 -c 'print(locals())'`                            allow / allow
//!   `echo 'print(1)' | python3 -c 'print(globals())'`         allow / allow
//!   controls: `python3 -c 'vars()["exec"]("print(1)")'`       ask / deny  (`vars(`)
//!       `python3 -c 'globals()["exec"]("print(1)")'`          ask / deny  (`globals()[`)
//!       `python3 -c 'print(vars(__builtins__)["exec"])'`      ask / deny  (`__builtins__`)
//!       `python3 -c 'import builtins; vars(builtins)["exec"]("print(1)")'`  ask / deny (`vars(`)
//!
//! Two readings: (1) real defect: the detector matches the substring `vars(`
//! regardless of what follows, whereas the equivalent `globals()` is only flagged
//! when subscripted (`globals()[`), so a plain `vars()` / `vars(obj)` whose result
//! is printed is misread as a code-builder; (2) intended: `vars()` returns the
//! module dict and `vars(builtins)["exec"]` reaches `exec`, so every `vars(` must
//! be refused. Tell apart: reading (2) cannot be about `vars(` itself, because the
//! reachable-`exec` shapes are already caught by their OWN markers
//! (`globals()[`, `__builtins__`) and the same `vars(builtins)["exec"]` shape is
//! flagged by the `vars(` marker only incidentally; and `globals()` — the same dict
//! as `vars()` at module level — is allowed. A program that prints `vars()` or
//! `vars(<instance>)` has no `[`/`.`-call that can reach `exec`.
//!
//! Asserted: `print(vars())` and `import argparse; print(vars(argparse.Namespace(a=1)))`,
//! alone and with a harmless `echo 'print(1)' |` in front, are allow in main and
//! subagent. Controls (pass at HEAD; must stay != allow): `vars()["exec"](...)`,
//! `vars(builtins)["exec"](...)`, `globals()["exec"](...)`, `vars(__builtins__)`.
//!
//! DOES NOT PROVE: that all `vars(...)` programs are safe (only the two exact
//! display-only programs above); that `vars(builtins)` without a subscript but
//! followed by `.get("exec")` / `getattr` is refused (not probed); that the
//! `import argparse` program passes the stdin-program rule for other imports (note
//! `echo x | python3 -c 'import sys; print(vars(sys))'` is a DIFFERENT ask, "takes
//! its program from stdin", observed, not asserted here); or the exact Ask/Deny
//! reason text after a fix.
//!
//! RED written by an independent test author, not the implementer.

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
        "bg-pyvarscall-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt").join("run")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pyvarscall-20261115",
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

#[test]
#[ignore = "RED: 2026-11-15 python3 -c 'print(vars())' / print(vars(argparse.Namespace(..))) is refused as 'builds or decodes the code it runs (`vars(`)' though nothing builds code (locals()/globals() allowed) — remove when fixed"]
fn python_c_display_only_vars_call_is_allowed() {
    for cmd in [
        "python3 -c 'print(vars())'",
        "echo 'print(1)' | python3 -c 'print(vars())'",
        "echo 'print(1)' | python3 -c 'import argparse; print(vars(argparse.Namespace(a=1)))'",
    ] {
        let home = unique_home("fp");
        let (m, mr) = run_as(cmd, &home, false);
        let (s, sr) = run_as(cmd, &home, true);
        assert_eq!(m, "allow", "main must allow `{cmd}`; reason={mr}");
        assert_eq!(s, "allow", "subagent must allow `{cmd}`; reason={sr}");
    }
}

/// Controls (pass at HEAD): a `vars`/`globals` result that is subscripted to
/// reach `exec`, or `__builtins__` itself, must stay refused.
#[test]
fn control_vars_or_globals_reaching_exec_is_not_allowed() {
    for cmd in [
        "python3 -c 'vars()[\"exec\"](\"print(1)\")'",
        "python3 -c 'import builtins; vars(builtins)[\"exec\"](\"print(1)\")'",
        "python3 -c 'globals()[\"exec\"](\"print(1)\")'",
        "python3 -c 'print(vars(__builtins__)[\"exec\"])'",
    ] {
        let home = unique_home("ctl");
        let (m, mr) = run_as(cmd, &home, false);
        let (s, sr) = run_as(cmd, &home, true);
        assert_ne!(m, "allow", "main must not allow `{cmd}`; reason={mr}");
        assert_ne!(s, "allow", "subagent must not allow `{cmd}`; reason={sr}");
    }
}

/// Control (passes at HEAD): the sibling builtin called the same display-only
/// way is allowed today — the shape the fix should extend to `vars`.
#[test]
fn control_display_only_locals_call_is_allowed() {
    let home = unique_home("sib");
    let (m, mr) = run_as("python3 -c 'print(locals())'", &home, false);
    assert_eq!(m, "allow", "sibling builtin baseline: {mr}");
}
