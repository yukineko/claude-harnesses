#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog repro: deleting an irreplaceable `$HOME` asset (`~/.ssh/id_rsa`) is
//! Allow today. The deletion policy is "Deny: system configuration, gate/harness
//! config, irreplaceable assets ($HOME-level config)", so the correct verdict is
//! anything but Allow. The GREEN control pins that deleting a work file in the
//! payload-cwd project is Allow, showing the fixture is sane.

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
            .join("backlog_ssh_private_key_rm")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let f = Fx {
            home: base.join("home"),
            proj: base.join("src/proj"),
            base,
        };
        std::fs::create_dir_all(f.home.join(".ssh")).unwrap();
        std::fs::write(f.home.join(".ssh/id_rsa"), "PRIVATE KEY FIXTURE\n").unwrap();
        std::fs::create_dir_all(&f.proj).unwrap();
        std::fs::write(f.proj.join("scratch.txt"), "x\n").unwrap();
        f
    }

    fn run(&self, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": format!("backlog-ssh-rm-{}-{}", std::process::id(), command.len()),
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

#[test]
fn control_rm_project_work_file_is_allow() {
    let f = Fx::new("control");
    let out = f.run(&format!("rm {}/scratch.txt", f.proj.display()));
    assert!(
        is_allow(&out),
        "work file in project must be Allow; got {out}"
    );
}

#[test]
fn rm_ssh_private_key_absolute_is_not_allow() {
    let f = Fx::new("abs");
    let out = f.run(&format!("rm {}/.ssh/id_rsa", f.home.display()));
    assert!(
        !is_allow(&out),
        "rm <HOME>/.ssh/id_rsa must NOT be Allow; got empty output (= Allow)"
    );
}

#[test]
fn rm_ssh_private_key_tilde_is_not_allow() {
    let f = Fx::new("tilde");
    let out = f.run("rm ~/.ssh/id_rsa");
    assert!(
        !is_allow(&out),
        "rm ~/.ssh/id_rsa must NOT be Allow; got empty output (= Allow)"
    );
}
