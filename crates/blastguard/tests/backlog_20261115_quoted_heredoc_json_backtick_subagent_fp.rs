#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 2026-11-15 suspected false positive (observed in a subagent, main rev
//! a7a670560): a `cat > decomp.json <<'EOF' ... EOF` command writing a condukt
//! decomposition JSON was denied with "the command word `"done_criteria":"`cargo`
//! is an expansion whose value only exists at run time". The backticks are only
//! text inside a QUOTED heredoc body (`<<'EOF'`), which the shell never expands.
//!
//! f0 = the shape of the recovered command (heredoc into a file, then commands
//! after the terminator), run in the fixture project. f1 = minimal form.
//! c1 = control with an UNquoted `<<EOF`, where a backtick span really is a
//! command substitution of an unresolved variable and must stay non-Allow.
//! Decisions only are asserted, never message prose.
//!
//! RED cases are `#[ignore]`; run with `-- --include-ignored`.
//!
//! DOES NOT PROVE: whether the trigger is the `"key":"` prefix glued to the
//! backtick, the line position, or the following commands; other delimiters.
//!
//! RED written by the finder, who does not implement the fix.

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
            .join("backlog_20261115_quoted_heredoc_json_backtick_subagent_fp")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(proj.join("scratch")).unwrap();
        Fx { base, home, proj }
    }

    fn bash(&self, command: &str, subagent: bool) -> &'static str {
        let mut payload = serde_json::json!({
            "session_id": "quoted-heredoc-json-backtick-fp-20261115",
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
            .env("HOME", &self.home)
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
        let v = verdict(&s);
        println!("[subagent={subagent}] [{command}] => {v} {:?}", s.trim());
        v
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

const F0_COMMAND: &str = r##"cd scratch && cat > decomp.json <<'EOF'
{"goal":"fix: `<&N-` is Allow",
 "tasks":[{"id":"t1","title":"t","touched_files":["a.rs"],
 "done_criteria":"`cargo test -p blastguard --test x` passes; `N-` is recorded.",
 "commands":["cargo test -p blastguard --test x"]}]}
EOF
cd .. && ls scratch"##;

const F1_COMMAND: &str =
    "cat > scratch/d.json <<'EOF'\n{\"done_criteria\":\"`cargo test -p x` passes\"}\nEOF\n";

const C1_COMMAND: &str = "cat > scratch/d.json <<EOF\n{\"done_criteria\":\"`$X` passes\"}\nEOF\n";

#[test]
#[ignore = "RED: 2026-11-15 quoted heredoc JSON body backtick denied in subagent — remove when fixed"]
fn f0_recovered_shape_is_allow_in_subagent_and_main() {
    let fx = Fx::new("f0");
    // Both verdicts are observed before asserting, so a failure reports the
    // main-session verdict too (a subagent Deny can be a main-session Ask).
    let got = (fx.bash(F0_COMMAND, true), fx.bash(F0_COMMAND, false));
    assert_eq!(got, ("allow", "allow"), "(subagent, main)");
}

#[test]
#[ignore = "RED: 2026-11-15 quoted heredoc JSON body backtick denied in subagent — remove when fixed"]
fn f1_minimal_is_allow_in_subagent_and_main() {
    let fx = Fx::new("f1");
    // Both verdicts are observed before asserting, so a failure reports the
    // main-session verdict too (a subagent Deny can be a main-session Ask).
    let got = (fx.bash(F1_COMMAND, true), fx.bash(F1_COMMAND, false));
    assert_eq!(got, ("allow", "allow"), "(subagent, main)");
}

#[test]
fn c1_unquoted_heredoc_unresolved_substitution_is_not_allow() {
    let fx = Fx::new("c1");
    assert_ne!(fx.bash(C1_COMMAND, true), "allow", "subagent");
    assert_ne!(fx.bash(C1_COMMAND, false), "allow", "main");
}
