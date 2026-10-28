#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-10-28: a truncating redirect onto an EXISTING file inside
//! `/private/tmp` (incl. the Claude scratchpad `/private/tmp/claude-*/...`) is
//! recoverable work and must Allow without asking. A NEW file there is already
//! Allow. Escapes out of tmp (`..`, symlink), protected paths and unresolved
//! variables must stay non-Allow.
//!
//! The payload cwd is a neutral_base project dir that is NOT under /private/tmp.

#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::io::Write;
use std::os::unix::fs::symlink;
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
            .join("tmp_truncation_recoverable")
            .join(&uniq);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&proj).unwrap();
        let tmp = PathBuf::from(format!("/private/tmp/bg-trunc-{uniq}"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        Fx {
            base,
            home,
            proj,
            tmp,
            session: format!("tmp-trunc-{uniq}"),
        }
    }

    fn run(&self, command: &str) -> String {
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

// ---- RED: truncating an EXISTING file in /private/tmp must Allow -------------

#[test]
fn t1_echo_redirect_onto_existing_tmp_file_allows() {
    let f = Fx::new("t1");
    f.write(&f.tmp.join("old.txt"), "old\n");
    let out = f.run(&format!("echo a > {}/old.txt", f.tmp.display()));
    assert_allow("t1", &out);
}

#[test]
fn t2_cat_heredoc_onto_existing_tmp_file_allows() {
    let f = Fx::new("t2");
    f.write(&f.tmp.join("old.txt"), "old\n");
    let out = f.run(&format!(
        "cat > {}/old.txt <<'EOF'\nx\nEOF",
        f.tmp.display()
    ));
    assert_allow("t2", &out);
}

#[test]
fn t3_cd_tmp_then_redirect_onto_existing_allows() {
    let f = Fx::new("t3");
    f.write(&f.tmp.join("old.txt"), "old\n");
    let out = f.run(&format!("cd {} && echo a > old.txt", f.tmp.display()));
    assert_allow("t3", &out);
}

#[test]
fn t4_colon_redirect_onto_existing_nested_tmp_file_allows() {
    let f = Fx::new("t4");
    f.write(&f.tmp.join("sub/old2.txt"), "old\n");
    let out = f.run(&format!(": > {}/sub/old2.txt", f.tmp.display()));
    assert_allow("t4", &out);
}

// ---- control: new file already Allow ----------------------------------------

#[test]
fn c1_redirect_to_new_tmp_file_allows() {
    let f = Fx::new("c1");
    let out = f.run(&format!("echo a > {}/new.txt", f.tmp.display()));
    assert_allow("c1", &out);
}

// ---- guards: must stay non-Allow --------------------------------------------

#[test]
fn g1_dotdot_escape_out_of_tmp_onto_existing_outside_file_not_allow() {
    let f = Fx::new("g1");
    let victim = f.proj.join("victim.txt");
    f.write(&victim, "keep\n");
    // /private/tmp/<d>/../../<rest> : two levels up from <d> is /private, so
    // climb enough levels to reach / then spell the absolute victim path.
    let rel = victim.strip_prefix("/").unwrap();
    let out = f.run(&format!(
        "echo a > {}/../../../{}",
        f.tmp.display(),
        rel.display()
    ));
    assert_not_allow("g1", &out);
}

#[test]
fn g2_symlink_escape_onto_existing_outside_file_not_allow() {
    let f = Fx::new("g2");
    let victim = f.proj.join("victim.txt");
    f.write(&victim, "keep\n");
    symlink(&victim, f.tmp.join("lnk")).unwrap();
    let out = f.run(&format!("echo a > {}/lnk", f.tmp.display()));
    assert_not_allow("g2", &out);
}

#[test]
fn g3a_claude_settings_under_fixture_home_not_allow() {
    let f = Fx::new("g3a");
    let p = f.home.join(".claude/settings.json");
    f.write(&p, "{}\n");
    let out = f.run(&format!("echo a > {}", p.display()));
    assert_not_allow("g3a", &out);
}

#[test]
fn g3b_git_config_inside_tmp_not_allow() {
    let f = Fx::new("g3b");
    f.write(&f.tmp.join(".git/config"), "[core]\n");
    let out = f.run(&format!("echo a > {}/.git/config", f.tmp.display()));
    assert_not_allow("g3b", &out);
}

#[test]
fn g3c_githooks_inside_tmp_not_allow() {
    let f = Fx::new("g3c");
    f.write(&f.tmp.join(".githooks/pre-commit"), "#!/bin/sh\n");
    let out = f.run(&format!(
        "echo a > {}/.githooks/pre-commit",
        f.tmp.display()
    ));
    assert_not_allow("g3c", &out);
}

#[test]
fn g4_unresolved_variable_dir_not_allow() {
    let f = Fx::new("g4");
    f.write(&f.tmp.join("old.txt"), "old\n");
    let out = f.run("echo a > \"$X/old.txt\"");
    assert_not_allow("g4", &out);
}
