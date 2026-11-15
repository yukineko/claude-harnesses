#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-15; worker-reported "contains a shell
//! expansion" denial, now reproduced): a QUOTED-delimiter heredoc body is data,
//! but when the body holds a backtick span containing `$` text AND a LATER command
//! in the same Bash call is one of rm / mv / cp / `git add` (not `ls`, not
//! `git log`), the body's backtick span is re-read as live shell and judged. Same
//! root as the two existing tests `backlog_20261113_commit_heredoc_backtick_fp.rs`
//! and `verify_e70b6a71_commit_heredoc_later_cmds.rs`, whose shapes pass; this
//! shape is the gap between them.
//!
//! Reproduced 2026-11-15 at main HEAD (blastguard 0.2.613, `git archive HEAD`
//! built into the scratchpad, scratch HOME, cwd=HOME/wt, binary run directly with
//! PreToolUse JSON on stdin, env -i). B(span) = `git commit -F - <<'EOF'\nfix: <span> here\nEOF\n`.
//! Observed (main / subagent):
//!   B(`rm "$f"`)   + `rm /tmp/x.tmp`   ask / deny  "this rm operand `\"$f\"` cannot be resolved
//!                                       to a path (variable or command substitution) ..."
//!   B(`rm "$f"`)   + `git add x`        ask / deny  (same)
//!   B(`rm "$f"`)   + `mv /tmp/a /tmp/b` ask / deny  (same);  + `cp /tmp/a /tmp/b && echo done`  same
//!   B(`rm "$f"`)   + `true; rm /tmp/x.tmp`  ask / deny (same)
//!   B(`$HOME`)     + `rm /tmp/x.tmp`    ask / deny  "the command word `$HOME` is an expansion whose
//!                                       value only exists at run time ..."
//!   B(`cat > "$f"`)+ `rm /tmp/x.tmp`    deny / deny "'> \"$f\"`' destroys the file's current contents
//!                                       and `\"$f\"`` contains a shell expansion, so blastguard cannot
//!                                       tell which file this names ..."   <- the worker's wording
//!   `cat <<'EOF'` (not even git) + span + `rm /tmp/x.tmp`   ask / deny (same as the first row)
//!   no later command, or later `ls /tmp` / `git log --oneline -1` / `git status --short` /
//!   `. "$HOME/.cargo/env" && cargo test ...` / `echo ok > /tmp/x`        allow / allow
//!   span written WITHOUT backticks (`rm "$f"` plain in the body) + `rm /tmp/x.tmp`  allow / allow
//!   `git commit -m 'fix: `rm "$f"` here'` (single-quoted -m) + `rm /tmp/x.tmp`        allow / allow
//! Later commands are all independently allowed by themselves (`rm /tmp/x.tmp` alone: allow).
//!
//! Two readings: (1) real defect: the quoted-delimiter body is data and is cut out
//! of the judged text only on some code paths; the rm/mv/cp/git-add analysis
//! re-lexes the original line text including the body, finds a backtick
//! substitution and judges `rm "$f"` / `> "$f"` that no shell ever runs;
//! (2) intended: `git commit -F -` stores the body in `.git/COMMIT_EDITMSG`, a later
//! command could execute it, so the body must stay in scope. Tell apart: the
//! verdict text names the SPAN CONTENT (`"$f"`), not a later command, and the
//! identical body with no later command, or with `ls`/`git log` later, is allow, as
//! is the body without backticks; and `cat <<'EOF'` stores nothing anywhere, so
//! reading (2) cannot apply to it. A later command that really executes the stored
//! message (`git log -1 --format=%B | sh`) is refused on its own ground (control).
//!
//! Asserted: for `git commit -F - <<'EOF'` with a backtick span `rm "$f"`, and
//! for `cat <<'EOF'` with the same body, followed by `rm /tmp/x.tmp`, main ==
//! "allow" and subagent == "allow". Controls (pass at HEAD, must stay != allow):
//! the same body with an UNQUOTED delimiter (the backtick really runs), a later
//! command that itself is `rm "$f"`, and a later `git log -1 --format=%B | sh`.
//! Base (passes at HEAD): same quoted body with a later `ls /tmp` is allow.
//!
//! DOES NOT PROVE: that all shapes of "span + later command" are allowed after a
//! fix (only the later `rm` is asserted; `mv`, `cp`, `git add x` are observed to
//! leak the same way but not asserted); that a span containing
//! `rm /etc/hosts` (observed deny/deny with later rm, same leak) should be allowed
//! — that is a system-harm string inside data and is left unasserted pending a
//! ruling; the residual that a LATER SEPARATE tool call executes
//! `.git/COMMIT_EDITMSG`; or the exact Ask/Deny split of the controls.
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
        "bg-hdspan-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt").join("run")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "hdspan-20261115",
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

