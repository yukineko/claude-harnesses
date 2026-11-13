#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED for two false positives observed 2026-11-13 at main 50474876
//! (blastguard 0.2.506), in subagent mode. Nothing harmful is executed in
//! either command.
//!
//! FP1: a QUOTED heredoc appending Rust source to a `.rs` file whose body
//! holds Rust string literals containing `python3 -c \"...\"` text, followed
//! by `cargo test`. The body is data (standing ruling: a heredoc body written
//! to a .py/.rs target is data, not shell; only .sh / no-extension targets are
//! judged as shell, unless run in the same command). Observed block: "`python3`
//! is invoked with an inline-eval flag, and its program cannot be tokenised as
//! Python".
//!
//! FP2: an explicit-path `git add` then `git commit` with `-m` messages that
//! contain escaped double quotes (`\"`). The quotes are balanced in shell
//! terms. Observed block: "cannot determine whether this git add/commit is
//! whole-tree staging (an unclosed double quote)".
//!
//! Observed reproduction status (hook binary, blastguard 0.2.507 worktree):
//! FP1 near-verbatim shape reproduces (main: ask, subagent: deny). FP2 as
//! reported (plain `git add && git commit -m \"..\\\"..\"` command) does NOT
//! reproduce: it is allow, so it is kept only as a control. What reproduces is
//! FP2b: git add/commit text with escaped quotes inside a quoted `.rs`
//! heredoc body followed by another command (same reason text as FP2).
//!
//! Asserted by the RED tests: FP1 and FP2b shapes are "allow" in main and
//! subagent mode. Controls (not ignored, pass at HEAD): the same heredoc
//! without the python3 text; a one-line python3 body; the plain FP2 commit; a
//! commit without escaped quotes; the FP2b body with no trailing command.
//!
//! DOES NOT PROVE: that other interpreters / other targets are handled, that
//! a heredoc to a .sh target is still judged as shell, or that commit messages
//! with unbalanced quotes are still blocked. Only the listed shapes are
//! covered. The decision is text-only; no real repo is needed.
//!
//! Written by an independent subagent for the flow-8cca7ef5 run (not a fix
//! implementer).

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
        "bg-fp-hdmsg-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt").join("crates/blastguard/tests")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "fp-heredoc-msg-20261113",
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

