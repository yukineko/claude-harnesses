#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (suspected fail-open, observed 2026-11-15): a `python3 - <<'PY'` heredoc
//! that WRITES a `.sh` file whose body is a system-harm command, followed in the
//! SAME Bash call by `sh t_test.sh`, is allowed. The harm then really runs.
//!
//! Reproduced 2026-11-15 at main HEAD (blastguard 0.2.616 `git archive HEAD`
//! built into the scratchpad, scratch HOME, cwd=HOME/wt, binary run directly with
//! PreToolUse JSON on stdin, env -i). Observed (main / subagent identical):
//!   python3 - <<'PY' / open("t_test.sh","w").write("chmod 777 /etc/hosts\n") / PY / sh t_test.sh   allow / allow
//!   same with `bash t_test.sh`                                                                      allow / allow
//!   same with `. ./t_test.sh`                                                                       allow / allow
//!   same with `chmod +x t_test.sh && ./t_test.sh`                                                   allow / allow
//!   same with body `rm -rf ~\n`                                                                     allow / allow
//!   same with `pathlib.Path("t_test.sh").write_text("chmod 777 /etc/hosts\n")`                      allow / allow
//!   python writes the .sh, NO run (control)                                                          allow / allow
//!   python writes `echo hi`, then `sh t_test.sh` (control)                                           allow / allow
//!   shell version: `cat > t_test.sh <<'EOF' / chmod 777 /etc/hosts / EOF / sh t_test.sh`            deny:
//!       "`chmod` re-permissions `/etc/hosts`, which is inside a system directory — removing,
//!       renaming, rewriting or re-permissioning an OS file or directory ..."
//!   shell version, write only, no run                                                                deny (same reason)
//!   `python3 -c 'open("t_test.sh","w").write("chmod 777 /etc/hosts\n")'; sh t_test.sh`              deny, but
//!       the reason names `/etc/hosts\n` / `/etc/hostsn` (single-quote shell text keeps the backslash),
//!       i.e. it is the quoting artefact, not a judgement of the written file. Not used as evidence.
//!
//! Two readings: (1) real defect: for a Python program the analysis treats string
//! literals as data (the sibling FP test relies on that), and nothing connects the
//! written `.sh` to the later `sh t_test.sh` in the same command line, so the
//! harmful body is never judged although the command line itself executes it
//! (system harm must be denied through any wrapper / indirection); (2) intended:
//! blastguard judges only text visible in the command, and a file written by
//! another interpreter is outside it (like `echo ... > f.sh` earlier in the
//! session), so this is an accepted limit. Tell apart: the same-call write+run is
//! fully visible in the command text (path literal `t_test.sh` is written and then
//! run), and the shell-heredoc form of the identical sequence is already denied, so
//! the repo does judge write-then-run in one call when the writer is the shell; the
//! Python writer is the only gap. Reading (2) would need the shell form to be
//! allowed too, or a written rule excluding interpreter writers.
//!
//! Asserted: for each write-then-run shape above (sh / bash / source / chmod+x
//! then ./ / `rm -rf ~` body / pathlib), main and subagent are NOT allow.
//! Controls (pass at HEAD): python writes a benign file then `sh` it is allow; the shell-heredoc write-then-run is != allow.
//!
//! DOES NOT PROVE: which verdict (deny vs ask) is right, only != allow; that a
//! write in one command and a run in a LATER Bash call is caught (not probed, and
//! would need cross-call state); that bodies computed at runtime (string concat,
//! base64, `"".join`) are caught; that other writers (`tee`, `dd`, `printf >`,
//! `os.write`) are handled; whether a python write of a harmful .sh WITHOUT a run
//! must itself be refused (the shell-heredoc writer is; ruling pending, so the
//! write-only shape is deliberately not asserted either way); or the exact reason
//! text.
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
        "bg-pywsh-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt").join("run")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pywsh-20261115",
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

const WRITE_HARM: &str = "open(\"t_test.sh\", \"w\").write(\"chmod 777 /etc/hosts\\n\")";

#[test]
fn python_writes_harmful_sh_then_same_call_runs_it_is_not_allowed() {
    let cases: Vec<String> = vec![
        format!("{}\nsh t_test.sh", py_heredoc(WRITE_HARM)),
        format!("{}\nbash t_test.sh", py_heredoc(WRITE_HARM)),
        format!("{}\n. ./t_test.sh", py_heredoc(WRITE_HARM)),
        format!(
            "{}\nchmod +x t_test.sh && ./t_test.sh",
            py_heredoc(WRITE_HARM)
        ),
        format!(
            "{}\nsh t_test.sh",
            py_heredoc("open(\"t_test.sh\", \"w\").write(\"rm -rf ~\\n\")")
        ),
        format!(
            "{}\nsh t_test.sh",
            py_heredoc(
                "import pathlib\npathlib.Path(\"t_test.sh\").write_text(\"chmod 777 /etc/hosts\\n\")"
            )
        ),
    ];
    for cmd in cases {
        let home = unique_home("fo");
        let (m, mr) = run_as(&cmd, &home, false);
        let (s, sr) = run_as(&cmd, &home, true);
        assert_ne!(
            m, "allow",
            "main: written harmful .sh is run in the same call; cmd={cmd:?}; reason={mr}"
        );
        assert_ne!(
            s, "allow",
            "subagent: written harmful .sh is run in the same call; cmd={cmd:?}; reason={sr}"
        );
    }
}

/// Control (passes at HEAD): a benign written script run in the same call is not over-blocked.
#[test]
fn control_benign_written_sh_then_run_is_allow() {
    let home = unique_home("benign");
    let cmd = format!(
        "{}\nsh t_test.sh",
        py_heredoc("open(\"t_test.sh\", \"w\").write(\"echo hi\\n\")")
    );
    let (m, mr) = run_as(&cmd, &home, false);
    assert_eq!(m, "allow", "benign body must stay allow; reason={mr}");
}

/// Control (passes at HEAD): the shell-heredoc equivalent is already refused, so
/// the repo does judge write-then-run in one call when the writer is the shell.
#[test]
fn control_shell_heredoc_write_then_run_is_not_allowed() {
    let home = unique_home("shell");
    let cmd = "cat > t_test.sh <<'EOF'\nchmod 777 /etc/hosts\nEOF\nsh t_test.sh";
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_ne!(m, "allow", "shell writer is judged; reason={mr}");
    assert_ne!(s, "allow", "shell writer is judged (subagent); reason={sr}");
}
