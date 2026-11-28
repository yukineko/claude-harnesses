#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Independent verifier tests for backlog ea96e65d (blastguard 0.2.1008):
//! relative protected targets, `git apply --directory`, `mv` of worktrees and
//! the main-tree tracked recursive delete. Written by a different agent than
//! the implementer (CLAUDE.md §2(a)).
//!
//! Layout (production-like): fixture under `/private/tmp/claude-askrules/`;
//! HOME = `<base>/home` holding `.claude/{settings.json,hooks/pre.sh,x/}`;
//! main tree `<home>/src/repo` (git, `src/lib.rs` + `src/a/b.rs` committed)
//! with linked worktree `<home>/src/.harness-worktrees/wt`; a lone repo
//! `<home>/src/lone` (no linked worktree); a `--separate-git-dir` primary
//! checkout `<home>/src/sep` with linked worktree `.harness-worktrees/sepwt`;
//! scratchpad `<base>/scratchpad`. `CLAUDE_PROJECT_DIR` is removed.
//!
//! The "no over-block" controls assert the verdict the pre-fix binary
//! (deployed blastguard 0.2.1005, measured 2026-11-28 by this verifier
//! against the same fixture) gave, so a tightening there is a regression.
//!
//! The `#[ignore = "RED observed ..."]` tests are fail-opens found by this
//! verifier; for each git-apply one, real git (2026-11-28, scratch dir) was
//! observed to rewrite `settings.json` with exactly that spelling.
//!
//! DOES NOT PROVE: that the hook runs in production for these commands;
//! Write/Edit tool paths; subagent / flow-driver modes; other protected files
//! than `settings.json` / `hooks/pre.sh`; what the right verdict is beyond
//! "not Allow" for the RED cases.
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::os::unix::fs::PermissionsExt;
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

fn commit_src(repo: &Path) {
    git(repo, &["add", "src"]);
    git(repo, &["commit", "-q", "-m", "i"]);
}

