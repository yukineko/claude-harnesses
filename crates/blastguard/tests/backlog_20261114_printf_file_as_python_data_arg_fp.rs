#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, p1, observed 2026-11-14): a `printf` whose output is
//! redirected into a file that the SAME command later passes as a DATA
//! argument (argv) to a python script is refused, as if the file were the
//! python program:
//!
//!   printf '%s\n' a b > d.txt && python3 s.py d.txt
//!
//! Live hit (subagent): "`printf` writes `$S/o/c.txt`, and another part of
//! this command may run that file with Python, so the written text is judged
//! as the Python program it is; its arguments (...) need printf's option or
//! format-reuse rules, which are not modelled, so blastguard cannot see the
//! program the file will hold". There the file was argv[2] of `p.py`, not the
//! program.
//!
//! Reproduced at main 4139f1d2 (blastguard 0.2.539), main / subagent:
//!   printf '%s\n' a b > d.txt && python3 s.py d.txt        ask / deny
//!   printf '%s\n' a b > d.txt; python3 s.py d.txt          ask / deny
//!   printf '%s\n' a b > d.txt && python3 s.py bin d.txt    ask / deny
//!   printf '%s\n' a b > d.txt && python3 d.txt             ask / deny (TRUE positive)
//!   printf '%s\n' a b > s.py  && python3 s.py              ask / deny (TRUE positive)
//!   printf 'x' > d.txt && python3 s.py d.txt               allow / allow
//!   printf 'import os' > s.py && python3 s.py              allow / allow
//! i.e. the refusal needs printf format-reuse (`%s\n` with several operands);
//! a single-conversion printf is judged and passes. (Literal /tmp/... paths
//! hit a different refusal, symlink resolution of a nonexistent dir, which is
//! an artifact of the probe environment and not asserted here.)
//!
//! Asserted (RED, `#[ignore]`d): the data-argument shapes are Allow in main
//! and in subagent mode. Controls (not ignored): the file IS the program
//! (`> s.py && python3 s.py`, and `python3 d.txt`) stays non-Allow, so the fix
//! must not blanket-allow printf-then-python.
//!
//! DOES NOT PROVE: shapes where the data file is also read by the python
//! program as code (`exec(open(argv[1]).read())`), other interpreters, or
//! `cat`/`echo` writers.
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
        "bg-pfpy-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "printf-file-python-data-arg-20261114",
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

const DATA_ARG_SHAPES: &[&str] = &[
    "printf '%s\\n' a b > d.txt && python3 s.py d.txt",
    "printf '%s\\n' a b > d.txt; python3 s.py d.txt",
    "printf '%s\\n' a b > d.txt && python3 s.py bin d.txt",
];

#[test]
#[ignore = "RED: 2026-11-14 printf-written file passed as a data argument to a python script is judged as the program — remove the ignore when fixed"]
fn printf_file_passed_as_python_data_argument_is_allow() {
    let home = unique_home("red");
    let mut bad = Vec::new();
    for shape in DATA_ARG_SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(shape, &home, subagent);
            if d != "allow" {
                bad.push(format!("subagent={subagent} {shape:?} => {d} ({r})"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "expected Allow for every shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_printf_file_that_is_the_python_program_is_not_allow() {
    let home = unique_home("ctl");
    for shape in [
        "printf '%s\\n' a b > s.py && python3 s.py",
        "printf '%s\\n' a b > d.txt && python3 d.txt",
    ] {
        for subagent in [false, true] {
            let (d, r) = run_as(shape, &home, subagent);
            assert_ne!(
                d, "allow",
                "control {shape:?} (subagent={subagent}): {d} {r}"
            );
        }
    }
}
