#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14 by another agent):
//! `printf '%s\n' '[{"cmd":"echo hi"}]' > cases5.json && python3 probe.py cases5.json`
//! (probe.py exists in the worktree and only reads its argument) was denied:
//! "`printf` writes `cases5.json`, and another part of this command may run
//! that file with Python, so the written text is judged as the Python program it
//! is; its arguments (...) need printf's option or format-reuse rules, which are
//! not modelled...".
//!
//! Two readings: (1) real FP: cases5.json is an argument of probe.py, a data
//! file, not the program python runs; (2) intended: any written file also named
//! on a python command line is judged as a program. Tell apart: python's program
//! is its first non-option operand (probe.py); a later operand is argv. Control:
//! `python3 probe.py cases5.json` alone (now Undetermined under S4 B.5, see the
//! control test).
//!
//! Asserted (re-anchored per user ruling 2026-11-18, S4: same verdict as the
//! program alone, reason names the program; originally subagent == "allow"):
//! the verdict equals that of `python3 probe.py cases5.json` alone in main and
//! subagent mode, the main-mode reason cites Table B.5 and names `probe.py`,
//! and no reason contains the legacy "judged as the Python program" reading.
//!
//! DOES NOT PROVE: `python3 cases5.json` (running the written file; must stay
//! refused/judged).
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
        "bg-side1114-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261114",
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

fn seed(home: &Home) {
    std::fs::write(
        home.0.join("wt").join("probe.py"),
        "import sys\nprint(open(sys.argv[1]).read())\n",
    )
    .unwrap();
}

// Re-anchored per user ruling 2026-11-18 (S4: same verdict as the program alone, reason names the program)
//
// Was: subagent == "allow". Under S4 Table B.5 `python3 probe.py` (program
// text not in the command) is Undetermined, so the verdict must equal that of
// `python3 probe.py cases5.json` alone in both modes, and the reason must be
// the B.5 one naming `probe.py`, not the legacy printf reading that judges the
// data file `cases5.json` as the Python program.
#[test]
fn printf_json_then_python_script_with_json_arg_judged_as_program_alone() {
    let home = unique_home("red");
    seed(&home);
    let cmd =
        "printf '%s\\n' '[{\"cmd\":\"echo hi\"}]' > cases5.json && python3 probe.py cases5.json";
    let alone = "python3 probe.py cases5.json";
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = run_as(cmd, &home, subagent);
        let (da, ra) = run_as(alone, &home, subagent);
        if d != da {
            bad.push(format!(
                "subagent={subagent}: {d} ({r}); program alone => {da} ({ra})"
            ));
        }
        if !subagent && !(r.contains("Table B.5") && r.contains("`probe.py`")) {
            bad.push(format!(
                "subagent={subagent}: reason must cite Table B.5 and name `probe.py`: {r}"
            ));
        }
        if r.contains("judged as the Python program") {
            bad.push(format!(
                "subagent={subagent}: legacy data-file-as-program reading: {r}"
            ));
        }
    }
    assert!(bad.is_empty(), "cmd={cmd:?}\n{}", bad.join("\n"));
}

#[test]
fn control_python_probe_alone_is_allow() {
    let home = unique_home("ctl");
    seed(&home);
    std::fs::write(home.0.join("wt").join("cases5.json"), "[]").unwrap();
    let (d, r) = run_as("python3 probe.py cases5.json", &home, true);
    // Re-anchored Allow -> Deny (subagent Undetermined): user ruling
    // 2026-11-18 (S4 B.5) -- a python program file run is Undetermined; the
    // reason names the program, not the data operand.
    assert_eq!(d, "deny", "control reason={r}");
    assert!(
        r.contains("probe.py") && !r.contains("cases5.json"),
        "reason={r}"
    );
}
