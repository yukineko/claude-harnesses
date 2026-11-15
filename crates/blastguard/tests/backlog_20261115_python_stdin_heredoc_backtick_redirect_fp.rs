#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! 2026-11-15 suspected false positive (observed in a subagent, main rev
//! 72cb90009): a `cd <dir> && python3 - <<'EOF' ... EOF` command whose Python
//! body held Rust doc text such as "`check_rerun` -> `tree_identity(cwd)`" was
//! denied with "'> ' destroys the file's current contents and `` is a relative
//! path and blastguard does not know which directory it is relative to, so it
//! cannot tell whether these bytes are recoverable". The body of a QUOTED
//! heredoc fed to `python3 -` is Python source on stdin: the shell never parses
//! it, so `-> \`...\`` in it is not a redirect.
//!
//! f0 = the recovered shape (cd, then python3 - with a quoted heredoc whose body
//! carries a raw string with `->` and backticks, then commands after EOF).
//! f1 = narrowing control (GREEN at HEAD): a minimal python3 - body with
//! `-> \`...\`` alone is allowed, so that token is NOT the trigger.
//! c1 = control: a real shell redirect (outside any heredoc) to an unresolved
//! command-substitution target must stay non-Allow. Decisions only are asserted.
//! Measured 2026-11-15 with main at 95fbd0631: f0 (subagent, main) =
//! ("deny", "deny") with the verbatim observed reason; f1 = allow in both.
//!
//! Readings: (a) defect: the quoted heredoc body of `python3 -` is judged as
//! shell; (b) intended: a body that reaches an interpreter is judged
//! conservatively. Tell apart: the reason speaks of shell redirect semantics
//! ("'> ' destroys the file's current contents") on text that only Python reads;
//! (b) would need a Python-level reason.
//!
//! DOES NOT PROVE: which part of the f0 body is the trigger (suspects: the
//! `<task worktree> &&` / `<out of scope>` angle-bracket text, the multi-line
//! raw string, the `open(...,'w')` call, or the commands after EOF); other
//! interpreters (`node -`, `ruby`).
//!
//! RED written by an independent test author, not the implementer.

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
            .join("backlog_20261115_python_stdin_heredoc_backtick_redirect_fp")
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
            "session_id": "python-stdin-heredoc-backtick-redirect-fp-20261115",
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

const F0_COMMAND: &str = r#"cd scratch && python3 - <<'EOF'
header = r'''//! Both `check_rerun` -> `tree_identity(cwd)` (rule 3) and
//! `check_task_scope(tests, cwd)` judge the payload cwd: `cd <task worktree> && cargo test --test
//! <out of scope>` is allowed.
'''
open('out.rs','w').write(header)
EOF
cd .. && ls scratch"#;

const F1_COMMAND: &str =
    "python3 - <<'EOF'\nx = \"check_rerun -> `tree_identity(cwd)`\"\nprint(x)\nEOF\n";

const C1_COMMAND: &str = "cd scratch && echo x > `printf %s \"$T\"`\n";

// test-weakening-justified: new RED for an unfixed defect (2026-11-15).
#[test]
#[ignore = "RED: 2026-11-15 quoted heredoc body of python3 - judged as shell redirect — remove when fixed"]
fn f0_recovered_shape_is_allow_in_subagent_and_main() {
    let fx = Fx::new("f0");
    let got = (fx.bash(F0_COMMAND, true), fx.bash(F0_COMMAND, false));
    assert_eq!(got, ("allow", "allow"), "(subagent, main)");
}

#[test]
fn f1_minimal_is_allow_in_subagent_and_main() {
    let fx = Fx::new("f1");
    let got = (fx.bash(F1_COMMAND, true), fx.bash(F1_COMMAND, false));
    assert_eq!(got, ("allow", "allow"), "(subagent, main)");
}

#[test]
fn c1_real_shell_redirect_to_unresolved_target_is_not_allow() {
    let fx = Fx::new("c1");
    assert_ne!(fx.bash(C1_COMMAND, true), "allow", "subagent");
    assert_ne!(fx.bash(C1_COMMAND, false), "allow", "main");
}
