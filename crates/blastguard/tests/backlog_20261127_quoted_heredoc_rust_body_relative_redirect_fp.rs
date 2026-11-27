#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED pin for a blastguard false positive observed 2026-11-27 (subagent
//! call, verbatim block reason):
//!
//! > '> harness_core::hook::run_pretooluse_guarded("modelguard",' is a RELATIVE
//! > redirect after an earlier cd/pushd/popd whose target directory blastguard
//! > could not statically resolve — it cannot tell whether this lands in a
//! > system directory or on a protected gate/config path, and refuses to guess
//!
//! Command shape:
//!
//! ```text
//! cd <literal absolute worktree> && cat > crates/modelguard/src/main.rs <<'EOF'
//! <rust source; lines include `    harness_core::hook::run_pretooluse_guarded("modelguard", |input| {`
//!  and `>` characters>
//! EOF
//! . "$HOME/.cargo/env"; cargo test -p modelguard --test model_policy 2>&1 | tail -8
//! ```
//!
//! Why Allow is right: the cd target is a literal absolute path (resolvable);
//! the delimiter is QUOTED so the body is inert data for `cat`; the only real
//! redirect is `cat > crates/modelguard/src/main.rs`, which resolves under the
//! literal cd target to a non-protected source file in the own project.
//!
//! Related, already-filed neighbour: `backlog_20261116_cd_literal_then_quoted_
//! heredoc_relative_redirect_fp.rs` (apostrophe line, then bare `>` line, over
//! 8 KiB). This file pins the 2026-11-27 shape with its own body and tail.
//!
//! Layout:
//!  * `fp_*`: the reported shape, Allow asserted in BOTH modes (main and
//!    subagent), and in particular no "RELATIVE redirect" reason.
//!  * `control_*` (must stay non-Allow): `cd "$X"` (only the shell knows the
//!    target) and a literal cd whose redirect lands on `.githooks/pre-commit`
//!    (a gate script, ruling Q4).
//!
//! DOES NOT PROVE: which parser path produces the FP; the size threshold (if
//! any); that the original body is reproduced byte for byte (it was not
//! retained; the body here is reconstructed from the reported lines); the
//! subagent reason text beyond its not naming "RELATIVE redirect".
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

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn new(name: &str) -> Fx {
        let base = neutral_base::neutral_base()
            .join("backlog_20261127_heredoc_rust_body_fp")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        for d in [
            ".claude-plugin",
            ".githooks",
            "crates/modelguard/.claude-plugin",
            "crates/modelguard/src",
            "crates/modelguard/tests",
        ] {
            std::fs::create_dir_all(proj.join(d)).unwrap();
        }
        Fx { base, home, proj }
    }

    /// `@PROJ@` in `command` is the fixture project dir (also payload `cwd`
    /// and `CLAUDE_PROJECT_DIR`).
    fn run(&self, command: &str, subagent: bool) -> (&'static str, String) {
        let command = command.replace("@PROJ@", &self.proj.display().to_string());
        let mut payload = serde_json::json!({
            "session_id": "heredoc-rust-body-fp-20261127",
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
        let reason = if v == "allow" {
            String::new()
        } else {
            let doc: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
            doc["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap_or("")
                .to_string()
        };
        (v, reason)
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

/// Reconstructed modelguard main.rs body: contains the reported line, `>`
/// (comparison, `->`, `=>`), `(`, quotes and an apostrophe in a comment.
const RUST_BODY: &str = r#"//! modelguard: PreToolUse gate on Agent spawns.
use std::process::ExitCode;

fn main() -> ExitCode {
    // the hook's verdict isn't optional: block when the model is banned
    harness_core::hook::run_pretooluse_guarded("modelguard", |input| {
        let model = input.tool_input.get("model").and_then(|m| m.as_str());
        match model {
            Some(m) if m.len() > 0 && m.contains("fable") => modelguard::deny(m),
            Some(m) => modelguard::check(m),
            None => modelguard::default_model(),
        }
    })
}
"#;

const FILE: &str = "crates/modelguard/src/main.rs";

/// The reported tail after the heredoc close.
fn command(cd_target: &str, file: &str, body: &str) -> String {
    format!(
        "cd {cd_target} && cat > {file} <<'EOF'\n{body}EOF\n\
. \"$HOME/.cargo/env\"; cargo test -p modelguard --test model_policy 2>&1 | tail -8"
    )
}

/// Body padded with more realistic Rust (with `>` and `(`) past `min_bytes`.
fn padded_body(min_bytes: usize) -> String {
    let mut b = RUST_BODY.to_string();
    let mut i = 0;
    while command("@PROJ@", FILE, &b).len() < min_bytes {
        b.push_str(&format!(
            "\nfn helper_{i}(n: usize) -> bool {{\n    // it's a helper\n    n > {i} && check(\"x\", n)\n}}\n"
        ));
        i += 1;
    }
    b
}

fn assert_allow_both(tag: &str, command: &str) {
    let fx = Fx::new(tag);
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = fx.run(command, subagent);
        println!(
            "[{tag}] bytes={} subagent={subagent} => {d} {r:?}",
            command.len()
        );
        if r.contains("RELATIVE redirect") {
            bad.push(format!(
                "subagent={subagent} => {d} with RELATIVE-redirect reason ({r})"
            ));
        } else if d != "allow" {
            bad.push(format!("subagent={subagent} => {d} ({r})"));
        }
    }
    assert!(
        bad.is_empty(),
        "{tag}: expected Allow in both modes:\n{}",
        bad.join("\n")
    );
}

fn assert_not_allow_both(tag: &str, command: &str) {
    let fx = Fx::new(tag);
    for subagent in [false, true] {
        let (d, r) = fx.run(command, subagent);
        println!("[{tag}] subagent={subagent} => {d} {r:?}");
        assert_ne!(
            d, "allow",
            "control {tag} subagent={subagent} must not Allow"
        );
    }
}

// ---------------------------------------------------------------------
// FP targets
// ---------------------------------------------------------------------

#[test]
fn fp_literal_cd_quoted_heredoc_rust_body_short() {
    assert_allow_both("fp_short", &command("@PROJ@", FILE, RUST_BODY));
}

#[test]
fn fp_literal_cd_quoted_heredoc_rust_body_padded_past_8kib() {
    assert_allow_both("fp_padded", &command("@PROJ@", FILE, &padded_body(9000)));
}

// ---------------------------------------------------------------------
// Controls (must stay non-Allow)
// ---------------------------------------------------------------------

#[test]
fn control_undeterminable_cd_stays_blocked() {
    assert_not_allow_both("control_undet", &command("\"$X\"", FILE, RUST_BODY));
}

#[test]
fn control_literal_cd_redirect_onto_githooks_pre_commit_stays_blocked() {
    assert_not_allow_both(
        "control_githooks",
        &command("@PROJ@", ".githooks/pre-commit", "#!/bin/sh\nexit 0\n"),
    );
}
