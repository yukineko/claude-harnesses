#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-15; worker-reported "python3 - <<'PY'
//! script that writes a .py test file whose content includes `chmod 777 /etc/hosts`
//! denied as system harm" — reproduced, but the trigger is NOT the following
//! `. "$HOME/.cargo/env"`): a plain Python STRING LITERAL whose text is a
//! `subprocess.run('chmod 777 /etc/hosts', shell=True)` call is judged as if it were
//! the program's own call. Nothing executes the string: it is written to a data
//! file, or only measured with `len()`.
//!
//! Reproduced 2026-11-15 at main HEAD (blastguard 0.2.613, `git archive HEAD`
//! built into the scratchpad, scratch HOME, cwd=HOME/wt, binary run directly with
//! PreToolUse JSON on stdin, env -i). Observed (main / subagent; subagent shows the
//! same verdicts):
//!   python3 - <<'PY' / with open("t_test.py","w") as f: f.write("subprocess.run('chmod 777 /etc/hosts', shell=True)\n") / PY
//!       deny: "`chmod` re-permissions `/etc/hosts`, which is inside a system directory —
//!       removing, renaming, rewriting or re-permissioning an OS file or directory ... can break
//!       the machine itself, so blastguard refuses it [system harm: refused on every request ..."
//!   same via `open("t_test.py","w").write(...)`, via `pathlib.Path(..).write_text(..)`, via
//!       `python3 -c 'open(...).write("subprocess.run(\"chmod 777 /etc/hosts\", shell=True)\n")'`   deny
//!   NO file write at all: `src = "subprocess.run('chmod 777 /etc/hosts', shell=True)\n"; print(len(src))`  deny
//!   sourcing `. "$HOME/.cargo/env"` after it or not changes nothing (deny with and without)
//!   string holds `chmod 777 /etc/hosts` only, or `os.chmod('/etc/hosts', 0o777)`, or
//!       `rm -rf /etc`, or a triple-quoted `def test_x(): cmd = "chmod 777 /etc/hosts"`   allow
//!   `import subprocess; subprocess.run("chmod 777 /etc/hosts", shell=True)` as real code   deny
//!   `import os; os.chmod("/etc/hosts", 0o777)` as real code                               deny
//! The `. "$HOME/.cargo/env"` reading in the report is therefore a misattribution:
//! the denial text names `chmod`/`/etc/hosts`, never the cargo env path.
//!
//! Two readings: (1) real defect: the Python analysis scans the TEXT of string
//! literals for the shape `subprocess.<fn>(... chmod ... /etc/...)` (raw-text
//! match, not an AST call), so a string that merely contains that text is judged
//! the same as a call; (2) intended: such a string might be `exec`'d later in the
//! same program or by a following command, so it must be judged. Tell apart:
//! reading (2) needs a path from the string to an interpreter. In the `len(src)`
//! program there is none (the program text is fully visible: no `exec`, `eval`,
//! `compile`, `-c`, `open(...).read`); and the sibling forms (`os.chmod('/etc/hosts',
//! ..)` as a string, `chmod 777 /etc/hosts` as a string) are already allowed, so
//! "string content is data" is the repo's own behaviour for them — only this one
//! pattern is inconsistent. The controls keep the real execution cases refused
//! (real `subprocess.run`, real `os.chmod`, `exec(src)`).
//!
//! Asserted: `src = "subprocess.run('chmod 777 /etc/hosts', shell=True)\n"; print(len(src))`
//! and the same string written to `t_test.py` with `open(...).write`, in main and
//! subagent, are allow. Controls (pass at HEAD; must stay != allow): the same
//! calls as real code, and `exec` of a string built that way.
//!
//! DOES NOT PROVE: that a string followed by a LATER command that runs the written
//! file (`python3 t_test.py`, `python3 -m pytest t_test.py`) is allowed — at HEAD
//! those are deny and the controls below only assert they stay != allow (they
//! currently deny BECAUSE of the leak, so after a fix they might flip to allow if the
//! file is unreadable; that is a separate ruling); that other string shapes
//! (`os.system('chmod ...')`, `shutil.rmtree('/etc')` inside a string) are handled
//! (not probed); or the exact reason text.
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
        "bg-pystr-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt").join("run")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pystr-20261115",
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

