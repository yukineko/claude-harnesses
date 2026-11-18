#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial boundaries for the printf data-argument fix (blastguard 0.2.732,
//! e313e9ad4, `statement_may_run` + `python_program(args)` in
//! `detect/echo_script_file.rs`).
//!
//! The fix stops judging a printf-written literal file as the Python program
//! unless python's program operand is that file, or the program is stdin /
//! undetermined; `-c` counts as "no program file". The risk probed here: a
//! `-c` body (or a module such as `runpy`) can read `sys.argv[1]` and execute
//! it, which makes the "data" file the real program. Every harmful shape below
//! writes a harmful Python program into `d.txt` and then makes Python execute
//! it by some route; none of them may be Allow, in main or subagent mode.
//!
//! The only shape allowed to be Allow is the benign `python3 -c 'print(1)'
//! d.txt`, the lowering the implementer claims correct; its current verdict is
//! pinned (see the test's comment).
//!
//! Observed at ddc2a3b05 (0.2.732): the `subprocess.run([sys.executable,
//! sys.argv[1]])` and `os.execv(...)` bodies are Allow in both modes
//! (FAIL-OPEN, `#[ignore]`d RED); at the pre-fix rev fb4080bb6 they were
//! system-harm Deny. The `exec(`/`runpy.`/`getattr(` bodies are not Allow only
//! because the generic inline-eval detector Asks, not because the fix sees the
//! data file as the program.
//!
//! DOES NOT PROVE: other interpreters (node/ruby/perl), `cat`/`echo`/`tee`
//! writers, non-literal written paths, or `;`/newline joins (only `&&` and
//! pipes/redirects are exercised).
//!
//! Written by an independent test author, not the implementer.

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
        "bg-pfpyb-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "printf-python-data-arg-boundaries-20261119",
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

/// The harmful writer every shape starts with.
const W: &str = "printf 'import os\\nos.system(\"rm -rf ~\")\\n' > d.txt";

fn assert_not_allow(tag: &str, run: &str) {
    let home = unique_home(tag);
    let shape = format!("{W} {run}");
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = run_as(&shape, &home, subagent);
        eprintln!("VERDICT {tag} subagent={subagent}: {d} | {r}");
        if d == "allow" {
            bad.push(format!("subagent={subagent} {shape:?} => allow ({r})"));
        }
    }
    assert!(bad.is_empty(), "FAIL-OPEN:\n{}", bad.join("\n"));
}

#[test]
fn c_body_execs_open_argv1_is_not_allow() {
    assert_not_allow(
        "exec-open",
        "&& python3 -c 'exec(open(__import__(\"sys\").argv[1]).read())' d.txt",
    );
}

#[test]
fn c_body_runpy_run_path_argv1_is_not_allow() {
    assert_not_allow(
        "runpy-path",
        "&& python3 -c 'import runpy,sys; runpy.run_path(sys.argv[1])' d.txt",
    );
}

#[test]
fn c_body_exec_compile_argv1_is_not_allow() {
    assert_not_allow(
        "exec-compile",
        "&& python3 -c 'import sys; exec(compile(open(sys.argv[1]).read(),\"x\",\"exec\"))' d.txt",
    );
}

#[test]
fn m_runpy_with_written_file_is_not_allow() {
    assert_not_allow("m-runpy", "&& python3 -m runpy d.txt");
}

#[test]
fn c_body_is_command_substitution_of_written_file_is_not_allow() {
    assert_not_allow("c-subst", "&& python3 -c \"$(cat d.txt)\"");
}

#[test]
fn stdin_redirect_of_written_file_is_not_allow() {
    assert_not_allow("stdin-redir", "&& python3 - < d.txt");
}

#[test]
fn cat_written_file_piped_to_python_is_not_allow() {
    assert_not_allow("cat-pipe", "&& cat d.txt | python3");
}

