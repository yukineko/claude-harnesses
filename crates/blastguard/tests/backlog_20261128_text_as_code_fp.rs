#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED pins for blastguard false positives observed 2026-11-28 in subagent
//! calls: text / data (a quoted here-document body written to a non-shell
//! file, a commit message string) read as shell code.
//!
//! User ruling (premise): a here-document body is judged by the language of
//! the file it is written to. The body of a heredoc written to a `.rs` / `.py`
//! / `.md` (non-shell) file is DATA unless the same command runs that file,
//! and is not interpreted as shell. Strings and code are told apart with a
//! real parser.
//!
//! Observed shapes (subagent Deny), with the verbatim reasons (only F2 and
//! F3 reproduced here; see "Measured" below):
//!   F1 `cat > crates/blastguard/src/detect/fileops/write_targets.rs <<'EOF'`,
//!      Rust body whose doc comments hold backticks:
//!      "'> `' destroys the file's current contents and ``` contains a shell
//!      expansion, so blastguard cannot tell which file this names"
//!   F2 `cat >> <file>.rs <<'EOF'` (Rust body with `!` and backticks) plus
//!      rustfmt and cargo test in the same command:
//!      "'> !`' destroys the file's current contents and `!`` contains a shell
//!      expansion"
//!   F3 `git commit -q -m "...git apply --unsafe-paths --directory=~/.claude
//!      <patch>..."`: "git apply/am reads its patch through `<` (not a
//!      here-document): the patch cannot be read here" (main session: allowed)
//!   F4 backtick heredoc append to docs/specs/*.md stopped as an `rm`
//!      expansion (body holds the word `rm`).
//!
//! Expectation: a write to an ordinary file inside the project gets the SAME
//! verdict as the plain `cat > f <<'EOF'\nhello\nEOF` with the same tail
//! (which is itself Allow — asserted as fixture sanity); for F3 the verdict is
//! the same as for the same command with the `git apply ...` words removed
//! from the message. Both modes (main and subagent, `agent_id` in the
//! payload) are measured.
//!
//! Controls (must stay non-Allow): the body run as shell in the same command
//! (`bash <<'EOF' rm -rf / EOF`, `cat > x.sh <<EOF … EOF; sh x.sh`).
//!
//! DOES NOT PROVE: the original bodies byte for byte (they were not retained;
//! the bodies here are reconstructed so that the reported redirect targets
//! "`" / "!`" appear after a `> ` in the text); which parser path produces the
//! FP; that a real `$HOME` behaves the same (scratch HOME). The fixture lives
//! under `/private/tmp/claude-askrules` (a blastguard temp safe root), which
//! may make some write verdicts more permissive than in a real worktree.
//!
//! Measured 2026-11-28 on main b19e77851 with this file's fixture:
//!   F2 (this body, `>>`, rustfmt && cargo test | tail)  main deny / subagent deny
//!   F2 body with NO tail, `>>` or `>`                        allow / allow
//!   F3 multi-line message (the `<patch>` line is its own
//!      physical line)                                         main ask / subagent deny
//!   F3 single-line message, same words                       allow / allow
//!   NOT REPRODUCED (removed, reported instead): F1 (doc comments with
//!   `> ` + backtick, with and without the F2 tail; also `// `a > !` is text`
//!   lines with a `cargo test` / `rustfmt` tail) and F4 (`.md` append with
//!   `` `rm -rf target` `` text, with and without a `git add` tail) were Allow
//!   in both modes. F2 fires only with the double-quoted string line
//!   `assert!(!t.is_empty(), "`> !` must name `!b`");` plus a later command.
//!
//! RED written by an independent test author, not the implementer.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    _dir: tempfile::TempDir,
    base: PathBuf,
    proj: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        std::fs::create_dir_all("/private/tmp/claude-askrules").unwrap();
        let dir = tempfile::Builder::new()
            .prefix("text-as-code-fp-")
            .tempdir_in("/private/tmp/claude-askrules")
            .unwrap();
        let base = dir.path().to_path_buf();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let proj = base.join("proj");
        for d in [
            "crates/blastguard/src/detect/fileops",
            "crates/blastguard/tests",
            "docs/specs",
        ] {
            std::fs::create_dir_all(proj.join(d)).unwrap();
        }
        Fx {
            _dir: dir,
            base,
            proj,
        }
    }

    /// (verdict, reason) for `command` in main (`subagent == false`) or
    /// subagent mode.
    fn run(&self, command: &str, subagent: bool) -> (String, String) {
        let mut payload = serde_json::json!({
            "session_id": "text-as-code-fp-20261128",
            "hook_event_name": "PreToolUse",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("test-subagent");
        }
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", self.base.join("home"))
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let out = bounded::run_bounded(
            &mut cmd,
            payload.to_string().as_bytes(),
            bounded::HOOK_DEADLINE,
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        if s.trim().is_empty() {
            return ("allow".into(), String::new());
        }
        let v: serde_json::Value = serde_json::from_str(s.trim()).expect("hook JSON");
        let h = &v["hookSpecificOutput"];
        (
            h["permissionDecision"]
                .as_str()
                .unwrap_or_else(|| panic!("no permissionDecision: {s}"))
                .to_string(),
            h["permissionDecisionReason"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        )
    }
}

/// `subject` must get the same verdict as `baseline` in both modes, and the
/// baseline must be Allow (fixture sanity: otherwise the comparison is void).
fn assert_same_as_baseline(tag: &str, subject: &str, baseline: &str) {
    let fx = Fx::new();
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (bv, br) = fx.run(baseline, subagent);
        assert_eq!(
            bv, "allow",
            "{tag}: baseline must be Allow (subagent={subagent}): {br}\n{baseline}"
        );
        let (v, r) = fx.run(subject, subagent);
        println!("[{tag}] subagent={subagent} => {v} {r:?}");
        if v != bv {
            bad.push(format!("subagent={subagent} => {v} ({r})"));
        }
    }
    assert!(
        bad.is_empty(),
        "{tag}: expected the baseline verdict (allow) in both modes:\n{}",
        bad.join("\n")
    );
}

fn assert_not_allow_both(tag: &str, command: &str) {
    let fx = Fx::new();
    for subagent in [false, true] {
        let (v, r) = fx.run(command, subagent);
        println!("[{tag}] subagent={subagent} => {v} {r:?}");
        assert_ne!(
            v, "allow",
            "control {tag} subagent={subagent} must not Allow"
        );
    }
}

fn heredoc(op: &str, file: &str, body: &str, tail: &str) -> String {
    format!("cat {op} {file} <<'EOF'\n{body}EOF\n{tail}")
}

const PLAIN: &str = "hello\n";

// ---------------------------------------------------------------- F2

const F2_FILE: &str = "crates/blastguard/tests/write_targets_probe.rs";

/// Rust test body with `!` macros and backticks; one comment holds `> !`
/// followed by a backtick, the reported target "!`".
const F2_BODY: &str = r#"
#[test]
fn negated_redirect_is_not_a_target() {
    // `a > !` is text here, not a redirect
    let t = write_targets("echo a > !b");
    assert!(!t.is_empty(), "`> !` must name `!b`");
    assert_eq!(t, vec!["!b".to_string()]);
}
"#;

const F2_TAIL: &str = "rustfmt crates/blastguard/tests/write_targets_probe.rs && \
cargo test -p blastguard --test write_targets_probe 2>&1 | tail -5";

// test-weakening-justified: ignore-added on a new RED for an unfixed FP (observed main deny / subagent deny 2026-11-28); backlog add for it was blocked by blastguard (relay bgr-4f96d9843b69), id to be written back
#[test]
#[ignore = "RED: 2026-11-28 blastguard FP — quoted .rs heredoc body string line read as redirect '> !`' when later commands follow; observed Deny (main+subagent)"]
fn f2_rs_heredoc_append_bang_backtick_then_rustfmt_cargo_test_same_as_plain() {
    assert_same_as_baseline(
        "f2",
        &heredoc(">>", F2_FILE, F2_BODY, F2_TAIL),
        &heredoc(">>", F2_FILE, PLAIN, F2_TAIL),
    );
}

// ---------------------------------------------------------------- F3

/// Multi-line commit message whose body mentions `git apply ... <patch>`.
fn f3_commit(mention: &str) -> String {
    format!(
        "git commit -q -m \"blastguard: refuse unreadable patch input\n\n\
The reader recognises {mention} as a patch reader.\n\n\
Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\""
    )
}

// test-weakening-justified: ignore-added on a new RED for an unfixed FP (backlog ad84b715, observed main ask / subagent deny 2026-11-28)
#[test]
#[ignore = "RED(ad84b715): 2026-11-28 blastguard FP — multi-line commit -m message line 'git apply ... <patch>' refused as git apply reading through <; observed Ask/Deny"]
fn f3_commit_message_mentioning_git_apply_lt_patch_same_as_without() {
    assert_same_as_baseline(
        "f3_multiline",
        &f3_commit("git apply --unsafe-paths --directory=~/.claude <patch>"),
        &f3_commit("the patch command"),
    );
}

// ---------------------------------------------------------------- controls

#[test]
fn control_bash_heredoc_body_is_shell_stays_blocked() {
    assert_not_allow_both("control_bash", "bash <<'EOF'\nrm -rf /\nEOF");
}

#[test]
fn control_sh_file_written_then_run_stays_blocked() {
    assert_not_allow_both("control_sh", "cat > x.sh <<EOF\nrm -rf ~\nEOF\nsh x.sh");
}