fn py_heredoc(body: &str) -> String {
    format!("python3 - <<'PY'\n{body}\nPY")
}

const STR_CALL: &str = "subprocess.run('chmod 777 /etc/hosts', shell=True)";

#[test]
#[ignore = "RED: 2026-11-15 python string literal containing subprocess.run('chmod 777 /etc/hosts', ..) is denied as system harm though only written to a data file / measured — remove when fixed"]
fn python_string_literal_holding_subprocess_chmod_call_is_data() {
    let programs = [
        // no file write at all
        format!("src = \"{STR_CALL}\\n\"\nprint(len(src))"),
        // written to a data file (the worker's shape)
        format!("open(\"t_test.py\", \"w\").write(\"{STR_CALL}\\n\")"),
        format!("with open(\"t_test.py\", \"w\") as f:\n    f.write(\"{STR_CALL}\\n\")"),
    ];
    for body in programs {
        let home = unique_home("fp");
        let cmd = py_heredoc(&body);
        let (m, mr) = run_as(&cmd, &home, false);
        let (s, sr) = run_as(&cmd, &home, true);
        assert_eq!(
            m, "allow",
            "main: string literal is data; cmd={cmd:?}; reason={mr}"
        );
        assert_eq!(
            s, "allow",
            "subagent: string literal is data; cmd={cmd:?}; reason={sr}"
        );
    }
}

/// Base (passes at HEAD): the sibling strings are already data.
#[test]
fn control_sibling_string_shapes_are_allowed() {
    for body in [
        "open(\"t_test.py\", \"w\").write(\"chmod 777 /etc/hosts\\n\")",
        "open(\"t_test.py\", \"w\").write(\"os.chmod('/etc/hosts', 0o777)\\n\")",
    ] {
        let home = unique_home("base");
        let cmd = py_heredoc(body);
        let (m, mr) = run_as(&cmd, &home, false);
        assert_eq!(
            m, "allow",
            "sibling string is data today; cmd={cmd:?}; reason={mr}"
        );
    }
}

/// Controls (pass at HEAD): the same operations as REAL code, or run via `exec`,
/// are system harm / undetermined and must stay refused.
#[test]
fn control_real_calls_and_exec_of_the_string_are_not_allowed() {
    for body in [
        "import subprocess\nsubprocess.run('chmod 777 /etc/hosts', shell=True)".to_string(),
        "import os\nos.chmod('/etc/hosts', 0o777)".to_string(),
        format!("src = \"import subprocess; {STR_CALL}\"\nexec(src)"),
    ] {
        let home = unique_home("ctl");
        let cmd = py_heredoc(&body);
        let (m, mr) = run_as(&cmd, &home, false);
        let (s, sr) = run_as(&cmd, &home, true);
        assert_ne!(m, "allow", "real execution; cmd={cmd:?}; reason={mr}");
        assert_ne!(
            s, "allow",
            "real execution (subagent); cmd={cmd:?}; reason={sr}"
        );
    }
}

/// Control (passes at HEAD, deny today): writing the harmful script to a file and
/// then running it in the same command must not be allowed.
#[test]
fn control_write_then_run_the_written_file_is_not_allowed() {
    let home = unique_home("run");
    let cmd = format!(
        "{}\npython3 t_test.py",
        py_heredoc(&format!(
            "open(\"t_test.py\", \"w\").write(\"import subprocess\\n{STR_CALL}\\n\")"
        ))
    );
    let (m, mr) = run_as(&cmd, &home, false);
    assert_ne!(m, "allow", "written file is then executed; reason={mr}");
}
