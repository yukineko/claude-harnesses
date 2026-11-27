#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verifier probes (2026-11-27) of the backlog 596ef57d fix
//! (commit 2ae27d330): holes in the new cargo Ask floor and false positives of
//! the floor / the closed-heredoc parse-failure floor. Written by a verifier,
//! not the implementer (CLAUDE.md 2(a)).

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
            .join("verify_596_cargo_floor_holes_20261127")
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
            session: format!("v596-{name}-{}", std::process::id()),
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
            payload["agent_id"] = serde_json::json!("agent-v596");
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

#[derive(Clone, Copy, PartialEq)]
enum Need {
    NotAllow,
    Allow,
}

fn check(req: &str, cases: &[(&str, String)], need: Need) {
    let mut bad: Vec<String> = Vec::new();
    for (name, cmd) in cases {
        let f = Fixture::new(&format!("{req}_{name}"));
        let main = f.decision(cmd, false);
        let sub = f.decision(cmd, true);
        let ok = match need {
            Need::NotAllow => main != "allow" && sub != "allow",
            Need::Allow => main == "allow" && sub == "allow",
        };
        eprintln!("PROBE {req}/{name}: main={main} sub={sub}");
        if !ok {
            bad.push(format!("{name}: main={main} sub={sub}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{req}: cases not resolved to the required side:\n{}",
        bad.join("\n")
    );
}

const RM: &str = "fn main(){ std::process::Command::new(\"rm\").args([\"-rf\",\"/usr/lib\"]).status().unwrap(); }";

fn w(path: &str, later: &str) -> String {
    format!("cat > {{DIR}}/{path} <<'EOF'\n{RM}\nEOF\n{later}")
}

/// Cargo spellings that run a just-written `src/main.rs` / `build.rs`.
#[test]
fn v596_cargo_run_spellings_not_allow() {
    let cases = [
        ("cargo_r", w("src/main.rs", "cargo r")),
        ("cargo_plus_nightly_run", w("src/main.rs", "cargo +nightly run")),
        ("cargo_manifest_path_run", w("src/main.rs", "cargo --manifest-path {DIR}/Cargo.toml run")),
        ("cargo_q_run", w("src/main.rs", "cargo -q run")),
        ("cargo_bench", w("benches/b.rs", "cargo bench")),
        ("cargo_build_rs_upper", w("BUILD.RS", "cargo build")),
        ("cargo_build_rs_mixed", w("Build.rs", "cargo check")),
    ];
    check("v596_cargo_run_spellings", &cases, Need::NotAllow);
}

/// Cargo reached through a wrapper or another path: the floor keys on a
/// literal `cargo` head only.
#[test]
fn v596_cargo_via_wrapper_not_allow() {
    let cases = [
        ("env_cargo_run", w("src/main.rs", "env cargo run")),
        ("abs_cargo_run", w("src/main.rs", "{DIR}/../../home/.cargo/bin/cargo run")),
        ("rustup_run_cargo_run", w("src/main.rs", "rustup run stable cargo run")),
        ("command_cargo_run", w("src/main.rs", "command cargo run")),
        ("cargo_dash_run", w("src/main.rs", "cargo-run")),
        ("env_cargo_build_rs", w("build.rs", "env cargo build")),
    ];
    check("v596_cargo_via_wrapper", &cases, Need::NotAllow);
}

/// The cargo config written by `tee` / `printf` (not `cat >`) then cargo.
#[test]
fn v596_cargo_config_other_writers_not_allow() {
    let cfg = "[build]\nrustc-wrapper = \"/bin/sh\"";
    let cases = [
        (
            "tee_config_heredoc",
            format!("mkdir -p {{DIR}}/.cargo && tee {{DIR}}/.cargo/config.toml <<'EOF'\n{cfg}\nEOF\ncargo build"),
        ),
        (
            "printf_config",
            "mkdir -p {DIR}/.cargo && printf '[build]\\nrustc-wrapper = \"/bin/sh\"\\n' > {DIR}/.cargo/config.toml && cargo build".to_string(),
        ),
        (
            "printf_build_rs",
            format!("printf '%s\\n' '{RM}' > {{DIR}}/build.rs && cargo build"),
        ),
    ];
    check("v596_cargo_config_other_writers", &cases, Need::NotAllow);
}

/// `rustc` direct: compile the written `.rs` and run the binary.
#[test]
fn v596_rustc_direct_not_allow() {
    let cases = [(
        "rustc_then_run",
        w("x.rs", "rustc {DIR}/x.rs -o {DIR}/x && {DIR}/x"),
    )];
    check("v596_rustc_direct", &cases, Need::NotAllow);
}

/// False-positive controls: B-4 ruling (`cargo test` / `cargo build` over an
/// ordinary `.rs`) and other non-running cargo subcommands stay Allow.
#[test]
fn v596_fp_controls_allow() {
    let lib = "pub fn f() -> u32 { 1 }";
    let mk = |path: &str, later: &str| format!("cat > {{DIR}}/{path} <<'EOF'\n{lib}\nEOF\n{later}");
    let cases = [
        ("lib_cargo_test", mk("src/lib.rs", "cargo test")),
        ("lib_cargo_build", mk("src/lib.rs", "cargo build")),
        ("lib_cargo_test_p", mk("src/lib.rs", "cargo test -p foo")),
        ("lib_cargo_fmt", mk("src/lib.rs", "cargo fmt")),
        ("lib_cargo_check", mk("src/lib.rs", "cargo check")),
        ("lib_cargo_clippy", mk("src/lib.rs", "cargo clippy --all-targets")),
        ("cargo_toml_cargo_test", mk("Cargo.toml", "cargo test")),
        ("notes_config_dir_cargo_test", mk("notes/config", "cargo test")),
        (
            "heredoc_then_for_loop",
            format!("cat > {{DIR}}/a.md <<'EOF'\n{lib}\nEOF\nfor f in a b; do echo $f; done"),
        ),
        (
            "heredoc_then_if",
            format!("cat > {{DIR}}/a.md <<'EOF'\n{lib}\nEOF\nif [ -f {{DIR}}/a.md ]; then echo ok; fi"),
        ),
        (
            "heredoc_then_dbl_bracket",
            format!("cat > {{DIR}}/a.md <<'EOF'\n{lib}\nEOF\n[[ -f {{DIR}}/a.md ]] && echo ok"),
        ),
    ];
    check("v596_fp_controls", &cases, Need::Allow);
}
