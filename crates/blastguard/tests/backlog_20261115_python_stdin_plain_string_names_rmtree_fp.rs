#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-15 at main 8c135610e, installed
//! blastguard 0.2.613, actor=subagent, Bash tool call): a `python3 - <<'PY'`
//! program that only builds a dict of strings and writes JSON with json.dump
//! was blocked because one ordinary double-quoted string in it contained the
//! text `backlog_20261114_python_stdin_triple_quoted_rmtree_literal_fp`. The
//! substring `rmtree` appears only inside a plain quoted literal that is never
//! executed (no exec/eval/os.system sink). Verbatim hook reason:
//!
//!   PreToolUse:Bash hook error: `python3 - <<'PY'` takes its program from
//!   stdin, and a quoted string in its program spells `rmtree`, which deletes
//!   files or directories outright if that string is run as code; blastguard
//!   cannot show it is not
//!
//! Two readings (UNRESOLVED, must be told apart by the fix owner):
//!  (a) real FP: the literal is never run as code, so nothing is destructive.
//!      Under the 2026-11-08 ruling (separate text from code with a real
//!      parser; Python lexer in detect/interpreter/python_semantics.rs), the
//!      program has no sink for the string and should be Allow.
//!  (b) intended Ask from 27795e6e (downgrade of the plain single-line literal
//!      case, "not asserted"): the gate cannot show the string is not run.
//!      Tell them apart: if the program has no exec/eval/compile/os.system/
//!      subprocess/importlib/getattr-style sink reachable by the literal (the
//!      minimised repro below), (a) holds and Ask is a false positive; if a
//!      sink exists or cannot be excluded, Ask is correct and this test's
//!      expectation must be narrowed with a stated reason.
//!
//! Related: backlog row "blastguard: python3 - heredoc program with rmtree
//! only inside a triple-quoted string literal denied as destructive call"
//! (tests/backlog_20261114_python_stdin_triple_quoted_rmtree_literal_fp.rs),
//! which covers triple-quoted literals; this one is the plain-literal case.
//!
//! DOES NOT PROVE: that the real command's exact shape (json.dump to a file)
//! is needed; the minimised repro uses json.dumps/print.

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
        "bg-pyplain-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-stdin-plain-rmtree-20261115",
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

fn stdin_program(body: &str) -> String {
    format!("python3 - <<'PY'\n{body}PY\n")
}

const PLAIN_LITERAL: &str = "import json\nd = {\"name\": \"x_rmtree_fp\"}\nprint(json.dumps(d))\n";

// test-weakening-justified: new RED for an unfixed false positive (backlog
// pending, 2026-11-15); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: <backlog-pending> 2026-11-15 python3 - heredoc program with rmtree only inside a plain string literal asked/denied; remove the ignore when fixed"]
fn python_stdin_plain_string_naming_rmtree_is_not_a_call() {
    let home = unique_home("red");
    let command = stdin_program(PLAIN_LITERAL);
    for subagent in [false, true] {
        let (d, r) = run_as(&command, &home, subagent);
        assert_eq!(d, "allow", "subagent={subagent}: {d} | {r}");
    }
}

// Sanity: same program with a harmless spelling is Allow, so the `rmtree`
// text is what is judged.
#[test]
fn sanity_plain_literal_harmless_text_is_allowed() {
    let home = unique_home("san");
    let command = stdin_program(&PLAIN_LITERAL.replace("rmtree", "copy"));
    for subagent in [false, true] {
        let (d, r) = run_as(&command, &home, subagent);
        assert_eq!(d, "allow", "subagent={subagent}: {d} | {r}");
    }
}

// Control: programs that really run rmtree are not Allow.
#[test]
fn control_python_stdin_real_rmtree_calls_are_not_allowed() {
    let home = unique_home("ctl");
    for body in [
        "import shutil\nshutil.rmtree('build')\n",
        "import json, shutil\nd = {\"name\": \"x\"}\nprint(json.dumps(d))\nshutil.rmtree(\"build\")\n",
        "exec('import shutil; shutil.rmtree(\"build\")')\n",
    ] {
        let command = stdin_program(body);
        for subagent in [false, true] {
            let (d, r) = run_as(&command, &home, subagent);
            assert_ne!(d, "allow", "body={body:?} subagent={subagent}: {d} | {r}");
        }
    }
}
