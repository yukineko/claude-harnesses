// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction of two heredoc false positives observed 2026-11-03 in
//! subagent sessions (where an Ask cannot be answered and so stalls the work).
//!
//! FP1 — `cd <abs> && git add <paths> && git commit -q -F - <<'EOF' … EOF`
//! was refused with:
//!
//! ```text
//! '> EOF' is a RELATIVE redirect after an earlier cd/pushd/popd whose target
//! directory blastguard could not statically resolve — it cannot tell whether
//! this lands in a system directory or on a protected gate/config path, and
//! refuses to guess
//! ```
//!
//! `<<'EOF'` is a here-document operator, not an output redirect; nothing is
//! written to a file named `EOF`. The `cd` target was an absolute literal path.
//!
//! FP2 — `cat > /abs/path/file.md <<'EOF'` whose body contains Markdown
//! backticks (`` `sitecraft decide purpose --text` ``) was refused with:
//!
//! ```text
//! the command word `decide` is an expansion whose value only exists at run
//! time — blastguard cannot tell what program this runs
//! ```
//!
//! With a quoted delimiter the shell performs no expansion of the body, so the
//! backticks are literal text, never command substitution.
//!
//! Expected verdict: whatever blastguard returns for the same command with the
//! heredoc replaced by a body-free equivalent (`git commit -q -m msg`, a plain
//! text body). At minimum the reason must not be the misparse.
//!
//! Controls pin what a fix must NOT give up:
//!   * an UNQUOTED heredoc whose body contains `$(rm -rf /)` is still treated
//!     as executing (not allowed);
//!   * a real relative `> out` redirect after an unresolvable `cd` is still
//!     refused with the relative-redirect reason.
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FP1_REASON: &str = "is a RELATIVE redirect after an earlier cd/pushd/popd";
const FP2_REASON: &str = "is an expansion whose value only exists at run time";

/// Fresh existing directory under the OS temp dir (used as cwd and cd target).
fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-heredoc-opener-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn verdict(cwd: &Path, command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.current_dir(cwd)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 (cmd: {command:?}); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let decision = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}, got: {stdout}"))
        .to_string();
    let reason = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (decision, reason)
}

fn esc(s: &str) -> String {
    s.replace('\n', "\\n")
}