fn assert_allow_everywhere(commands: &[String], home: &Home) {
    let mut failures = Vec::new();
    for c in commands {
        for subagent in [false, true] {
            let (d, r) = run_as(c, home, subagent);
            if d != "allow" {
                failures.push(format!("subagent={subagent} {c:?} -> {d} | {r}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} benign command(s) were not allowed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn heredoc(target: &str, body: &str, tail: &str) -> String {
    format!("cat >> {target} <<'EOF'\n{body}\nEOF\n{tail}")
}

fn fp1_shapes(home: &Home) -> Vec<String> {
    let t = home.0.join("wt/crates/blastguard/tests/x.rs");
    let t = t.display().to_string();
    let wt = home.0.join("wt").display().to_string();
    let minimal = "const S: &str = \"python3 -c \\\"print(1)\\\"\";";
    let near_verbatim = "\nconst PATHLIB_CHMOD_SHAPES: &[&str] = &[\n    \"python3 -c \\\"from pathlib import Path; Path('/etc/hosts').chmod(0o777)\\\"\",\n    \"python3 -c \\\"import pathlib; pathlib.Path('/etc/hosts').chmod(0o777)\\\"\",\n];";
    // Observed: the single-line `minimal` body is ALLOWED (control below); only
    // the multi-line near-verbatim body reproduces.
    let _ = minimal;
    vec![heredoc(
        &t,
        near_verbatim,
        &format!("cd {wt} && cargo test -p blastguard --test x"),
    )]
}

// Control: the one-line python3-in-string body does not reproduce FP1 at HEAD.
#[test]
fn control_rs_heredoc_with_one_line_python_text_is_allowed() {
    let home = unique_home("fp1m");
    let t = home.0.join("wt/crates/blastguard/tests/x.rs");
    let wt = home.0.join("wt").display().to_string();
    let b = "const S: &str = \"python3 -c \\\"print(1)\\\"\";";
    let c = vec![
        heredoc(&t.display().to_string(), b, ""),
        heredoc(
            &t.display().to_string(),
            b,
            &format!("cd {wt} && cargo test -p blastguard --test x"),
        ),
    ];
    assert_allow_everywhere(&c, &home);
}

// test-weakening-justified: new RED for an unfixed false positive (2026-11-13);
// the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: quoted heredoc body with python3 -c \"..\" Rust string literals appended to a .rs file is denied in subagent mode (FP1, 2026-11-13) — remove the ignore when fixed"]
fn fp1_rs_heredoc_body_with_python_c_text_is_allowed() {
    let home = unique_home("fp1");
    assert_allow_everywhere(&fp1_shapes(&home), &home);
}

// Control: same heredoc, no python3 text.
#[test]
fn control_rs_heredoc_without_python_text_is_allowed() {
    let home = unique_home("fp1c");
    let t = home.0.join("wt/crates/blastguard/tests/x.rs");
    let wt = home.0.join("wt").display().to_string();
    let c = heredoc(
        &t.display().to_string(),
        "const S: &[&str] = &[\n    \"hello\",\n];",
        &format!("cd {wt} && cargo test -p blastguard --test x"),
    );
    assert_allow_everywhere(&[c], &home);
}

// FP2 as reported (plain command, escaped `\"` inside balanced -m messages).
// At 50474876 + this worktree these shapes are ALLOWED by the hook binary
// (observed), so they do not reproduce as a RED; they are kept as passing
// controls. What does reproduce is FP2b below: the same git add/commit TEXT
// inside the body of a quoted heredoc written to a .rs file (observed: the
// subagent's own `cat >> x.rs <<'EOF'` command carrying such a body was blocked
// with the FP2 reason).
fn fp2_plain_shapes() -> Vec<String> {
    vec![
        "git add a.txt && git commit -q -m \"say \\\"hi\\\"\"".to_string(),
        "git add .claude-plugin/marketplace.json Cargo.lock crates/blastguard/Cargo.toml && git commit -q -m \"test(blastguard): RED for pathlib Path(..).chmod on a system path (569b9b9d scope)\" -m \"python3 -c \\\"from pathlib import Path; Path('/etc/hosts').chmod(0o777)\\\" and the pathlib.Path form are Allow in main and subagent mode.\" -m \"Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\"".to_string(),
    ]
}

// Control: FP2 as reported (plain command) is allowed at HEAD.
#[test]
fn control_fp2_plain_commit_with_escaped_quotes_is_allowed() {
    let home = unique_home("fp2p");
    assert_allow_everywhere(&fp2_plain_shapes(), &home);
}

fn fp2b_shapes(home: &Home) -> Vec<String> {
    let t = home.0.join("wt/crates/blastguard/tests/x.rs");
    let t = t.display().to_string();
    let wt = home.0.join("wt").display().to_string();
    // Rust source line (heredoc body) that merely contains git commit text.
    // (observed: a one-message variant `format!("... -m \"{msg}\"")` is allowed;
    // the two-message escaped-quote variant below reproduces.)
    let minimal =
        r#""git add a.txt && git commit -q -m \"a \\\"b\\\" c\" -m \"d \\\"e\\\"\"".to_string(),"#;
    vec![
        format!("cat >> {t} <<'EOF'\n{minimal}\nEOF\ncargo test -p blastguard --test x"),
        format!("cat >> {t} <<'EOF'\n{minimal}\nEOF\ncd {wt} && cargo test -p blastguard --test x"),
    ]
}

// test-weakening-justified: new RED for an unfixed false positive (2026-11-13);
// the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: quoted heredoc body (to a .rs file) containing git add/commit text with escaped quotes, followed by another command, is denied as 'unclosed double quote' whole-tree staging (FP2b, 2026-11-13) — remove the ignore when fixed"]
fn fp2b_rs_heredoc_body_with_git_commit_text_is_allowed() {
    let home = unique_home("fp2b");
    assert_allow_everywhere(&fp2b_shapes(&home), &home);
}

// Control: the same body with no trailing command is allowed at HEAD.
#[test]
fn control_rs_heredoc_with_git_commit_text_and_no_tail_is_allowed() {
    let home = unique_home("fp2bc");
    let t = home.0.join("wt/crates/blastguard/tests/x.rs");
    let body =
        r#""git add a.txt && git commit -q -m \"a \\\"b\\\" c\" -m \"d \\\"e\\\"\"".to_string(),"#;
    let c = format!("cat >> {} <<'EOF'\n{body}\nEOF", t.display());
    assert_allow_everywhere(&[c], &home);
}

// Control: same commit, no escaped quotes.
#[test]
fn control_commit_message_without_escaped_quotes_is_allowed() {
    let home = unique_home("fp2c");
    let c = vec![
        "git add a.txt && git commit -q -m \"say hi\"".to_string(),
        "git add .claude-plugin/marketplace.json Cargo.lock crates/blastguard/Cargo.toml && git commit -q -m \"test(blastguard): RED for pathlib on a system path (569b9b9d scope)\" -m \"Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\"".to_string(),
    ];
    assert_allow_everywhere(&c, &home);
}
