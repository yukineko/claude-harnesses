#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-20 in a subagent): a Bash call that
//! builds a Rust test file from a brace group `{ cat <<'EOF' … EOF; sed -n
//! '…' f | sed '$d'; … cat <<'EOF' … EOF; } > $F` was denied with
//!   "the command word ``--lib`/`--doc`` is an expansion whose value only exists
//!    at run time — blastguard cannot tell what program this runs"
//! (relay bgr-a866a4fd1397). The text `` `--lib`/`--doc` `` occurs only inside a
//! doc comment in the body of a QUOTED-delimiter heredoc (`<<'EOF'`), which is
//! data: no shell runs it.
//!
//! Fixture: tests/fixtures/backlog_20261120_quoted_heredoc_group_backtick_flag_fp.cmd
//! is the observed command with two edits: the leading `cd` target is the
//! placeholder `@WT@` (replaced by the scratch HOME's `wt`, so the `> $F`
//! target does not exist, as at observation time; with the real path the file
//! now exists and a different ground — truncation — fires), and the LAST
//! heredoc body is abbreviated to one line. The first heredoc body and the
//! three `sed` lines are verbatim.
//!
//! Observed at HEAD (blastguard 0.2.813): main = ask, subagent = deny, reason
//! as above. Bisection (same harness): the fixture with its three `sed` lines
//! removed is allow; joining the one backtick span that crosses a line break
//! still asks; small shapes (`cat <<'EOF'` + `` `--lib`/`--doc` `` body, with
//! or without a brace group, redirect, a `sed` holding a backticked pattern,
//! `| sed '$d'`) are all allow. The minimal trigger is NOT isolated: the
//! interaction needs the heredoc body together with the `sed` lines whose
//! single-quoted patterns hold backticks (`` `condukt testscope` ``,
//! `` `<run>.json` ``, `` `state init --run <run>` ``).
//!
//! Asserted (RED): the observed command is allow for main and subagent.
//! Controls (pass at HEAD): base — the same command without the `sed` lines is
//! allow; danger — the same command with every `<<'EOF'` unquoted (`<<EOF`, so
//! the backticks really run as command substitutions) is not allow.
//!
//! DOES NOT PROVE: which lexer step mispairs the backticks; that other
//! heredoc-plus-quoted-backtick shapes are fixed; the full-length last body.
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
        "bg-hdgrp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt").join("run")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "hdgrp-20261120",
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

const OBSERVED: &str =
    include_str!("fixtures/backlog_20261120_quoted_heredoc_group_backtick_flag_fp.cmd");

fn observed(home: &Home) -> String {
    OBSERVED.replace("@WT@", home.0.join("wt").to_str().unwrap())
}

#[test]
#[ignore = "backlog RED: quoted heredoc body backtick `--lib`/`--doc` judged as a command word expansion"]
fn observed_quoted_heredoc_group_command_is_allowed() {
    let home = unique_home("red");
    let cmd = observed(&home);
    let (m, mr) = run_as(&cmd, &home, false);
    let (s, sr) = run_as(&cmd, &home, true);
    assert_eq!(m, "allow", "main: quoted heredoc body is data; reason={mr}");
    assert_eq!(
        s, "allow",
        "subagent: quoted heredoc body is data; reason={sr}"
    );
}

#[test]
fn control_base_without_sed_lines_is_allowed() {
    let home = unique_home("base");
    let cmd: String = observed(&home)
        .lines()
        .filter(|l| !l.starts_with("sed "))
        .map(|l| format!("{l}\n"))
        .collect();
    let (m, mr) = run_as(&cmd, &home, false);
    assert_eq!(m, "allow", "base (no sed lines): {mr}");
}

#[test]
fn control_unquoted_delimiter_backticks_really_run_is_not_allowed() {
    let home = unique_home("unq");
    let cmd = observed(&home).replace("<<'EOF'", "<<EOF");
    assert_ne!(cmd, observed(&home), "fixture must contain <<'EOF'");
    let (m, mr) = run_as(&cmd, &home, false);
    let (s, sr) = run_as(&cmd, &home, true);
    assert_ne!(
        m, "allow",
        "unquoted heredoc runs the backticks; reason={mr}"
    );
    assert_ne!(
        s, "allow",
        "unquoted heredoc runs the backticks; reason={sr}"
    );
}
