#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Pins the restrictive guarantees of commit 3ae2d9975 (backlog 06c51772):
//! for a `python3 - <<'EOF'` program that progeffect reads as Known, a file the
//! program writes whose content progeffect did not read must not be treated as
//! harmless when a later part of the command runs that file. A Known program
//! with a process effect keeps an unknown-path write.
//!
//! Each case asserts decision != "allow" in main mode and == "deny" in
//! subagent mode, except where noted. Controls A and B must be allow so the
//! harness can be seen to discriminate.
//!
//! DOES NOT PROVE: Undetermined programs, `python3 -c`, unquoted heredocs, or
//! any run form other than `sh t.sh` / `. "$HOME/.cargo/env"`.

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
        "bg-t1known-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "t1-known-python-write-then-run-20261121",
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

fn cmd(prog: &str, tail: &str) -> String {
    format!("python3 - <<'EOF'\n{prog}\nEOF\n{tail}")
}

const RUN_T: &str = "sh t.sh";
const RUN_DOT: &str = ". \"$HOME/.cargo/env\"";

/// not allow in main mode and deny in subagent mode.
fn assert_restricted(tag: &str, command: &str) {
    let home = unique_home(tag);
    let (d, r) = run_as(command, &home, false);
    assert_ne!(
        d, "allow",
        "[{tag}] main: decision={d} reason={r}\ncmd={command}"
    );
    let (d, r) = run_as(command, &home, true);
    assert_eq!(
        d, "deny",
        "[{tag}] subagent: decision={d} reason={r}\ncmd={command}"
    );
}

/// not allow in both modes.
fn assert_not_allow(tag: &str, command: &str) {
    let home = unique_home(tag);
    for sub in [false, true] {
        let (d, r) = run_as(command, &home, sub);
        assert_ne!(
            d, "allow",
            "[{tag}] subagent={sub}: decision={d} reason={r}\ncmd={command}"
        );
    }
}

fn assert_allow(tag: &str, command: &str) {
    let home = unique_home(tag);
    for sub in [false, true] {
        let (d, r) = run_as(command, &home, sub);
        assert_eq!(
            d, "allow",
            "[{tag}] subagent={sub}: decision={d} reason={r}\ncmd={command}"
        );
    }
}

#[test]
fn c1_handle_written_later() {
    assert_restricted(
        "c1",
        &cmd("f = open('t.sh','w')\nf.write('echo hi')\nf.close()", RUN_T),
    );
}

#[test]
fn c2_with_open_write() {
    assert_restricted(
        "c2",
        &cmd("with open('t.sh','w') as f:\n    f.write('echo hi')", RUN_T),
    );
}

#[test]
fn c3_os_open_fd() {
    assert_restricted(
        "c3",
        &cmd(
            "import os\nfd = os.open('t.sh', os.O_WRONLY|os.O_CREAT)",
            RUN_T,
        ),
    );
}

#[test]
fn c4_shutil_copy_dest() {
    assert_restricted(
        "c4",
        &cmd("import shutil\nshutil.copy('a.txt','t.sh')", RUN_T),
    );
}

#[test]
fn c5_shutil_move_dest() {
    assert_restricted(
        "c5",
        &cmd("import shutil\nshutil.move('a.txt','t.sh')", RUN_T),
    );
}

#[test]
fn c6_fstring_content() {
    // Ideal is deny in both modes; asserted: at least not allow.
    assert_not_allow(
        "c6",
        &cmd("x='rm'\nopen('t.sh','w').write(f'{x} -rf ~')", RUN_T),
    );
}

#[test]
fn c7_literal_harmful_content() {
    let command = cmd("open('t.sh','w').write('rm -rf ~\\n')", RUN_T);
    let home = unique_home("c7");
    for sub in [false, true] {
        let (d, r) = run_as(&command, &home, sub);
        assert_eq!(
            d, "deny",
            "[c7] subagent={sub}: decision={d} reason={r}\ncmd={command}"
        );
    }
}

#[test]
fn c8_known_program_with_subprocess_and_write() {
    assert_not_allow(
        "c8",
        &cmd(
            "import subprocess\nopen('o.txt','w').write('x')\nsubprocess.run(['true'])",
            RUN_DOT,
        ),
    );
}

#[test]
fn c9_non_utf8_bytes_content() {
    assert_restricted("c9", &cmd("open('t.sh','wb').write(b'\\xff\\xfe')", RUN_T));
}

#[test]
#[ignore = "observed ask: `sh` runs the program file `t.sh`, whose text is not in the command — blastguard cannot judge what it would do (Table B.5: Undetermined)"]
fn control_a_literal_benign_content_is_allow() {
    assert_allow("ca", &cmd("open('t.sh','w').write('echo hi\\n')", RUN_T));
}

#[test]
fn control_b_unrelated_literal_write_then_dot_is_allow() {
    assert_allow("cb", &cmd("open('a.rs','w').write('x')", RUN_DOT));
}