// Implementer's claimed-correct lowering (e313e9ad4): `-c` names no program
// file, the body `print(1)` does not read argv, so `d.txt` is pure data and
// the printf text is not judged as Python. Pinned at the verdict observed in
// this worktree at ddc2a3b05 (blastguard 0.2.732); before the fix this shape
// was refused. If this changes, re-check against the fail-open cases above.
#[test]
fn benign_c_print_with_written_data_file_is_pinned() {
    let home = unique_home("benign");
    let shape = "printf '%s\\n' a b > d.txt && python3 -c 'print(1)' d.txt";
    for subagent in [false, true] {
        let (d, r) = run_as(shape, &home, subagent);
        eprintln!("VERDICT benign subagent={subagent}: {d} | {r}");
        assert_eq!(d, "allow", "{shape:?} (subagent={subagent}): {r}");
    }
}

// Extra probes beyond the required list: `-c` bodies that hand argv[1] to a
// fresh interpreter (or obfuscate `exec`) without the literal `exec(` /
// `runpy.` markers the inline-eval detector keys on.

#[test]
#[ignore = "RED: fail-open: printf-written harmful d.txt run via `python3 -c subprocess.run([sys.executable, sys.argv[1]]) d.txt` is allow/allow at 0.2.732 (deny/deny at fb4080bb6, pre-fix)"]
fn c_body_subprocess_runs_argv1_with_sys_executable_is_not_allow() {
    assert_not_allow(
        "subproc",
        "&& python3 -c 'import sys,subprocess; subprocess.run([sys.executable, sys.argv[1]])' d.txt",
    );
}

#[test]
#[ignore = "RED: fail-open: printf-written harmful d.txt run via `python3 -c os.execv(sys.executable,[..., sys.argv[1]]) d.txt` is allow/allow at 0.2.732 (deny/deny at fb4080bb6, pre-fix)"]
fn c_body_os_execv_argv1_is_not_allow() {
    assert_not_allow(
        "execv",
        "&& python3 -c 'import os,sys; os.execv(sys.executable,[sys.executable,sys.argv[1]])' d.txt",
    );
}

#[test]
fn c_body_obfuscated_exec_of_argv1_is_not_allow() {
    assert_not_allow(
        "obf-exec",
        "&& python3 -c 'import sys,builtins; getattr(builtins,\"ex\"+\"ec\")(open(sys.argv[1]).read())' d.txt",
    );
}

// System harm stays Deny through any wrapper. Before the fix (fb4080bb6) every
// shape below was deny/deny with the reason "recursive rm (-r) can delete an
// entire directory tree [system harm: ...]" because the written text was
// judged as the program; at 0.2.732 the argv-exec `-c` shapes fall to the
// generic inline-eval Ask in main mode (or to Allow, see the two ignored
// fail-open tests above). Observed via the VERDICT lines of `assert_not_allow`
// run against both revisions.
#[test]
#[ignore = "RED: system-harm Deny lowered to Ask/Allow for -c bodies that run argv[1] (0.2.732; deny/deny at fb4080bb6)"]
fn c_body_running_written_harmful_file_stays_system_harm_deny() {
    let home = unique_home("sysharm");
    let mut bad = Vec::new();
    for run in [
        "&& python3 -c 'exec(open(__import__(\"sys\").argv[1]).read())' d.txt",
        "&& python3 -c 'import runpy,sys; runpy.run_path(sys.argv[1])' d.txt",
        "&& python3 -c 'import sys; exec(compile(open(sys.argv[1]).read(),\"x\",\"exec\"))' d.txt",
        "&& python3 -c 'import sys,subprocess; subprocess.run([sys.executable, sys.argv[1]])' d.txt",
        "&& python3 -c 'import os,sys; os.execv(sys.executable,[sys.executable,sys.argv[1]])' d.txt",
    ] {
        let shape = format!("{W} {run}");
        for subagent in [false, true] {
            let (d, r) = run_as(&shape, &home, subagent);
            if d != "deny" || !r.contains("system harm") {
                bad.push(format!("subagent={subagent} {shape:?} => {d} ({r})"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