/// FP1: the quoted heredoc feeding `git commit -F -` after an absolute `cd`
/// must be judged like the body-free `git commit -m` equivalent, and must
/// never carry the relative-redirect reason for `> EOF`.
#[test]
fn fp1_quoted_heredoc_after_absolute_cd_is_not_a_relative_redirect() {
    let base = fresh_dir("fp1-base");
    let target = fresh_dir("fp1-target");
    let t = target.to_string_lossy();
    // The message's last line ends in `>` (the commit trailer), so the body's
    // `>` + the `EOF` line tokenise as `> EOF` when the body is not removed.
    let msg = "fix: subject line\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>";
    // (label, heredoc form, body-free `-m` equivalent). Both shapes observed
    // to ask with FP1_REASON on 0.2.215 (2026-11-03).
    let cases = [
        (
            "quoted git add path",
            format!("cd {t} && git add 'a b.rs' && git commit -q -F - <<'EOF'\n{msg}\nEOF"),
            format!("cd {t} && git add 'a b.rs' && git commit -q -m 'fix: subject line'"),
        ),
        (
            "command after the closing EOF",
            format!("cd {t} && git add a.rs && git commit -q -F - <<'EOF'\n{msg}\nEOF\ngit log -1"),
            format!("cd {t} && git add a.rs && git commit -q -m 'fix: subject line'\ngit log -1"),
        ),
    ];
    let mut bad = Vec::new();
    for (label, heredoc, equivalent) in &cases {
        let (hd, hr) = verdict(&base, heredoc);
        let (ed, er) = verdict(&base, equivalent);
        if hr.contains(FP1_REASON) || hd != ed || hr != er {
            bad.push(format!(
                "[{label}] got {hd} (reason: {hr}) vs -m equivalent {ed} (reason: {er}) for {:?}",
                esc(heredoc)
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "FP1: quoted heredoc opener/body misread as relative redirect:\n{}",
        bad.join("\n")
    );
}

/// FP2 (live shape): a `python3 - <<'PY'` program whose string literal holds
/// Markdown backticks. Observed 2026-11-03 to ask with FP2_REASON on 0.2.215
/// (the subagent's own probe-file rewrite was denied with "the command word
/// ``sitecraft` is an expansion whose value only exists at run time").
/// Expected: judged like the same program with the backticks removed.
#[test]
fn fp2_backticks_in_quoted_python_heredoc_body_are_not_command_words() {
    let base = fresh_dir("fp2-py");
    let program = |text: &str| {
        format!(
            "python3 - <<'PY'\np='f.rs'\ns=open(p).read()\ns=s.replace('a','{text}')\nopen(p,'w').write(s)\nPY"
        )
    };
    let cmd = program("Run `sitecraft decide purpose --text` x");
    let equivalent = program("Run sitecraft decide purpose --text x");
    let (d, r) = verdict(&base, &cmd);
    let (ed, er) = verdict(&base, &equivalent);
    assert!(
        !r.contains(FP2_REASON) && d == ed && r == er,
        "FP2: quoted-heredoc body backticks treated as a command word: got {d} (reason: {r}) \
vs backtick-free equivalent {ed} (reason: {er}) for {:?}",
        esc(&cmd)
    );
}

/// FP2 (cat shape, as first reported): backticks inside a QUOTED heredoc body
/// are literal Markdown text; the write must be judged like the same write
/// with a backtick-free body.
#[test]
fn fp2_backticks_in_quoted_heredoc_body_are_not_command_words() {
    let base = fresh_dir("fp2-base");
    let out = base.join("notes").join("file.md");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let o = out.to_string_lossy();
    let bodies = [
        "# Notes\n\nRun `sitecraft decide purpose --text` to record the purpose.",
        "Use `sitecraft decide purpose --text \"x\"` then `sitecraft status`.",
        "- `sitecraft decide purpose --text`\n- `sitecraft decide audience --text`",
    ];
    let equivalent = format!("cat > {o} <<'EOF'\n# Notes\n\nplain text body\nEOF");
    let (ed, er) = verdict(&base, &equivalent);
    let mut bad = Vec::new();
    for b in bodies {
        let cmd = format!("cat > {o} <<'EOF'\n{b}\nEOF");
        let (d, r) = verdict(&base, &cmd);
        if r.contains(FP2_REASON) || d != ed || r != er {
            bad.push(format!(
                "got {d} (reason: {r}) vs equivalent {ed} (reason: {er}) for {:?}",
                esc(&cmd)
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "FP2: quoted-heredoc body backticks treated as command words:\n{}",
        bad.join("\n")
    );
}

/// Control: an UNQUOTED heredoc body IS expanded by the shell, so a
/// `$(rm -rf /)` in it really executes and must not be allowed.
#[test]
fn control_unquoted_heredoc_body_substitution_still_executes() {
    let base = fresh_dir("ctl-unquoted");
    let out = base.join("out.md");
    let o = out.to_string_lossy();
    for cmd in [
        format!("cat > {o} <<EOF\nhello $(rm -rf /)\nEOF"),
        format!("cat > {o} <<EOF\nhello `rm -rf /`\nEOF"),
    ] {
        let (d, r) = verdict(&base, &cmd);
        assert_ne!(d, "allow", "control: {:?} reason: {r}", esc(&cmd));
    }
}

/// Control: a real relative `> out` after a `cd` whose target is a run-time
/// expansion is still refused with the relative-redirect reason.
#[test]
fn control_real_relative_redirect_after_unresolvable_cd_is_refused() {
    let base = fresh_dir("ctl-relredir");
    let cmd = "cd \"$(some_tool --print-dir)\" && echo hi > out";
    let (d, r) = verdict(&base, cmd);
    assert_ne!(d, "allow", "control: {cmd:?} reason: {r}");
    assert!(
        r.contains(FP1_REASON),
        "control: expected relative-redirect reason, got {d} (reason: {r}) for {cmd:?}"
    );
}
