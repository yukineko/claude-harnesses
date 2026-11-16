#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false-positive candidate, observed 2026-11-17 as relay
//! bgr-be0f4d8da498, a subagent of session 06c2bd77 in worktree
//! fix-b2d9cb6b; minimised by the test author on main aa1ccd1d0 /
//! blastguard 0.2.679): a `python3 - <<'EOF'` program that writes only
//! LITERAL paths (`open('worktree.rs','w').write(s)`) and holds no
//! exec/eval/subprocess/os.system, but has a backtick inside a plain string
//! literal (Rust doc text such as '/// ... `topic` ...'), followed later by
//! `. "$HOME/.cargo/env"`, is Ask (main) / Deny (subagent). Verbatim reason:
//!   "`the Python program` writes `a path the Python program does not spell
//!    as a literal`, and another part of this command may run that file, so
//!    the written text is judged as the shell script it is; the Python
//!    program holds a dynamic-execution or process marker (`exec`, `eval`,
//!    `subprocess`, `os.system`, …), so blastguard does not read what it
//!    writes, so blastguard cannot see the program the file will hold"
//!
//! Chain (read from source at aa1ccd1d0):
//!   1. `inert_literal::DYNAMIC_EXECUTION_MARKERS` contains "`" and
//!      `has_dynamic_code_execution` is a lowercase substring match over the
//!      whole program, so a backtick inside a string literal hits;
//!   2. `python_written_files::python_program_writes` then returns ONE write
//!      with `path: None` — the literal paths it would otherwise read are
//!      dropped;
//!   3. `echo_script_file::python_heredoc_files` makes that an `Opaque`
//!      target, which "is run by any pipeline holding a shell, `.` or
//!      `source` word"; `. "$HOME/.cargo/env"` is such a word.
//!
//! Two readings:
//!   (a) defect: the backtick is Python string data, never executed; the
//!       written paths are exact literals, and nothing in the command names
//!       them, so no run of a written file exists;
//!   (b) intended: any marker makes blastguard refuse to read the program.
//!       But the reason claims the path is not spelled as a literal while it
//!       is, which points to (a).
//!
//! Asserted (ignored, RED): the minimised shapes are not Ask/Deny in main or
//! subagent mode. Controls (not ignored): the no-backtick twin is Allow; a
//! program that writes `rm -rf ~` to `f` and runs it with
//! `subprocess.run(['bash','f'])` stays Deny; a backtick text written to `f`
//! followed by `bash f` stays not-Allow.
//!
//! DOES NOT PROVE: markers other than "`" (e.g. `compile` inside an
//! identifier), `python3 -c`, unquoted heredocs, or that a real marker
//! (`exec(s)`) with a non-literal write path stays refused.
//!
//! Fixed structurally in stage S5 (Python parser), not by a one-off patch.

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
        "bg-pymarkerfp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "python-heredoc-marker-fp-20261117",
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

fn decisions(cmds: &[&str], tag: &str) -> Vec<(bool, String, String, String)> {
    let home = unique_home(tag);
    let mut out = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            out.push((subagent, cmd.to_string(), d, r));
        }
    }
    out
}

const FP_SHAPES: &[&str] = &[
    // minimal: backtick in a plain literal, literal write path, later `.` word
    "python3 - <<'EOF'\ns = \"`x`\"\nopen('a.rs','w').write(s)\nEOF\n. \"$HOME/.cargo/env\"",
    // read-modify-write of the same literal path
    "python3 - <<'EOF'\ns = open('a.rs').read()\nopen('a.rs','w').write(s.replace('`x`','`y`'))\nEOF\n. \"$HOME/.cargo/env\"",
    // the observed tail shape
    "python3 - <<'EOF'\ns = \"`x`\"\nopen('a.rs','w').write(s)\nEOF\nwc -l a.rs; cd .. && . \"$HOME/.cargo/env\" && cargo fmt -p condukt",
];

#[test]
#[ignore = "RED: 2026-11-17 backtick in a Python string literal drops literal write paths (S5 python-parser) — remove when fixed"]
fn backtick_in_python_string_does_not_drop_literal_write_paths() {
    let bad: Vec<String> = decisions(FP_SHAPES, "red")
        .into_iter()
        .filter(|(_, _, d, _)| d == "ask" || d == "deny")
        .map(|(s, c, d, r)| format!("subagent={s} {c:?} => {d} ({r})"))
        .collect();
    assert!(
        bad.is_empty(),
        "expected not-Ask/not-Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_no_backtick_twin_is_allow() {
    let cmds =
        ["python3 - <<'EOF'\ns = \"x\"\nopen('a.rs','w').write(s)\nEOF\n. \"$HOME/.cargo/env\""];
    for (s, c, d, r) in decisions(&cmds, "ctl-allow") {
        assert_eq!(d, "allow", "subagent={s} {c:?} => {d} ({r})");
    }
}

#[test]
fn control_subprocess_run_of_written_harmful_file_stays_deny() {
    let cmds = ["python3 - <<'EOF'\nimport subprocess\nopen('f','w').write('rm -rf ~\\n')\nsubprocess.run(['bash','f'])\nEOF"];
    for (s, c, d, r) in decisions(&cmds, "ctl-subproc") {
        assert_eq!(d, "deny", "subagent={s} {c:?} => {d} ({r})");
    }
}

#[test]
fn control_backtick_text_written_then_run_by_bash_is_not_allow() {
    let cmds = ["python3 - <<'EOF'\ns = \"`x`\"\nopen('f','w').write(s)\nEOF\nbash f"];
    for (s, c, d, r) in decisions(&cmds, "ctl-run") {
        assert_ne!(d, "allow", "subagent={s} {c:?} => {d} ({r})");
    }
}