impl Fx {
    fn new() -> Fx {
        std::fs::create_dir_all("/private/tmp/claude-askrules").unwrap();
        let dir = tempfile::Builder::new()
            .prefix("relative-protected-verifier-")
            .tempdir_in("/private/tmp/claude-askrules")
            .unwrap();
        let base = dir.path().to_path_buf();
        let fx = Fx { _dir: dir, base };
        let claude = fx.claude();
        std::fs::create_dir_all(claude.join("hooks")).unwrap();
        std::fs::create_dir_all(claude.join("x")).unwrap();
        std::fs::write(claude.join("settings.json"), "{}").unwrap();
        std::fs::write(claude.join("hooks/pre.sh"), "#!/bin/sh\n").unwrap();

        let main = fx.main();
        std::fs::create_dir_all(main.join("src/a")).unwrap();
        std::fs::write(main.join("src/lib.rs"), "x").unwrap();
        std::fs::write(main.join("src/a/b.rs"), "x").unwrap();
        git(&main, &["init", "-q"]);
        commit_src(&main);
        std::fs::create_dir_all(fx.store()).unwrap();
        git(&main, &["worktree", "add", "-q", fx.wt().to_str().unwrap()]);
        std::fs::write(fx.wt().join("notes.txt"), "x").unwrap();

        let lone = fx.lone();
        std::fs::create_dir_all(lone.join("src")).unwrap();
        std::fs::write(lone.join("src/lib.rs"), "x").unwrap();
        git(&lone, &["init", "-q"]);
        commit_src(&lone);

        let sep = fx.sep();
        std::fs::create_dir_all(sep.join("src")).unwrap();
        std::fs::write(sep.join("src/lib.rs"), "x").unwrap();
        let gd = fx.base.join("sep-gitdir");
        git(
            &sep,
            &["init", "-q", "--separate-git-dir", gd.to_str().unwrap()],
        );
        commit_src(&sep);
        let sepwt = fx.store().join("sepwt");
        git(&sep, &["worktree", "add", "-q", sepwt.to_str().unwrap()]);

        let sp = fx.sp();
        std::fs::create_dir_all(sp.join("build")).unwrap();
        std::fs::write(sp.join("build/o"), "x").unwrap();
        std::fs::write(sp.join("out.txt"), "x").unwrap();
        std::fs::write(sp.join("settings.json"), "x").unwrap();
        std::fs::write(
            sp.join("p.patch"),
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
    fn src(&self) -> PathBuf {
        self.home().join("src")
    }
    fn main(&self) -> PathBuf {
        self.src().join("repo")
    }
    fn lone(&self) -> PathBuf {
        self.src().join("lone")
    }
    fn sep(&self) -> PathBuf {
        self.src().join("sep")
    }
    fn store(&self) -> PathBuf {
        self.src().join(".harness-worktrees")
    }
    fn wt(&self) -> PathBuf {
        self.store().join("wt")
    }
    fn sp(&self) -> PathBuf {
        self.base.join("scratchpad")
    }
    fn patch(&self) -> String {
        self.sp().join("p.patch").display().to_string()
    }
}

fn d(p: &Path) -> String {
    p.display().to_string()
}

fn verdict(fx: &Fx, cwd: &Path, cmd: &str) -> String {
    let payload = serde_json::json!({
        "session_id": "relative-protected-verifier",
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

/// Every `(cwd, cmd, expected)` must give exactly `expected`; all mismatches
/// are reported together.
fn assert_verdicts(fx: &Fx, cases: &[(PathBuf, String, &str)], what: &str) {
    let bad: Vec<String> = cases
        .iter()
        .filter_map(|(cwd, c, want)| {
            let got = verdict(fx, cwd, c);
            (got != *want).then(|| {
                format!(
                    "`{c}` (cwd {}) => {got}, want {want}",
                    cwd.strip_prefix(&fx.base).unwrap_or(cwd).display()
                )
            })
        })
        .collect();
    assert!(bad.is_empty(), "{what}:\n{}", bad.join("\n"));
}

/// Every `(cwd, cmd)` must be anything but Allow.
fn assert_none_allow(fx: &Fx, cases: &[(PathBuf, String)], what: &str) {
    let bad: Vec<String> = cases
        .iter()
        .filter(|(cwd, c)| verdict(fx, cwd, c) == "allow")
        .map(|(cwd, c)| {
            format!(
                "`{c}` (cwd {}) => allow",
                cwd.strip_prefix(&fx.base).unwrap_or(cwd).display()
            )
        })
        .collect();
    assert!(bad.is_empty(), "{what}:\n{}", bad.join("\n"));
}

// ---------------------------------------------------------------- git apply

#[test]
fn git_apply_directory_spellings_into_claude_are_ask() {
    let fx = Fx::new();
    let p = fx.patch();
    let wt = fx.wt();
    let cases = [
        // space-separated value
        (
            wt.clone(),
            format!("git apply --directory {} {p}", d(&fx.claude())),
            "ask",
        ),
        // `-C` moves the base the relative value is placed against
        (
            wt.clone(),
            format!("git -C {} apply --directory=.claude {p}", d(&fx.home())),
            "ask",
        ),
        (
            wt.clone(),
            format!("git -C {} apply --directory=. {p}", d(&fx.claude())),
            "ask",
        ),
    ];
    assert_verdicts(&fx, &cases, "git apply --directory into ~/.claude");
}

#[test]
fn git_apply_unsafe_paths_alone_is_ask() {
    let fx = Fx::new();
    let p = fx.patch();
    assert_verdicts(
        &fx,
        &[(fx.wt(), format!("git apply --unsafe-paths {p}"), "ask")],
        "--unsafe-paths without --directory",
    );
}

#[test]
fn git_apply_harmless_spellings_keep_pre_fix_allow() {
    let fx = Fx::new();
    let p = fx.patch();
    let wt = fx.wt();
    let cases = [
        (wt.clone(), format!("git apply {p}"), "allow"),
        (
            wt.clone(),
            format!("git apply --check --directory={} {p}", d(&fx.claude())),
            "allow",
        ),
        (
            wt.clone(),
            format!("git apply --directory={} {p}", d(&fx.sp())),
            "allow",
        ),
    ];
    assert_verdicts(&fx, &cases, "over-block on harmless git apply");
}

#[test]
fn red_git_apply_readonly_flag_then_apply_is_not_allow() {
    let fx = Fx::new();
    let p = fx.patch();
    let c = d(&fx.claude());
    let cases: Vec<(PathBuf, String)> = [
        format!("git apply --stat --apply --directory={c} {p}"),
        format!("git apply --check --apply --directory={c} {p}"),
        format!("git apply --numstat --apply --directory {c} {p}"),
    ]
    .into_iter()
    .map(|s| (fx.wt(), s))
    .collect();
    assert_none_allow(&fx, &cases, "these rewrite ~/.claude/settings.json");
}

#[test]
fn red_git_apply_abbreviated_directory_option_is_not_allow() {
    let fx = Fx::new();
    let p = fx.patch();
    let cases = [(fx.wt(), format!("git apply --dir={} {p}", d(&fx.claude())))];
    assert_none_allow(&fx, &cases, "this rewrites ~/.claude/settings.json");
}

#[test]
fn red_git_apply_in_protected_dir_is_not_allow() {
    let fx = Fx::new();
    let p = fx.patch();
    let cases = [
        (fx.claude(), format!("git apply {p}")),
        (fx.wt(), format!("git -C {} apply {p}", d(&fx.claude()))),
    ];
    assert_none_allow(&fx, &cases, "these rewrite ~/.claude/settings.json");
}

// ---------------------------------------------------------------------- mv

#[test]
fn mv_of_worktree_store_or_checkout_is_ask() {
    let fx = Fx::new();
    let m = d(&fx.sp().join("m"));
    let cases = [
        // glob source, placed by its literal prefix
        (fx.src(), format!("mv .harness-worktrees/* {m}"), "ask"),
        // relative linked checkout from the storage root
        (fx.store(), format!("mv wt {m}"), "ask"),
        (fx.wt(), format!("mv {}/ {m}", d(&fx.wt())), "ask"),
        (
            fx.wt(),
            format!("mv -t {} {}", d(&fx.sp()), d(&fx.store())),
            "ask",
        ),
    ];
    assert_verdicts(&fx, &cases, "mv of worktrees");
}

#[test]
fn mv_ordinary_files_keep_pre_fix_allow() {
    let fx = Fx::new();
    let cases = [
        (fx.wt(), "mv notes.txt n2.txt".to_string(), "allow"),
        (fx.wt(), "mv src s2".to_string(), "allow"),
        (fx.sp(), "mv out.txt o2.txt".to_string(), "allow"),
        (fx.sp(), "mv settings.json s.json".to_string(), "allow"),
    ];
    assert_verdicts(&fx, &cases, "over-block on ordinary mv");
}

#[test]
fn red_mv_of_worktree_store_container_is_not_allow() {
    let fx = Fx::new();
    let cases = [(
        fx.wt(),
        format!("mv {} {}", d(&fx.src()), d(&fx.sp().join("m"))),
    )];
    assert_none_allow(&fx, &cases, "this moves .harness-worktrees away");
}

// ------------------------------------------------- relative protected rm/mv

#[test]
fn relative_protected_spellings_are_deny() {
    let fx = Fx::new();
    let c = fx.claude();
    let hooks = c.join("hooks");
    let cases = [
        (hooks.clone(), "rm ../settings.json".to_string(), "deny"),
        (c.clone(), "rm ./x/../settings.json".to_string(), "deny"),
        (hooks.clone(), "rm pre.sh".to_string(), "deny"),
        (c.clone(), "rm -r hooks".to_string(), "deny"),
        (c.clone(), "rm -rf hooks/".to_string(), "deny"),
        (c.clone(), "mv settings.json x".to_string(), "deny"),
        (c.clone(), "mv hooks h2".to_string(), "deny"),
        (c.clone(), "unlink settings.json".to_string(), "deny"),
        (c.clone(), "rm -- settings.json".to_string(), "deny"),
        (c.clone(), "cd hooks && rm pre.sh".to_string(), "deny"),
        (c.clone(), "chmod -x hooks/pre.sh".to_string(), "deny"),
    ];
    assert_verdicts(&fx, &cases, "relative spellings of protected targets");
}

#[test]
fn relative_rm_of_ordinary_files_keeps_pre_fix_verdict() {
    let fx = Fx::new();
    let cases = [
        (fx.wt(), "rm -f notes.txt".to_string(), "allow"),
        (fx.wt(), "rm -rf src".to_string(), "allow"),
        (fx.sp(), "rm -f out.txt".to_string(), "allow"),
        (fx.sp(), "rm -f settings.json".to_string(), "allow"),
        (fx.sp(), "rm -rf build".to_string(), "ask"),
        // lone repo (no linked worktree): rm_by_target class 5 unchanged
        (fx.lone(), "rm -rf src".to_string(), "allow"),
        (fx.lone(), "rm -r src/".to_string(), "allow"),
    ];
    assert_verdicts(&fx, &cases, "over-block on ordinary relative rm");
}

#[test]
fn red_quoted_relative_rm_of_settings_is_not_allow() {
    let fx = Fx::new();
    let cases = [
        (fx.claude(), "rm -f \"settings.json\"".to_string()),
        (fx.claude(), "rm -f 'settings.json'".to_string()),
        (fx.claude().join("hooks"), "rm \"pre.sh\"".to_string()),
    ];
    assert_none_allow(&fx, &cases, "these delete protected files");
}

// --------------------------------------------------- main tree tracked rm

#[test]
fn main_tree_tracked_recursive_rm_is_not_allow() {
    let fx = Fx::new();
    let m = fx.main();
    let cases = [
        (m.clone(), "rm -rf ./src".to_string()),
        (m.clone(), "rm -r src/".to_string()),
        (m.clone(), "rm -rf src/a".to_string()),
        (m.join("src"), "rm -rf a".to_string()),
        (fx.wt(), format!("rm -rf {}", d(&m.join("src")))),
        (fx.wt(), format!("cd {} && rm -rf src", d(&m))),
    ];
    assert_none_allow(&fx, &cases, "these delete tracked files of the main tree");
}

/// Restores `.git/worktrees` permissions so the tempdir can be removed even
/// when an assertion panics.
struct Unreadable(PathBuf);
impl Drop for Unreadable {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn unreadable_worktree_registry_is_ask() {
    let fx = Fx::new();
    let reg = fx.main().join(".git/worktrees");
    std::fs::set_permissions(&reg, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _guard = Unreadable(reg.clone());
    assert!(
        std::fs::read_dir(&reg).is_err(),
        "precondition: {} must be unreadable (running as root?)",
        reg.display()
    );
    let cases = [
        (fx.main(), "rm -rf src".to_string(), "ask"),
        (fx.main(), "rm -r src/".to_string(), "ask"),
        (
            fx.src(),
            format!("mv .harness-worktrees/* {}", d(&fx.sp().join("m"))),
            "ask",
        ),
    ];
    assert_verdicts(&fx, &cases, "unreadable .git/worktrees");
}

#[test]
fn red_separate_git_dir_main_tree_rm_is_not_allow() {
    let fx = Fx::new();
    let cases = [(fx.sep(), "rm -rf src".to_string())];
    assert_none_allow(&fx, &cases, "this deletes tracked files of a main tree");
}

#[test]
fn red_git_rm_recursive_in_main_tree_is_not_allow() {
    let fx = Fx::new();
    let cases = [
        (fx.main(), "git rm -r src".to_string()),
        (fx.main(), "git rm -rq src".to_string()),
    ];
    assert_none_allow(&fx, &cases, "these delete tracked files of the main tree");
}