/// `<opener> <<'EOF'` + a body line holding a backtick span `rm "$f"` + `EOF`,
/// then `rest` on the following lines.
fn heredoc(opener: &str, delim: &str, rest: &str) -> String {
    format!("{opener} <<{delim}\nfix: `rm \"$f\"` here\nEOF\n{rest}")
}

#[test]
#[ignore = "RED: 2026-11-15 quoted heredoc body backtick span with $ text is judged as live shell when a later rm/mv/cp/git-add follows — remove when fixed"]
fn quoted_heredoc_backtick_span_is_data_even_with_later_rm() {
    for opener in ["git commit -F -", "cat"] {
        let home = unique_home("fp");
        let cmd = heredoc(opener, "'EOF'", "rm /tmp/x.tmp");
        let (m, mr) = run_as(&cmd, &home, false);
        let (s, sr) = run_as(&cmd, &home, true);
        assert_eq!(
            m, "allow",
            "main: quoted-delimiter body is data ({opener}); reason={mr}"
        );
        assert_eq!(
            s, "allow",
            "subagent: quoted-delimiter body is data ({opener}); reason={sr}"
        );
    }
}

/// Base (passes at HEAD): the same body with a later command that does not trip
/// the leak is allowed — the shape the FP deviates from.
#[test]
fn control_base_same_body_with_later_ls_is_allowed() {
    let home = unique_home("base");
    let cmd = heredoc("git commit -F -", "'EOF'", "ls /tmp");
    let (m, mr) = run_as(&cmd, &home, false);
    assert_eq!(m, "allow", "base: {mr}");
}

/// Control (passes at HEAD): an UNQUOTED delimiter expands the backtick, so the
/// span really runs `rm "$f"` and must not be allowed.
#[test]
fn control_unquoted_delimiter_span_really_runs_is_not_allowed() {
    let home = unique_home("unq");
    let cmd = heredoc("git commit -F -", "EOF", "rm /tmp/x.tmp");
    let (m, mr) = run_as(&cmd, &home, false);
    let (s, sr) = run_as(&cmd, &home, true);
    assert_ne!(
        m, "allow",
        "unquoted heredoc executes the span; reason={mr}"
    );
    assert_ne!(
        s, "allow",
        "unquoted heredoc executes the span; reason={sr}"
    );
}

/// Control (passes at HEAD): a LATER command that itself deletes an unresolved
/// `$f` is refused whatever the heredoc held.
#[test]
fn control_later_command_with_unresolved_rm_operand_is_not_allowed() {
    let home = unique_home("later");
    let cmd = "git commit -F - <<'EOF'\nfix: ok\nEOF\nrm \"$f\"";
    let (m, mr) = run_as(cmd, &home, false);
    assert_ne!(m, "allow", "later rm \"$f\" is undetermined; reason={mr}");
}

/// Control (passes at HEAD): a later command that feeds the stored message to a
/// shell is refused on its own ground (stage piping into `sh` is not a literal emitter).
#[test]
fn control_later_command_running_stored_message_is_not_allowed() {
    let home = unique_home("sh");
    let cmd = "git commit -F - <<'EOF'\nfix: handle `foo`\nEOF\ngit log -1 --format=%B | sh";
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_ne!(m, "allow", "message piped into sh; reason={mr}");
    assert_ne!(s, "allow", "message piped into sh; reason={sr}");
}
