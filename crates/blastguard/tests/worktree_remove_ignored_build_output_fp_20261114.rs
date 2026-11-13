// このファイルは丸ごと integration test なので unwrap/expect を許可する
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Candidate false positive (observed 2026-11-14, deployed blastguard 0.2.523+,
//! subagent mode, 3 times): `git worktree remove --force <linked worktree>` is
//! denied when the worktree's tracked tree is clean and the only extra content
//! is gitignored build output (`target/`). Observed reason, verbatim:
//! "git worktree remove --force `/Users/yuki/src/harness-wt-label-fix`: git
//! status reports uncommitted changes, untracked files or ignored files there
//! (ignored files such as build output in target/ count: the forced removal
//! deletes the whole directory), which would be destroyed".
//!
//! Standing ruling: block only config/asset destruction; regenerable build
//! output in a fully clean linked worktree is not that. Expected: Allow, as
//! main and as subagent.
//!
//! Controls (must stay not-Allow): modified tracked file; untracked
//! non-ignored file; gitignored non-build file (`.env`, config/secret).
//! Decisions only; prose is never asserted. Allow is exactly empty stdout.

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
    wt: PathBuf,
    tag: String,
}

impl Fixture {
    /// A repo whose `.gitignore` lists `target/` and `.env`, plus a linked
    /// worktree (sibling of the repo) whose tracked tree is clean.
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("worktree_remove_ignored_build_output_fp_20261114")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("src/repo");
        let wt = base.join("src/repo-wt-linked");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("tracked.txt"), "x\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "target/\n.env\n").unwrap();
        git(&repo, &["add", "tracked.txt", ".gitignore"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked",
                wt.to_str().unwrap(),
            ],
        );
        assert!(wt.join("tracked.txt").exists());
        Fixture {
            base,
            repo,
            wt,
            tag: name.to_string(),
        }
    }

    fn add_build_output(&self) {
        let d = self.wt.join("target/debug");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("x"), "bin\n").unwrap();
    }

    fn cmd(&self) -> String {
        self.cmd_for(&self.wt)
    }

    fn cmd_for(&self, op: &Path) -> String {
        format!(
            "git -C {} worktree remove --force {}",
            self.repo.display(),
            op.display()
        )
    }

    fn decision(&self, role: Role) -> String {
        self.decision_for(role, &self.wt)
    }

    fn decision_for(&self, role: Role, op: &Path) -> String {
        let case = format!("c{}", SEQ.fetch_add(1, Ordering::Relaxed));
        let session = format!("bg-wtrm-{}-{case}-{}", self.tag, std::process::id());
        let home = self.base.join("homes").join(&case);
        std::fs::create_dir_all(&home).unwrap();
        let mut v = serde_json::json!({
            "session_id": session,
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": self.cmd_for(op) },
        });
        if role == Role::Sub {
            v["agent_id"] = serde_json::json!("agent-wtrm");
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
                self.base.join("store").join(&case),
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
        let out = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.trim().is_empty() {
            return "allow".to_string();
        }
        let v = serde_json::from_str::<serde_json::Value>(out.trim())
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out}"));
        v.pointer("/hookSpecificOutput/permissionDecision")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("no permissionDecision in {out}"))
    }

    fn assert_decision_all_roles(&self, want_allow: bool) {
        let mut bad = Vec::new();
        for role in [Role::Main, Role::Sub] {
            let d = self.decision(role);
            if (d == "allow") != want_allow {
                bad.push(format!("[{}] -> {d}", role.name()));
            }
        }
        assert!(
            bad.is_empty(),
            "`{}` (want {}): {}",
            self.cmd(),
            if want_allow { "allow" } else { "not-allow" },
            bad.join("; ")
        );
    }
}

#[test]
fn t1_clean_worktree_with_only_ignored_build_output_is_allow() {
    let f = Fixture::new("t1");
    f.add_build_output();
    f.assert_decision_all_roles(true);
}

/// RED (2026-11-14, flow-8cca7ef5 orchestrator — not an implementer), user
/// ruling 2026-11-14 「gateもconduktも絶対に勝手に他のworktreeを参照してはならない」
/// (an operation explicitly aimed at a named worktree is exempt): the content
/// check may inspect ONLY the worktree named in the remove command. When the
/// operand is not itself a worktree root (here: the ignored `target/` build
/// dir inside the MAIN worktree), git resolves the enclosing repository, so
/// the check reads the main worktree's state — and at main c4336b61 it then
/// answers Allow from that foreign state. Expected: not Allow (main and
/// subagent), decided without consulting any worktree other than the operand.
/// DOES NOT PROVE: that no git subprocess touched the enclosing repo (only the
/// verdict is observed).
#[ignore = "RED: worktree remove operand that is not a worktree root is judged by the enclosing (main) worktree's state"]
#[test]
fn t2_operand_inside_main_worktree_is_not_judged_by_main_state() {
    let f = Fixture::new("t2");
    let d = f.repo.join("target/debug");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("x"), "bin\n").unwrap();
    let op = f.repo.join("target");
    let mut bad = Vec::new();
    for role in [Role::Main, Role::Sub] {
        let got = f.decision_for(role, &op);
        if got == "allow" {
            bad.push(format!("[{}] -> {got}", role.name()));
        }
    }
    assert!(
        bad.is_empty(),
        "`{}` (want not-allow): {}",
        f.cmd_for(&op),
        bad.join("; ")
    );
}

#[test]
fn c0_clean_worktree_without_build_output_is_allow() {
    let f = Fixture::new("c0");
    f.assert_decision_all_roles(true);
}

#[test]
fn ca_modified_tracked_file_stays_not_allow() {
    let f = Fixture::new("ca");
    f.add_build_output();
    std::fs::write(f.wt.join("tracked.txt"), "modified\n").unwrap();
    f.assert_decision_all_roles(false);
}

#[test]
fn cb_untracked_non_ignored_file_stays_not_allow() {
    let f = Fixture::new("cb");
    f.add_build_output();
    std::fs::write(f.wt.join("new_work.txt"), "work\n").unwrap();
    f.assert_decision_all_roles(false);
}

#[test]
fn cc_ignored_env_file_stays_not_allow() {
    let f = Fixture::new("cc");
    f.add_build_output();
    std::fs::write(f.wt.join(".env"), "SECRET=1\n").unwrap();
    f.assert_decision_all_roles(false);
}
