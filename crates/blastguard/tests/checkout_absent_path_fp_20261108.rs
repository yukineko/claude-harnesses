// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction (observed 2026-11-08): `git checkout <rev> -- <path>`
//! where `<path>` does NOT exist in the working tree (and has no index entry)
//! was refused with "git checkout -- `<path>` overwrites the file from git
//! and would destroy any uncommitted changes in it, and blastguard cannot tell
//! whether it has any (git could not be asked)". There is nothing in the
//! working tree or the index at that path to destroy, so the expected verdict
//! is Allow — for the main agent and for a subagent.
//!
//! Controls that must stay not-Allow:
//!   * the path exists with uncommitted modifications;
//!   * the path is absent from disk but has a STAGED (index-only) version —
//!     `git checkout <rev> -- <path>` overwrites that index entry, so the
//!     absence on disk alone is not "nothing to lose";
//!   * git cannot be asked at all (the hook cwd is not inside a repository).
//!
//! Allow is exactly empty stdout.

#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn git_out(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git(dir: &Path, args: &[&str]) {
    let _ = git_out(dir, args);
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Main,
    Sub,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Main => "main",
            Role::Sub => "subagent",
        }
    }
}

struct Fixture {
    base: PathBuf,
    repo: PathBuf,
    tag: String,
    /// A commit in which `gone.rs` and `sub/deep.rs` exist; HEAD has neither
    /// (both removed and committed), so they are absent from disk and index.
    old: String,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("checkout_absent_path_fp_20261108")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("src/repo");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("f.txt"), "x\n").unwrap();
        std::fs::write(repo.join("gone.rs"), "fn a() {}\n").unwrap();
        std::fs::write(repo.join("sub/deep.rs"), "fn b() {}\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        let old = git_out(&repo, &["rev-parse", "--short", "HEAD"]);
        git(&repo, &["rm", "-q", "-r", "gone.rs", "sub"]);
        git(&repo, &["commit", "-q", "-m", "remove"]);
        assert!(!repo.join("gone.rs").exists());
        assert!(!repo.join("sub").exists());
        Fixture {
            base,
            repo,
            tag: name.to_string(),
            old,
        }
    }

    /// One hook call with the payload `cwd` set to `cwd`.
    fn bash_in(&self, cwd: &Path, command: &str, role: Role, case: &str) -> String {
        let session = format!("bg-coabs-{}-{case}-{}", self.tag, std::process::id());
        let home = self.base.join("homes").join(case);
        std::fs::create_dir_all(&home).unwrap();
        let mut v = serde_json::json!({
            "session_id": session,
            "hook_event_name": "PreToolUse",
            "cwd": cwd.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if role == Role::Sub {
            v["agent_id"] = serde_json::json!("agent-coabs");
            v["agent_type"] = serde_json::json!("general-purpose");
        }
        let payload = v.to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
            .env(
                "BLASTGUARD_APPROVALS_DIR",
                self.base.join("store").join(case),
            )
            .env("HOME", &home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDE_CODE_SESSION_ID", &session)
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
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

    /// `"allow"` for empty stdout, else `"<decision>: <reason>"`.
    fn decision_in(&self, cwd: &Path, command: &str, role: Role) -> String {
        let case = format!("c{}", SEQ.fetch_add(1, Ordering::Relaxed));
        let out = self.bash_in(cwd, command, role, &case);
        if out.trim().is_empty() {
            return "allow".to_string();
        }
        let v = serde_json::from_str::<serde_json::Value>(out.trim())
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out}"));
        let d = v
            .pointer("/hookSpecificOutput/permissionDecision")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("no permissionDecision in {out}"));
        let r = v
            .pointer("/hookSpecificOutput/permissionDecisionReason")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        format!("{d}: {r}")
    }

    /// Every variant, as main and as subagent, with payload cwd = repo.
    fn assert_allow(&self, group: &str, variants: &[String]) {
        let mut failing = Vec::new();
        for c in variants {
            for role in [Role::Main, Role::Sub] {
                let d = self.decision_in(&self.repo, c, role);
                if d != "allow" {
                    failing.push(format!("[{}] {c:?} -> {d}", role.name()));
                }
            }
        }
        assert!(
            failing.is_empty(),
            "{group}: {} of {} (variant x role) not Allow:\n{}",
            failing.len(),
            variants.len() * 2,
            failing.join("\n")
        );
    }

    /// Every variant, as main and as subagent, must NOT be Allow.
    fn assert_not_allow(&self, cwd: &Path, group: &str, variants: &[String]) {
        let mut failing = Vec::new();
        for c in variants {
            for role in [Role::Main, Role::Sub] {
                let d = self.decision_in(cwd, c, role);
                if d == "allow" {
                    failing.push(format!("[{}] {c:?} -> allow", role.name()));
                }
            }
        }
        assert!(
            failing.is_empty(),
            "{group}: {} of {} (variant x role) were Allowed:\n{}",
            failing.len(),
            variants.len() * 2,
            failing.join("\n")
        );
    }
}

// ---- Target: absent path => nothing to destroy => Allow ----

#[test]
fn t1_checkout_rev_absent_file_is_allow() {
    let f = Fixture::new("t1");
    let old = &f.old;
    f.assert_allow(
        "t1 checkout <rev> -- <absent file>",
        &[
            format!("git checkout {old} -- gone.rs"),
            format!("git checkout {old} -- ./gone.rs"),
            format!("git checkout {old} -- {}", f.repo.join("gone.rs").display()),
        ],
    );
}

#[test]
fn t2_checkout_rev_absent_file_in_absent_dir_is_allow() {
    let f = Fixture::new("t2");
    let old = &f.old;
    f.assert_allow(
        "t2 checkout <rev> -- <file under absent dir>",
        &[format!("git checkout {old} -- sub/deep.rs")],
    );
}

#[test]
fn t3_observed_chain_shape_is_allow() {
    let f = Fixture::new("t3");
    let old = &f.old;
    f.assert_allow(
        "t3 observed chain",
        &[format!(
            "git status --short && git log --oneline -3 && git checkout {old} -- gone.rs && \
             git status --short"
        )],
    );
}

// ---- Controls: must stay not-Allow ----

#[test]
fn c1_existing_file_with_uncommitted_changes_is_not_allow() {
    let f = Fixture::new("c1");
    std::fs::write(f.repo.join("f.txt"), "modified\n").unwrap();
    let old = &f.old;
    f.assert_not_allow(
        &f.repo,
        "c1 modified file",
        &[
            format!("git checkout {old} -- f.txt"),
            "git checkout HEAD -- f.txt".to_string(),
            format!(
                "git status --short && git log --oneline -3 && git checkout {old} -- f.txt && \
                 git status --short"
            ),
        ],
    );
}

#[test]
fn c2_absent_on_disk_but_staged_is_not_allow() {
    let f = Fixture::new("c2");
    std::fs::write(f.repo.join("gone.rs"), "staged only\n").unwrap();
    git(&f.repo, &["add", "gone.rs"]);
    std::fs::remove_file(f.repo.join("gone.rs")).unwrap();
    assert!(!f.repo.join("gone.rs").exists());
    let old = &f.old;
    f.assert_not_allow(
        &f.repo,
        "c2 staged-only content at an absent path",
        &[format!("git checkout {old} -- gone.rs")],
    );
}

#[test]
fn c3_not_a_repo_is_not_allow() {
    let f = Fixture::new("c3");
    // NOT under the fixture base: that base physically lives in the cargo
    // target dir, which is inside a git checkout (observed: a `plain` dir
    // there made git answer `!!` for x.txt and the hook Allowed). The system
    // temp dir is outside every repository.
    let plain = std::env::temp_dir().join(format!(
        "bg-coabs-c3-plain-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&plain);
    std::fs::create_dir_all(&plain).unwrap();
    {
        let mut probe = git_env::pinned(&plain);
        probe.args(["rev-parse", "--show-toplevel"]);
        let out = bounded::run_bounded_stdin(
            &mut probe,
            bounded::ChildStdin::Null,
            bounded::TOOL_DEADLINE,
        );
        assert!(
            !out.status.success(),
            "c3 precondition: {} must not be inside a git repository",
            plain.display()
        );
    }
    std::fs::write(plain.join("x.txt"), "local\n").unwrap();
    f.assert_not_allow(
        &plain,
        "c3 cwd not a repository",
        &[
            "git checkout HEAD -- x.txt".to_string(),
            "git checkout HEAD -- absent.txt".to_string(),
        ],
    );
}
