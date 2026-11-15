#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 2026-11-14 suspected false positive (observed in a subagent): a `git add ... &&
//! git commit -F - <<'EOF' 2>&1 | tail -15; git log --oneline -1` command was
//! blocked with "the command word `mark-relayed` is an expansion whose value only
//! exists at run time". `mark-relayed` is only text inside a QUOTED heredoc body
//! (`<<'EOF'`), where a backtick span crosses a line break (`relay` newline
//! `mark-relayed`). Quoted heredoc bodies are data; no expansion happens.
//!
//! f0 = the exact recovered command. f1 = minimal form. c1 = control with an
//! UNquoted `<<EOF`, where backticks really are command substitutions and the
//! result must stay non-Allow.
//! Decisions only are asserted, never message prose.
//!
//! RED cases are `#[ignore]`; run with `-- --include-ignored`.
//!
//! DOES NOT PROVE: which of the two suspected triggers (multi-line backtick span
//! vs. `2>&1 | tail -15` after the heredoc operator) is the cause; other
//! heredoc delimiters; `$(...)` spans in quoted bodies.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let base = neutral_base::neutral_base()
            .join("quoted_heredoc_multiline_backtick_fp_20261114")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(proj.join("scratch")).unwrap();
        Fx { base, home, proj }
    }

    fn bash(&self, command: &str) -> (&'static str, String) {
        let payload = serde_json::json!({
            "session_id": "quoted-heredoc-multiline-backtick-fp-20261114",
            "hook_event_name": "PreToolUse",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        let v = verdict(&s);
        println!("[{command}] => {v} {:?}", s.trim());
        (v, s)
    }
}

fn verdict(out: &str) -> &'static str {
    if out.trim().is_empty() {
        "allow"
    } else if out.contains(r#""permissionDecision":"deny""#) {
        "deny"
    } else if out.contains(r#""permissionDecision":"ask""#) {
        "ask"
    } else {
        "other"
    }
}

/// The exact first entry of the recovered blocked command.
const F0_COMMAND: &str = r##"cd /Users/yuki/src/harness-wt-ask-relay; git add .claude-plugin/marketplace.json .specguard/spec-docs.toml Cargo.lock crates/blastguard/.claude-plugin/plugin.json crates/blastguard/Cargo.toml crates/blastguard/README.ja.md crates/blastguard/README.md crates/blastguard/hooks/hooks.json crates/blastguard/src/lib.rs crates/blastguard/src/main.rs crates/blastguard/src/relay.rs crates/blastguard/tests/subagent_ask_relay_20261114.rs docs/specs/blastguard.md && git commit -q -F - <<'EOF' 2>&1 | tail -15; git log --oneline -1
feat(blastguard): subagent Ask relay — a subagent's Ask is adjudicated in main

User rulings 2026-11-14 (verbatim):
- 「sub agentのAskはmainで裁定する」
- 「この仕組みはコードで強制する、どうせわすれる」
- 「gateにそんな例外は追加してほしくない」
- 「gateもconduktも絶対に勝手に他のworktreeを参照してはならない」

When a subagent's Bash Ask is hardened to Deny (and the main session could
ask), blastguard appends a `queued` record to ~/.blastguard/relay/<session>.jsonl
and appends the relay id to the printed deny reason. The verdict is unchanged;
the violation ledger keeps the unannotated reason. A true Deny is never queued.
A failed queue write is surfaced in the deny reason.

New hooks: PostToolUse/PostToolUseFailure Bash `relay mark-executed` (exact
command+cwd match, main session only), PostToolUse SendMessage `relay
mark-relayed` (id named, executed first), Stop `relay stop` (blocks while any
entry is neither executed+relayed nor dismissed; unreadable/corrupt queue
blocks fail-closed; runs under run_guarded). `blastguard relay dismiss <id>
--reason` requires a non-blank reason; dismissals are shown on later Stops.
Nothing in the relay executes a command or inspects the stored cwd.

RED tests by an independent agent (16 FAIL / 2 guard pass) -> 18/18 GREEN.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
"##;

#[test]
#[ignore = "RED 2026-11-14: multi-line backtick span in quoted heredoc body judged as command word"]
fn f0_exact_recovered_command_is_allowed() {
    let f = Fx::new("f0");
    let (v, raw) = f.bash(F0_COMMAND);
    assert_eq!(v, "allow", "exact recovered command; hook output: {raw}");
}

#[test]
fn f1_minimal_quoted_heredoc_multiline_backtick_is_allowed() {
    let f = Fx::new("f1");
    let cmd = "git commit -F - <<'EOF'\na `relay\nmark-relayed` b\nEOF\n";
    let (v, raw) = f.bash(cmd);
    assert_eq!(
        v, "allow",
        "quoted heredoc body is data: {cmd}; output: {raw}"
    );
}

#[test]
fn c1_control_unquoted_heredoc_multiline_backtick_stays_not_allowed() {
    let f = Fx::new("c1");
    let cmd = "git commit -F - <<EOF\na `relay\nmark-relayed` b\nEOF\n";
    let (v, raw) = f.bash(cmd);
    assert_ne!(
        v, "allow",
        "unquoted heredoc backticks execute: {cmd}; output: {raw}"
    );
    assert_ne!(v, "other", "unrecognised hook output: {raw}");
}

// Bisected 2026-11-14 (script over the built binary): removing any single body
// line of f0 keeps Ask; the trigger needs a command BEFORE the heredoc command
// (`true;` / `&&`) plus a backtick span crossing a line break inside the quoted
// body. Without the preceding command (f1) it is Allow.
#[test]
#[ignore = "RED 2026-11-14: preceding command + multi-line backtick span in quoted heredoc body"]
fn f2_minimal_preceded_heredoc_multiline_backtick_is_allowed() {
    let f = Fx::new("f2");
    let cmd = "true; cat <<'EOF'\na `relay\nmark-relayed` b\nEOF\n";
    let (v, raw) = f.bash(cmd);
    assert_eq!(
        v, "allow",
        "quoted heredoc body is data: {cmd}; output: {raw}"
    );
}

#[test]
fn f3_control_preceded_heredoc_single_line_backtick_is_allowed() {
    let f = Fx::new("f3");
    let cmd = "true; cat <<'EOF'\na `relay mark-relayed` b\nEOF\n";
    let (v, raw) = f.bash(cmd);
    assert_eq!(
        v, "allow",
        "single-line span in quoted body: {cmd}; output: {raw}"
    );
}
