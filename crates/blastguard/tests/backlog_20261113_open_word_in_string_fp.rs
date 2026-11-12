#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, traced by a verifier; observed against the real hook at
//! fix/bg-python-open-write-modes 3efbbf51, blastguard 0.2.485): a benign
//! READ-ONLY python program that merely mentions the word `open` in a string or
//! comment (not as a call) is treated as write-capable, so its absolute path
//! literals get probed and the program is not Allowed.
//!
//! Each pair is (with_open, without_open): the second is the identical program
//! with the word `open` replaced by `shut`. The control test asserts the
//! without-open programs are Allow (isolates the cause to the word `open`).
//!
//! Asserted: the with-open benign programs are Allow, in main and subagent.
//!
//! DOES NOT PROVE: that real writers are still caught, other interpreters, or
//! other write-capability triggers.

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
        "bg-openword-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-openword-20261113",
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

/// Benign read-only programs templated with {W} (the word) and {H} (the temp
/// HOME, so a path under it outside the worktree is unrecoverable).
const TEMPLATES: &[&str] = &[
    "python3 -c \"print('cannot {W}'); from pathlib import Path; print(Path('/Users/someone/notes/a.md').read_text())\"",
    "python3 -c \"print('cannot {W}'); from pathlib import Path; print(Path('{H}/notes/a.md').read_text())\"",
    "python3 -c \"import os; print('{W} sesame'); print(os.path.exists('/etc/hosts'))\"",
    "python3 -c \"import os; print(os.path.exists('/etc/hosts')) # {W} the door\"",
    "python3 -c \"import os; print(os.path.isfile('{H}/notes/a.md')) # {W}\"",
];

fn render(t: &str, word: &str, home: &Home) -> String {
    t.replace("{W}", word)
        .replace("{H}", &home.0.display().to_string())
}

// test-weakening-justified: new RED for an unfixed false positive; the ignore
// is removed in the fixing commit.
#[test]
#[ignore = "RED: FP, the word `open` in a string/comment makes a read-only python program write-capable"]
fn read_only_python_mentioning_open_in_string_or_comment_is_allowed() {
    let home = unique_home("red");
    let mut failures = Vec::new();
    for t in TEMPLATES {
        let c = render(t, "open", &home);
        for subagent in [false, true] {
            let (d, r) = run_as(&c, &home, subagent);
            if d != "allow" {
                failures.push(format!("subagent={subagent} {c:?} -> {d} | {r}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} benign read-only program(s) mentioning `open` were NOT allowed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// Control: the identical programs without the word `open` are Allow.
#[test]
fn control_same_programs_without_the_word_open_are_allowed() {
    let home = unique_home("ctl");
    for t in TEMPLATES {
        let c = render(t, "shut", &home);
        for subagent in [false, true] {
            let (d, r) = run_as(&c, &home, subagent);
            assert_eq!(d, "allow", "subagent={subagent} {c:?} -> {d} | {r}");
        }
    }
}
