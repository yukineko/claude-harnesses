#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Refusal guards for the "truncating redirect onto an EXISTING regular file under
//! /tmp or /private/tmp is Allow" ruling: multi-link files and a $TMPDIR outside
//! the temp roots must stay non-Allow. c1 is the Allow control. (An untracked or
//! gitignored file in a git tree under a temp root keeps its existing Allow, per
//! the 2026-09-18 ruling; see the 2026-10-28 revert.)

#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

struct Fx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
    /// unique dir under /private/tmp
    tmp: PathBuf,
    session: String,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let uniq = format!("{}-{n}-{name}", std::process::id());
        let base = neutral_base::neutral_base()
            .join("tmp_truncation_refusals")
            .join(&uniq);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&proj).unwrap();
        let tmp = PathBuf::from(format!("/private/tmp/bg-truncref-{uniq}"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        Fx {
            base,
            home,
            proj,
            tmp,
            session: format!("tmp-truncref-{uniq}"),
        }
    }

    fn run(&self, command: &str) -> String {
        self.run_env(command, None)
    }

    fn run_env(&self, command: &str, tmpdir: Option<&Path>) -> String {
        let payload = serde_json::json!({
            "session_id": self.session,
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
        if let Some(t) = tmpdir {
            cmd.env("TMPDIR", t);
        }
        // Probe hook: BG_REFUSALS_BIN runs the same cases against another binary.
        if let Ok(bin) = std::env::var("BG_REFUSALS_BIN") {
            cmd = Command::new(bin);
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
            if let Some(t) = tmpdir {
                cmd.env("TMPDIR", t);
            }
        }
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

    fn write(&self, p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.tmp);
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn is_allow(out: &str) -> bool {
    out.trim().is_empty()
}

#[track_caller]
fn assert_allow(case: &str, out: &str) {
    assert!(
        is_allow(out),
        "{case}: must be Allow (no output); got {out}"
    );
}

#[track_caller]
fn assert_not_allow(case: &str, out: &str) {
    assert!(
        !is_allow(out),
        "{case}: must NOT be Allow; got empty output (= Allow)"
    );
}

// ---- guards: an existing-file truncation under tmp must NOT be Allow when ----

#[test]
fn h1_hardlink_to_file_outside_tmp_not_allow() {
    let f = Fx::new("h1");
    let outside = f.base.join("outside/precious.txt");
    f.write(&outside, "keep\n");
    let link = f.tmp.join("x");
    std::fs::hard_link(&outside, &link).unwrap();
    let out = f.run(&format!("echo a > {}", link.display()));
    assert_not_allow("h1", &out);
}

#[test]
fn g3_tmpdir_is_not_a_temp_root_not_allow() {
    let f = Fx::new("g3");
    let scratch = f.base.join("tmpdir-scratch");
    f.write(&scratch.join("plain.txt"), "p\n");
    let out = f.run_env(
        &format!("echo a > {}/plain.txt", scratch.display()),
        Some(&scratch),
    );
    assert_not_allow("g3", &out);
}

// ---- control: plain single-link file under /private/tmp, no git -> Allow ----

#[test]
fn c1_plain_existing_single_link_file_in_tmp_allows() {
    let f = Fx::new("c1");
    f.write(&f.tmp.join("plain.txt"), "p\n");
    let out = f.run(&format!("echo a > {}/plain.txt", f.tmp.display()));
    assert_allow("c1", &out);
}
