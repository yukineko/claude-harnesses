#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-17; relay bgr-d2f040bc9abe, subagent
//! deny / main ask).
//!
//! Shape: `cd <literal dir> && python3 /dev/stdin <<'EOF'` with a Python program
//! that (a) holds `>=1,` in ordinary Python code (`assert s.count(old)>=1, old`)
//! and (b) holds a MULTI-LINE triple-quoted string whose text contains `<&str,`
//! (Rust source being substituted). The QUOTED delimiter means the shell never
//! expands the body; the body is Python program text, not shell. Verbatim reason
//! (main ask / subagent deny):
//!   '> =1,' is a RELATIVE redirect after an earlier cd/pushd/popd whose target
//!   directory blastguard could not statically resolve — …
//!
//! Measured 2026-11-17 against blastguard built from main (0.2.679 and 0.2.681,
//! scratch HOME, cwd HOME/wt, PreToolUse JSON on stdin, env cleared), main/sub:
//!   cd D && python3 /dev/stdin, body `assert a>=1, b` + `x='''`/`Option<&str>,`/`'''`  ask / deny
//!   same with `"""` instead of `'''`                                             ask / deny
//!   same, triple-quoted string BEFORE the assert line                           ask / deny
//!   no `cd` (or `true &&` in place of `cd D &&`)                                allow / allow
//!   no `>=1` line                                                               allow / allow
//!   `<&str,` in a single-line string, or as a bare line                         allow / allow
//!   triple-quoted string holding `foo`, `<&`, `a & b`, or `a < b`               allow / allow
//!   `cat > n.txt` (or `cat > x.py`) with the same body                          allow / allow
//! The literal `cd` target is a real directory; the "could not statically
//! resolve" wording is therefore about the body being re-read as shell, not about
//! the cd itself.
//!
//! Readings: (a) defect — the python program text is re-read as shell (the
//! redirect scan sees `>=1,` and the body's `<&` quoting state makes the earlier
//! `cd` look unresolved); (b) intended fail-closed for python stdin programs —
//! but the same body WITHOUT `<&str,` is Allow today, so the Ask is not the
//! python-stdin policy, it is an artefact of the text reading.
//! Structural fix: S3 judges redirects from the AST (a quoted heredoc body has no
//! redirects) and S6 retires the string re-reading; until S5 (Python parser) the
//! SUBSET.md B.5 python row is Undetermined → Ask, so this goes GREEN only after
//! S5 + S6.
//!
//! DOES NOT PROVE: which code path re-reads the body (suspected, UNVERIFIED:
//! `detect.rs` redirect scan over `redirect_view_without_quoted_here_document_body`,
//! which strips quoted bodies only for data readers, and `segment_cwd_states`);
//! anything about the python program's real effects (it only reads/writes
//! files in the cwd); the exact Ask/Deny split of the controls beyond "not
//! allow".
//!
//! RED written by the triage agent, which did not write the blastguard code under
//! test; `src/` untouched.

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
        "bg-pytq-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt").join("sub")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "pytq-20261117",
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

/// `cd <HOME>/wt/sub && python3 /dev/stdin <<'EOF'` + `body` + `EOF`.
fn command(home: &Home, body: &str) -> String {
    let dir = home.0.join("wt").join("sub");
    format!(
        "cd {} && python3 /dev/stdin <<'EOF'\n{body}\nEOF",
        dir.display()
    )
}

const FP_BODY: &str = "assert a>=1, b\nx='''\nOption<&str>,\n'''";

#[test]
#[ignore = "RED: 2026-11-17 (S6; needs S5) python stdin body with `>=1,` and a triple-quoted `<&str,` after a literal cd is read as a relative redirect — remove when fixed"]
fn python_stdin_program_text_is_not_a_shell_redirect() {
    let home = unique_home("fp");
    let cmd = command(&home, FP_BODY);
    for subagent in [false, true] {
        let (d, r) = run_as(&cmd, &home, subagent);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: a quoted python stdin program's text is not shell; reason={r}"
        );
    }
}

#[test]
fn control_same_body_without_angle_amp_is_allowed() {
    let home = unique_home("c1");
    let cmd = command(&home, &FP_BODY.replace("<&str>", "<str>"));
    for subagent in [false, true] {
        let (d, r) = run_as(&cmd, &home, subagent);
        assert_eq!(d, "allow", "subagent={subagent}: reason={r}");
    }
}

#[test]
fn control_same_shape_writing_a_system_file_stays_denied() {
    let home = unique_home("c2");
    let body = format!("{FP_BODY}\nopen('/etc/passwd','w').write('x')");
    let cmd = command(&home, &body);
    for subagent in [false, true] {
        let (d, r) = run_as(&cmd, &home, subagent);
        assert_eq!(d, "deny", "subagent={subagent}: reason={r}");
    }
}

#[test]
fn control_same_shape_removing_a_system_tree_stays_denied() {
    let home = unique_home("c3");
    let body = format!("import shutil\n{FP_BODY}\nshutil.rmtree('/usr')");
    let cmd = command(&home, &body);
    for subagent in [false, true] {
        let (d, r) = run_as(&cmd, &home, subagent);
        assert_eq!(d, "deny", "subagent={subagent}: reason={r}");
    }
}

#[test]
fn control_real_relative_redirect_after_unresolvable_cd_is_not_allowed() {
    let home = unique_home("c4");
    let cmd = "cd \"$D\" && echo x > a.txt";
    let (d, r) = run_as(cmd, &home, false);
    assert_ne!(d, "allow", "reason={r}");
}
