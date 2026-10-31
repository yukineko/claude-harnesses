#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog repro: `B=$(ls -d <HOME>/.claude/plugins/cache/yukineko/backlog/*/bin/backlog
//! | sort -V | tail -1); echo $B` is answered with an ask saying "`-d` is a command
//! blastguard has no rule for", i.e. the `$( ls -d ...)` substitution is misparsed
//! and `-d` is taken as the command word. Correct: the reason must never name `-d`
//! as the command. The control pins today's verdict for the bare `ls -d ...`.

#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::io::Write;
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
            .join("backlog_ls_d_cmdsubst_misparse")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let f = Fx {
            home: base.join("home"),
            proj: base.join("src/proj"),
            base,
        };
        let bin = f
            .home
            .join(".claude/plugins/cache/yukineko/backlog/0.1.0/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("backlog"), "#!/bin/sh\n").unwrap();
        std::fs::create_dir_all(&f.proj).unwrap();
        f
    }

    fn glob(&self) -> String {
        format!(
            "{}/.claude/plugins/cache/yukineko/backlog/*/bin/backlog",
            self.home.display()
        )
    }

    fn run(&self, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": format!("backlog-ls-d-{}-{}", std::process::id(), command.len()),
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
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let mut child = cmd.spawn().expect("binary spawns");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("binary runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        println!("[{command}] => {:?}", s.trim());
        s
    }
}

fn is_allow(out: &str) -> bool {
    out.trim().is_empty()
}

fn reason(out: &str) -> String {
    if is_allow(out) {
        return String::new();
    }
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("hook output is JSON");
    v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

// Pinned from observation (see report); recorded, not endorsed.
const PINNED_BARE_LS_D_IS_ALLOW: bool = true;

#[test]
fn control_bare_ls_d_verdict_pinned() {
    let f = Fx::new("control");
    let out = f.run(&format!("ls -d {}", f.glob()));
    assert_eq!(
        PINNED_BARE_LS_D_IS_ALLOW,
        is_allow(&out),
        "bare `ls -d` verdict changed; got {out}"
    );
    assert!(
        !reason(&out).contains("`-d` is a command"),
        "bare `ls -d` must not misparse -d as command; got {out}"
    );
}

#[test]
fn cmdsubst_ls_d_does_not_name_dash_d_as_command() {
    let f = Fx::new("subst");
    let out = f.run(&format!(
        "B=$(ls -d {} | sort -V | tail -1); echo $B",
        f.glob()
    ));
    let r = reason(&out);
    assert!(
        !r.contains("`-d` is a command"),
        "reason must not name `-d` as the command; got reason: {r}"
    );
}
