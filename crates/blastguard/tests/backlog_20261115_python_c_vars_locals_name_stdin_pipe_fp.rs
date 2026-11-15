#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, NEW with fix 9d0b60188 `python_file.rs:608/:658`, observed
//! 2026-11-15): a literal `echo '<program text>' | python3 -c '<code that never
//! reads stdin>'` is judged AS IF the piped text were the program that runs. When
//! the piped text holds a system-harm operation, the verdict is deny/deny even
//! though the `-c` program is `print(vars)` / `print(locals())` / `print(globals())`
//! and never reads stdin, so the piped text is only ever data.
//!
//! Reproduced 2026-11-15 at main HEAD (blastguard 0.2.613, `git archive HEAD`
//! built into the scratchpad, scratch HOME, cwd=HOME/wt, binary run directly with
//! PreToolUse JSON on stdin, env -i). P = `echo 'import os; os.chmod("/etc/hosts",
//! 0o777)' | python3 -c '<X>'`. Observed (main / subagent):
//!   X=`print(vars)`                      deny / deny  "the program calls `os.chmod` on
//!                                        `/etc/hosts`, a file in a system directory ..."
//!   X=`print(locals())`                  deny / deny  (same reason)
//!   X=`print(globals())`                 deny / deny  (same reason; not in the suspect list)
//!   X=`print(1)`                         allow / allow  (base)
//!   X=`g=globals; g()["exec"](input())`  deny / deny  (same reason)  <- must stay
//!   X=`v=vars; v()["exec"](input())`     deny / deny
//!   X=`l=locals; l()["exec"](input())`   deny / deny
//!   X=`exec(input())`                    deny / deny
//!   harmless piped text `echo 'print(1)'` + `print(globals())`   allow / allow
//! So the verdict flips on whether the piped text is harmful, not on anything the
//! `-c` program does with it.
//!
//! Two readings: (1) real defect: the new stdin-exec detector treats the mere
//! appearance of the builtin NAME `vars`/`locals`/`globals` (as a bare name or as a
//! call) as "this program may exec what it reads from stdin" and therefore
//! executes-in-the-mind the piped text; (2) intended: any `-c` program that names
//! those builtins can reach `exec` via `g()["exec"]`, so the piped text must be
//! judged. Tell apart: reading (2) is only a reason if the program can also obtain
//! the text. `print(vars)` neither calls `vars`, nor indexes anything, nor mentions
//! `exec`/`eval`/`input`/`sys.stdin`/`open`, so it has no path from stdin to an
//! interpreter (the repo's rule: "permissive path not found" is UNDETERMINED, but
//! here the program text is fully visible and contains no reader of stdin at all).
//! The control programs, which DO combine the builtin with `exec(input())`, stay
//! deny at HEAD and are asserted != allow, so a fix that simply stops looking at
//! the builtins names would fail them.
//!
//! Asserted: for the three `-c` programs above with a system-harm piped text,
//! main != deny and != ask (i.e. allow) and subagent == allow. Controls (must stay
//! != allow in both modes): the three alias-then-exec forms and plain `exec(input())`.
//!
//! DOES NOT PROVE: that every `-c` program mentioning `vars`/`locals`/`globals` is
//! safe (only the three exact programs above); that the piped text is never
//! executed by some OTHER mechanism for these programs (e.g. `python3 -c` with
//! `-i` or `PYTHONSTARTUP` are not probed); the exact reason text or whether the
//! controls would be Ask or Deny after a fix (only != allow); that the base
//! `print(1)` allow is correct for a program reading `sys.stdin` (not probed here).
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
        "bg-pyvars-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt").join("run")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pyvars-20261115",
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

/// `echo '<harmful text>' | python3 -c '<prog>'`; the piped text is system harm
/// IF it were executed.
fn piped(prog: &str) -> String {
    format!("echo 'import os; os.chmod(\"/etc/hosts\", 0o777)' | python3 -c '{prog}'")
}

#[test]
#[ignore = "RED: 2026-11-15 python3 -c 'print(vars)' / print(locals()) / print(globals()) never read stdin but the harmful echo'd text is judged as the program (deny/deny) — remove when fixed"]
fn python_c_naming_vars_or_locals_without_reading_stdin_does_not_judge_piped_text() {
    for prog in ["print(vars)", "print(locals())", "print(globals())"] {
        let home = unique_home("fp");
        let cmd = piped(prog);
        let (m, mr) = run_as(&cmd, &home, false);
        let (s, sr) = run_as(&cmd, &home, true);
        assert_eq!(
            m, "allow",
            "main: `-c {prog}` never reads stdin, the piped text is data; reason={mr}"
        );
        assert_eq!(
            s, "allow",
            "subagent: `-c {prog}` never reads stdin, the piped text is data; reason={sr}"
        );
    }
}

/// Control (passes at HEAD): the same piped text with a `-c` program that does
/// nothing with the builtins is allowed — the base the FP deviates from.
#[test]
fn control_base_program_without_builtin_names_is_allowed() {
    let home = unique_home("base");
    let cmd = piped("print(1)");
    let (m, mr) = run_as(&cmd, &home, false);
    let (s, sr) = run_as(&cmd, &home, true);
    assert_eq!(m, "allow", "base print(1): {mr}");
    assert_eq!(s, "allow", "base print(1) (subagent): {sr}");
}

/// Controls (pass at HEAD, must keep passing after a fix): the program DOES hand
/// stdin to `exec`, directly or through an alias of `globals`/`vars`/`locals`,
/// so the piped system-harm text really would run.
#[test]
fn control_exec_of_stdin_through_builtin_alias_is_not_allowed() {
    for prog in [
        "g=globals; g()[\"exec\"](input())",
        "v=vars; v()[\"exec\"](input())",
        "l=locals; l()[\"exec\"](input())",
        "exec(input())",
    ] {
        let home = unique_home("ctl");
        let cmd = piped(prog);
        let (m, mr) = run_as(&cmd, &home, false);
        let (s, sr) = run_as(&cmd, &home, true);
        assert_ne!(m, "allow", "main: `-c {prog}` executes stdin; reason={mr}");
        assert_ne!(
            s, "allow",
            "subagent: `-c {prog}` executes stdin; reason={sr}"
        );
    }
}
