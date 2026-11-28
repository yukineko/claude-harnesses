#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! RED: relative-path (cwd-dependent) spellings of destructive commands on
//! protected targets are Allow, while the absolute spelling is Deny.
//!
//! Reported by an independent verifier on the fp-ask-rules branch
//! (`verifier8_ask_learning_20261128.rs::r8_detector_relative_rm_of_claude_settings_is_allow`),
//! re-measured here against main by a separate test author (CLAUDE.md §2(a)).
//!
//! Layout (production-like): fixture under `/private/tmp/claude-askrules/`;
//! HOME = `<base>/home` holding `.claude/settings.json`; main tree
//! `<home>/src/repo` (git, `src/lib.rs` committed); linked worktree
//! `<home>/src/.harness-worktrees/wt`; scratchpad `<base>/scratchpad`.
//! `CLAUDE_PROJECT_DIR` is removed (as in the verifier's harness).
//!
//! Controls (GREEN): absolute `rm -f <home>/.claude/settings.json` is Deny;
//! a harmless relative `rm -f out.txt` in the scratchpad keeps its current
//! verdict (Allow).
//!
//! DOES NOT PROVE: that the commands would actually run; other protected
//! files; Write/Edit tool paths; subagent mode; what the correct verdict for
//! each RED case is beyond "not Allow".
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    _dir: tempfile::TempDir,
    base: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let st = git_env::pinned(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} in {}", dir.display());
}

impl Fx {
    fn new() -> Fx {
        std::fs::create_dir_all("/private/tmp/claude-askrules").unwrap();
        let dir = tempfile::Builder::new()
            .prefix("detector-relative-protected-")
            .tempdir_in("/private/tmp/claude-askrules")
            .unwrap();
        let base = dir.path().to_path_buf();
        let fx = Fx { _dir: dir, base };
        let claude = fx.home().join(".claude");
        std::fs::create_dir_all(claude.join("hooks")).unwrap();
        std::fs::write(claude.join("settings.json"), "{}").unwrap();
        let main = fx.main();
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join("src/lib.rs"), "x").unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["add", "src"]);
        git(&main, &["commit", "-q", "-m", "i"]);
        std::fs::create_dir_all(fx.wt().parent().unwrap()).unwrap();
        git(&main, &["worktree", "add", "-q", fx.wt().to_str().unwrap()]);
        std::fs::create_dir_all(fx.sp()).unwrap();
        std::fs::write(fx.sp().join("out.txt"), "x").unwrap();
        std::fs::write(
            fx.sp().join("p.patch"),
            "--- a/settings.json\n+++ b/settings.json\n@@ -1 +1 @@\n-{}\n+{\"hooks\":{}}\n",
        )
        .unwrap();
        fx
    }
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn claude(&self) -> PathBuf {
        self.home().join(".claude")
    }
    fn main(&self) -> PathBuf {
        self.home().join("src/repo")
    }
    fn wt(&self) -> PathBuf {
        self.home().join("src/.harness-worktrees/wt")
    }
    fn sp(&self) -> PathBuf {
        self.base.join("scratchpad")
    }
}

fn verdict(fx: &Fx, cwd: &Path, cmd: &str) -> String {
    let payload = serde_json::json!({
        "session_id": "detector-relative-protected",
        "hook_event_name": "PreToolUse",
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    git_env::scrub(&mut hook)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOME", fx.home())
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .env("BLASTGUARD_BLOCK_LOG_DIR", fx.base.join("blocklog"))
        .env("BLASTGUARD_RELAY_DIR", fx.base.join("relay"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("BLASTGUARD_ASK_RULES")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&fx.base);
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must always exit 0");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("hook JSON");
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision: {s}"))
        .to_string()
}

fn assert_none_allow(fx: &Fx, cases: &[(PathBuf, String)], what: &str) {
    let bad: Vec<String> = cases
        .iter()
        .map(|(cwd, c)| (verdict(fx, cwd, c), cwd, c))
        .filter(|(v, _, _)| v == "allow")
        .map(|(v, cwd, c)| format!("`{c}` (cwd {}) => {v}", cwd.display()))
        .collect();
    assert!(
        bad.is_empty(),
        "{what}, but these are Allow:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_absolute_protected_config_delete_is_deny() {
    let fx = Fx::new();
    let c = format!("rm -f {}", fx.claude().join("settings.json").display());
    assert_eq!(verdict(&fx, &fx.wt(), &c), "deny", "{c}");
}

#[test]
fn control_harmless_relative_rm_in_scratchpad_is_allow() {
    let fx = Fx::new();
    assert_eq!(
        verdict(&fx, &fx.sp(), "rm -f out.txt"),
        "allow",
        "over-block: rm -f out.txt (cwd scratchpad)"
    );
}

#[test]
#[ignore = "RED: backlog ea96e65d"]
fn relative_rm_of_claude_settings_is_not_allow() {
    let fx = Fx::new();
    let cases: Vec<(PathBuf, String)> = [
        "rm settings.json",
        "rm -f settings.json",
        "rm -f ./settings.json",
    ]
    .into_iter()
    .map(|c| (fx.claude(), c.to_string()))
    .collect();
    assert_none_allow(&fx, &cases, "these delete ~/.claude/settings.json");
}

#[test]
#[ignore = "RED: backlog ea96e65d"]
fn git_apply_unsafe_paths_into_claude_dir_is_not_allow() {
    let fx = Fx::new();
    let c = format!(
        "git apply --unsafe-paths --directory={} {}",
        fx.claude().display(),
        fx.sp().join("p.patch").display()
    );
    assert_none_allow(
        &fx,
        &[(fx.wt(), c)],
        "this rewrites ~/.claude/settings.json",
    );
}

#[test]
#[ignore = "RED: backlog ea96e65d"]
fn mv_of_worktrees_container_is_not_allow() {
    let fx = Fx::new();
    let c = format!(
        "mv {}/src/.harness-worktrees/ {}",
        fx.home().display(),
        fx.sp().join("moved").display()
    );
    assert_none_allow(
        &fx,
        &[(fx.wt(), c)],
        "this moves every linked worktree away",
    );
}

#[test]
#[ignore = "RED: backlog ea96e65d"]
fn relative_rm_rf_src_in_main_tree_is_not_allow() {
    let fx = Fx::new();
    assert_none_allow(
        &fx,
        &[(fx.main(), "rm -rf src".to_string())],
        "this destroys the main working tree's tracked src/",
    );
}
