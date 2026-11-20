#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (possible false positive, observed 2026-11-20, subagent mode, blastguard
//! 0.2.809): `cd <dir> && backlog add --title "... '> \$fd' ..." --notes "..."
//! 2>&1 | tail -30; ...` was refused: "'> \$fd' is a RELATIVE redirect after an
//! earlier cd/pushd/popd whose target directory blastguard could not statically
//! resolve ...". The `> \$fd` text sits inside a double-quoted argument of
//! `backlog add`; no redirect is there.
//!
//! Two readings: (a) real defect: text inside a quoted argument is not a
//! redirect and must not be judged as one; (b) intended: blastguard cannot see
//! whether the callee later re-parses the argument as shell, so it is
//! conservative. Tell apart: the same command with the `>` text removed gets
//! verdict V (control). If the `>` text version is stricter than V, the quoted
//! text alone raised it (reading a).
//!
//! Asserted: verdict(command with quoted `>` text) is no stricter than verdict
//! (same command without it), for the exact observed command and a minimal
//! reduction, in subagent and main mode.
//!
//! Measured: the exact observed command reproduces (control allow, with-text
//! deny/ask); the one-line MINIMAL reduction does NOT (allow both ways), so its
//! tests are passing controls and the trigger inside the long command is not
//! yet isolated.
//!
//! DOES NOT PROVE: what `backlog add` does with its arguments; `cd` forms other
//! than the literal ones used. Control: a real relative redirect after an
//! unresolvable cd stays non-allow.
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
        "bg-side1120q-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261120q",
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

fn rank(d: &str) -> u8 {
    match d {
        "allow" => 0,
        "ask" => 1,
        _ => 2,
    }
}

/// The exact command attempted when the refusal was observed (verbatim).
const OBSERVED: &str = r####"cd /Users/yuki/.condukt/worktrees/run-20261120-171414-35880-whileread && backlog add --project "$PWD" --title "blastguard FP: heredoc fed to while-read loop (data) denied as '> \$fd' redirect" --tag blastguard --tag false-positive --notes "Observed 2026-11-20 (flow session, blastguard 0.2.798): a while IFS='|' read -r ...; do ...; done <<'EOF' loop whose heredoc DATA lines contained 'echo hi >&\$fd' was Denied verbatim: \"'> \$fd' destroys the file's current contents and \`\$fd\` contains a shell expansion, so blastguard cannot tell which file this names, let alone whether its contents are recoverable\". Measured at rev bf046b953 (parent of 0b4348ae4) on 2026-11-20: same loop with harmless data = allow; with data 'x|echo hi >&\$fd / y|probe 3<<< x' = deny (main and subagent). Controls pass: eval-body loop = deny (system harm), 'bash <<EOF echo hi >&\$fd' = deny. Readings: (a) FP: a heredoc that is stdin to read is data, the body only echoes it, so the text must not be judged; (b) intended: blastguard cannot see whether the loop body later executes the data (eval, sh -c). Tell apart: compare with harmless-data control (allow); if body only echoes/reads variables the data is inert (a), if body eval/sh -c/source's the variable it is executed (b, stays deny)." --repro-test "cargo test -p blastguard --test backlog_20261120_while_read_data_heredoc_fd_redirect_fp -- --include-ignored" 2>&1 | tail -30; echo ---; git status --short; git log -3 --oneline; git -C /Users/yuki/src/harness log -3 --oneline; git -C /Users/yuki/src/harness log -1 --oneline origin/main"####;

const MINIMAL: &str = r#"cd "$PWD" && backlog add --title "x '> \$fd' y""#;

/// Same command with the quoted `>` text removed (the real `2>&1` stays).
fn without_gt_text(c: &str) -> String {
    c.replace("> \\$fd", "fd").replace(">&\\$fd", "fd")
}

fn check(tag: &str, command: &str, subagent: bool) {
    let home = unique_home(tag);
    let control = without_gt_text(command);
    assert_ne!(control, command, "control must differ");
    let (cd, cr) = run_as(&control, &home, subagent);
    let (fd, fr) = run_as(command, &home, subagent);
    eprintln!("MEASURED subagent={subagent} control={cd} ({cr}) with-text={fd} ({fr})");
    assert!(
        rank(&fd) <= rank(&cd),
        "quoted-arg redirect text made it stricter (subagent={subagent}): control={cd} ({cr}) with-text={fd} ({fr})"
    );
}

#[test]
#[ignore = "RED: quoted-arg redirect text after cd FP, see backlog"]
fn observed_command_quoted_redirect_text_no_stricter_subagent() {
    check("os", OBSERVED, true);
}

#[test]
#[ignore = "RED: quoted-arg redirect text after cd FP, see backlog"]
fn observed_command_quoted_redirect_text_no_stricter_main() {
    check("om", OBSERVED, false);
}

#[test]
fn minimal_quoted_redirect_text_no_stricter_subagent() {
    check("ms", MINIMAL, true);
}

#[test]
fn minimal_quoted_redirect_text_no_stricter_main() {
    check("mm", MINIMAL, false);
}

#[test]
fn control_real_relative_redirect_after_unresolvable_cd_stays_non_allow() {
    let home = unique_home("r");
    let (d, r) = run_as(r#"cd "$X" && echo hi > out.txt"#, &home, true);
    eprintln!("MEASURED real-redirect={d} ({r})");
    assert_ne!(d, "allow", "real unresolved relative redirect must stay non-allow; reason={r}");
}
