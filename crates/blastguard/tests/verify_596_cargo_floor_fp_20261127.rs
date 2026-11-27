#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier FP probes (2026-11-27) of the line-level cargo floor
//! (`src/detect/ast/cargo_floor.rs`, commit ca60c0f0a, backlog 596ef57d):
//! ordinary lines that write something and run cargo must stay Allow. One case
//! per test so a failing case can be ignored on its own. Written by a
//! verifier, not the implementer (CLAUDE.md 2(a)).

#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = git_env::pinned(dir);
    cmd.args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "init.defaultBranch=main",
    ])
    .args(args)
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_NOSYSTEM", "1");
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(out.status.success(), "git {args:?} failed");
}

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    session: String,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("verify_596_cargo_floor_fp_20261127")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("f.txt"), "x\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Fixture {
            base,
            home,
            repo,
            session: format!("v596fp-{name}-{}", std::process::id()),
        }
    }

    fn decision(&self, command: &str, subagent: bool) -> String {
        let command = command.replace("{DIR}", &self.repo.display().to_string());
        let mut payload = serde_json::json!({
            "session_id": format!("{}-{}", self.session, if subagent { "sub" } else { "main" }),
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("agent-v596fp");
            payload["agent_type"] = serde_json::json!("general-purpose");
        }
        let session = payload["session_id"].as_str().unwrap().to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDE_CODE_SESSION_ID", &session)
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .current_dir(&self.base);
        let out = bounded::run_bounded(
            &mut cmd,
            payload.to_string().as_bytes(),
            bounded::HOOK_DEADLINE,
        );
        let out = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.trim().is_empty() {
            return "allow".to_string();
        }
        let v = serde_json::from_str::<serde_json::Value>(out.trim()).unwrap();
        v.pointer("/hookSpecificOutput/permissionDecision")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "nodecision".into())
    }
}

/// `cmd` is Allow in both main and subagent mode.
fn allow(name: &str, cmd: &str) {
    let f = Fixture::new(name);
    let main = f.decision(cmd, false);
    let sub = f.decision(cmd, true);
    eprintln!("PROBE {name}: main={main} sub={sub}");
    assert!(
        main == "allow" && sub == "allow",
        "{name}: main={main} sub={sub}"
    );
}

const LIB: &str = "pub fn f() -> u32 { 1 }";

#[test]
fn fp_echo_to_var_then_cargo_test() {
    allow(
        "echo_var_cargo_test",
        "F={DIR}/notes.txt; echo x > \"$F\" && cargo test",
    );
}

#[test]
#[ignore = "RED(596-verify): observed main=deny sub=deny at ca60c0f0a; layer not attributed"]
fn fp_echo_to_unset_var_then_cargo_test() {
    allow("echo_unset_var_cargo_test", "echo x > \"$F\" && cargo test");
}

#[test]
fn fp_heredoc_lib_rs_then_cargo_test() {
    allow(
        "heredoc_lib_cargo_test",
        &format!("cat > {{DIR}}/src/lib.rs <<'EOF'\n{LIB}\nEOF\ncargo test"),
    );
}

#[test]
fn fp_printf_lib_rs_then_cargo_test() {
    allow(
        "printf_lib_cargo_test",
        &format!("printf '%s\\n' '{LIB}' > {{DIR}}/src/lib.rs && cargo test"),
    );
}

#[test]
fn fp_tee_notes_then_cargo_build() {
    allow(
        "tee_notes_cargo_build",
        "echo x | tee notes.txt && cargo build",
    );
}

#[test]
fn fp_cargo_test_piped_to_tee_log() {
    allow("cargo_test_tee_log", "cargo test 2>&1 | tee {DIR}/test.log");
}

#[test]
#[ignore = "RED(596-verify): observed main=deny sub=deny at ca60c0f0a; layer not attributed"]
fn fp_cargo_test_piped_to_tee_var_log() {
    allow("cargo_test_tee_var_log", "cargo test 2>&1 | tee \"$LOG\"");
}

#[test]
#[ignore = "RED(596-verify): observed main=deny sub=deny at ca60c0f0a; layer not attributed"]
fn fp_cargo_test_redirect_var_log() {
    allow("cargo_test_redirect_var_log", "cargo test > \"$LOG\" 2>&1");
}

#[test]
fn fp_cargo_offline_test_after_lib_rs() {
    allow(
        "cargo_offline_test_lib",
        &format!("cat > {{DIR}}/src/lib.rs <<'EOF'\n{LIB}\nEOF\ncargo --offline test"),
    );
}

#[test]
fn fp_cargo_toml_edit_then_cargo_test() {
    allow(
        "cargo_toml_cargo_test",
        "printf '[dependencies]\\n' >> {DIR}/Cargo.toml && cargo test",
    );
}

#[test]
fn fp_cp_lib_rs_then_cargo_build() {
    allow(
        "cp_lib_cargo_build",
        "cp {DIR}/f.txt {DIR}/src/lib.rs && cargo build",
    );
}

#[test]
fn fp_echo_config_txt_then_cargo_check() {
    allow(
        "echo_config_txt_cargo_check",
        "echo x > {DIR}/docs/config.txt && cargo check",
    );
}

#[test]
fn fp_cargo_run_redirect_out_txt() {
    allow("cargo_run_out_txt", "cargo run > {DIR}/out.txt");
}
