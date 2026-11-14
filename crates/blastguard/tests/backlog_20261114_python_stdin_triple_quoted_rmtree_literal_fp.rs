#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14 at main 9bab9873e, blastguard
//! 0.2.594 built from that rev; first seen in run-20261114-163341-69351, task
//! worktree run-20261114-163341-69351-b793fddc, blastguard 0.2.588 block log,
//! rule interpreter-destructive-call): a `python3 - <<'EOF'` program that only
//! holds `shutil.rmtree(...)` as TEXT inside a triple-quoted string literal
//! (and writes that text to a file, or prints it) is denied. Verbatim reason on
//! the minimised form (main and subagent: deny):
//!
//!   `python3 - <<'EOF'` takes its program from stdin, and a quoted string in
//!   its program spells `rmtree`, which deletes files or directories outright
//!   if that string is run as code; blastguard cannot show it is not
//!
//! The run's verbatim command (a program rewriting
//! scripts/check-fail-open-mutation.py via `p.write_text(s)`, rmtree only in
//! `r'''…'''`) was denied with "… and it runs `rmtree`, which deletes files or
//! directories outright" — it calls no rmtree.
//!
//! The delimiter is quoted (no shell expansion reaches the program), the
//! program calls nothing that runs a string, and the literal's contents are
//! data. Under the user ruling of 2026-11-08 (tell executed code from inert
//! strings with a real parser, never heuristics), Python's own tokenizer can
//! show `rmtree` is only string contents here.
//!
//! Observed splits (main session, 0.2.594):
//!   s = 'shutil.rmtree(x)'; print(s)              ask  (27795e6e downgrade)
//!   s = '''shutil.rmtree(x)'''; print(s)          deny <- FP
//!   p.write_text(r'''shutil.rmtree(x)\n''')       deny <- FP
//!   p.write_text(r'''shutil.copy(x)\n''')         allow (sanity)
//!   import shutil; shutil.rmtree('build')         deny (control)
//!
//! Suspected site (UNVERIFIED, not proven by this test): the layer-0 substring
//! scan `interpreter_destructive_calls` in
//! crates/blastguard/src/detect/interpreter.rs (~1165) whose only downgrade is
//! the heuristic `inert_literal_bodies`
//! (crates/blastguard/src/detect/interpreter/inert_literal.rs ~123), which
//! returns None for any program containing `'''`/`"""` ("triple-quoted strings
//! are not modelled"); the Python lexer in detect/interpreter/python_semantics.rs
//! is not consulted there.
//!
//! Controls (GREEN at HEAD, not ignored): a stdin program that really calls
//! shutil.rmtree, one that calls it after a triple-quoted literal, and one that
//! exec()s a literal spelling rmtree are not Allow.
//!
//! DOES NOT PROVE: the plain single-quoted literal Ask (asserted nowhere here);
//! that the "it runs `rmtree`" wording of the verbatim command has the same cause.

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
        "bg-pytriple-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-stdin-triple-rmtree-20261114",
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
    format!("python3 - <<'EOF'\n{body}EOF\n")
}

const WRITE_RAW_TRIPLE: &str =
    "from pathlib import Path\np = Path('out.py')\np.write_text(r'''shutil.rmtree(x)\n''')\n";
const PRINT_TRIPLE: &str = "s = '''shutil.rmtree(x)'''\nprint(s)\n";

// test-weakening-justified: new RED for an unfixed false positive (backlog
// pending, 2026-11-14); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: <backlog-pending> 2026-11-14 python3 - heredoc program with rmtree only inside a triple-quoted string literal denied as destructive call; remove the ignore when fixed"]
fn python_stdin_triple_quoted_rmtree_text_is_not_a_call() {
    let home = unique_home("red");
    for body in [WRITE_RAW_TRIPLE, PRINT_TRIPLE] {
        let command = stdin_program(body);
        for subagent in [false, true] {
            let (d, r) = run_as(&command, &home, subagent);
            assert!(
                !r.contains("rmtree"),
                "body={body:?} subagent={subagent}: string literal read as an rmtree call: {d} | {r}"
            );
            assert!(
                d != "deny" && d != "ask",
                "body={body:?} subagent={subagent}: {d} | {r}"
            );
        }
    }
}

// Sanity (GREEN at HEAD): the same program with a harmless spelling is Allow,
// so the `rmtree` text is what is judged.
#[test]
fn sanity_triple_quoted_harmless_text_is_allowed() {
    let home = unique_home("san");
    let command = stdin_program(&WRITE_RAW_TRIPLE.replace("rmtree", "copy"));
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
        "import shutil\ns = '''doc'''\nshutil.rmtree('build')\n",
        "exec('import shutil; shutil.rmtree(\"build\")')\n",
    ] {
        let command = stdin_program(body);
        for subagent in [false, true] {
            let (d, r) = run_as(&command, &home, subagent);
            assert_ne!(d, "allow", "body={body:?} subagent={subagent}: {d} | {r}");
        }
    }
}
