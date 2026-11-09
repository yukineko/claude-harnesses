// Integration test file: unwrap/expect allowed (workspace lints are production-only).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent-test RED/GREEN for `split_segments` breaking an unquoted closed
//! process substitution `<(...)` / `>(...)` at a `;` or newline inside it, so
//! the body escaped judgment. Hook binary is the REAL one. The payload
//! `rm -rf /etc` is JSON data, never executed.
//!
//! Shell semantics: `<(list)` / `>(list)` run `list` as a full command list, so
//! `;`/newline/`&&` inside are separators of the INNER list and every inner
//! command must be judged. Inside double quotes `<(` is literal text.

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
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(p: &Path, content: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    /// Primary working tree, on `main`; also CLAUDE_PROJECT_DIR.
    repo: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("procsub_segment_split_failopen_20261108")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(base.join("victim")).unwrap();

        git(&repo, &["init", "-q"]);
        write(&repo.join("tracked.txt"), "one\n");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        // A staged change, as a real commit would have.
        write(&repo.join("tracked.txt"), "two\n");
        git(&repo, &["add", "tracked.txt"]);

        Fixture { base, home, repo }
    }

    /// `git -C <repo> commit` prefix.
    #[allow(dead_code)]
    fn commit(&self) -> String {
        format!("git -C {} commit", self.repo.display())
    }

    /// Hook stdout for a Bash `command` (cwd = CLAUDE_PROJECT_DIR = primary).
    fn bash(&self, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": "procsub-segment-split",
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
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
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// `"allow"` (empty stdout), `"deny"`, `"ask"`, or `"other"`.
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

#[track_caller]
fn run(name: &str, cases: &[(&str, &str)], want: &str) {
    let f = Fixture::new(name);
    let mut bad = Vec::new();
    for (label, cmd) in cases {
        let out = f.bash(cmd);
        let got = verdict(&out);
        eprintln!("VERDICT [{label}] want {want} got {got}");
        if got != want {
            bad.push(format!(
                "  [{label}] want {want}, got {got}\n    command: {cmd:?}"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} cases wrong:\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}

#[test]
fn procsub_body_with_separator_is_judged() {
    run(
        "deny",
        &[
            ("trailing semicolon", "cat <(rm -rf /etc;)"),
            ("newline before close", "echo x <(rm -rf /etc\n)"),
            (
                "out-procsub newline EOF newline",
                "echo x >(rm -rf /etc\nEOF\n)",
            ),
            ("out-procsub and-and", "echo x >(true && rm -rf /etc;)"),
            (
                "nested in command substitution",
                "echo $( cat <(rm -rf /etc;) )",
            ),
            ("payload second after semicolon", "cat <(true; rm -rf /etc)"),
            ("payload after the procsub", "diff <(ls a;) b; rm -rf /etc"),
        ],
        "deny",
    );
}

#[test]
fn benign_procsub_and_quoted_literal_stay_allow() {
    run(
        "allow",
        &[
            ("plain diff", "diff <(ls a) <(ls b)"),
            (
                "benign separators inside",
                "diff <(ls a; ls b) <(ls c\nls d)",
            ),
            ("dq literal with payload", "echo \"<(rm -rf /etc;)\""),
            ("dq literal out-form", "echo \">(rm -rf /etc\n)\""),
        ],
        "allow",
    );
}
