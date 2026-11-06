// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repros for operator rules that today live only in LLM memory
//! (memory audit 2026-11-07, class B; user ruling 2026-11-07: "実装で補えるものは
//! 実装にして。LLMの推論での防御はすべてあてにならない").
//!
//! * item 1 — memory `worktree-always-merge-then-delete`: deleting a branch
//!   whose commits are NOT reachable from the default branch, or force-removing
//!   a DIRTY linked worktree, destroys unmerged work and must be Ask.
//! * item 3 — memory `condukt-worker-dirty-tree-commit`: a whole-tree stage
//!   (`git add -A` / `--all` / `.`, `git commit -a`) sweeps in foreign WIP from
//!   other sessions and must be Ask; staging explicit paths stays Allow.
//! * item 16 — memory `scratch-artifacts-in-deletable-places`: creating
//!   scratch trees (`git clone`, `mkdir -p`, `cp -r`) OUTSIDE the managed
//!   places (session scratchpad / temp roots, `.harness-worktrees`, the
//!   project) must be Ask; inside them stays Allow.
//!
//! Each test drives the REAL hook binary against a REAL fixture repo, and
//! pins controls so that a blanket Ask cannot pass it.

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
    /// `<base>/src/.harness-worktrees/dirty` — linked worktree, dirty.
    dirty_wt: PathBuf,
    /// `<base>/src/.harness-worktrees/clean` — linked worktree, clean.
    clean_wt: PathBuf,
}

impl Fixture {
    /// Repo on `main` (2 commits) with branches:
    ///   * `merged`   — points at main~1 (fully merged);
    ///   * `unmerged` — one commit NOT reachable from main;
    ///   * `dirty`, `clean` — checked out in linked worktrees.
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("audit_b_memory_rules_20261107")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        let hw = base.join("src/.harness-worktrees");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&hw).unwrap();

        git(&repo, &["init", "-q"]);
        write(&repo.join("tracked.txt"), "one\n");
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        git(&repo, &["branch", "merged"]);
        write(&repo.join("tracked.txt"), "two\n");
        git(&repo, &["commit", "-q", "-am", "two"]);

        git(&repo, &["switch", "-q", "-c", "unmerged"]);
        write(&repo.join("unique.txt"), "unique work\n");
        git(&repo, &["add", "unique.txt"]);
        git(&repo, &["commit", "-q", "-m", "unique work"]);
        git(&repo, &["switch", "-q", "main"]);

        let dirty_wt = hw.join("dirty");
        let clean_wt = hw.join("clean");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "dirty",
                dirty_wt.to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "clean",
                clean_wt.to_str().unwrap(),
            ],
        );
        write(&dirty_wt.join("tracked.txt"), "uncommitted edit\n");
        write(&dirty_wt.join("new.txt"), "untracked work\n");

        // Primary tree: a dirty tracked file and an untracked file (the
        // "foreign WIP" a whole-tree stage would sweep in).
        write(&repo.join("tracked.txt"), "foreign wip\n");
        write(&repo.join("foreign.txt"), "foreign untracked\n");

        Fixture {
            base,
            home,
            repo,
            dirty_wt,
            clean_wt,
        }
    }

    /// Hook stdout for a Bash `command` (cwd = CLAUDE_PROJECT_DIR = primary).
    fn bash(&self, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": "audit-b-memory-rules",
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

/// Run every `(command, wanted)` case and fail once, listing every mismatch,
/// so the failure names exactly which shapes are wrong.
#[track_caller]
fn check_all(f: &Fixture, cases: &[(String, &str)]) {
    let mut bad = Vec::new();
    for (cmd, want) in cases {
        let out = f.bash(cmd);
        let got = verdict(&out);
        let ok = match *want {
            // "not allow": Ask or Deny both refuse to wave it through.
            "not-allow" => got != "allow",
            w => got == w,
        };
        if !ok {
            bad.push(format!(
                "  `{cmd}`: want {want}, got {got} ({})",
                out.trim()
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} cases have the wrong verdict:\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}

/// Item 1 (memory `worktree-always-merge-then-delete`).
#[test]
fn deleting_unmerged_branch_or_dirty_worktree_is_ask() {
    let f = Fixture::new("branch_delete");
    // Preconditions the verdict must depend on.
    let unique = f.repo.join("unique.txt");
    assert!(
        !unique.exists(),
        "unique.txt only exists on branch unmerged"
    );
    let r = f.repo.display().to_string();
    let dirty = f.dirty_wt.display().to_string();
    let clean = f.clean_wt.display().to_string();
    let cases = vec![
        // Destroys the only ref to an unmerged commit.
        (format!("git -C {r} branch -D unmerged"), "ask"),
        (
            format!("git -C {r} branch --delete --force unmerged"),
            "ask",
        ),
        // Discards uncommitted + untracked work in a linked worktree.
        (format!("git -C {r} worktree remove --force {dirty}"), "ask"),
        // Controls: nothing is lost.
        (format!("git -C {r} branch -d merged"), "allow"),
        (format!("git -C {r} branch -D merged"), "allow"),
        (format!("git -C {r} worktree remove {clean}"), "allow"),
    ];
    check_all(&f, &cases);
}

/// Item 3 (memory `condukt-worker-dirty-tree-commit`).
#[test]
fn whole_tree_stage_or_commit_all_is_ask() {
    let f = Fixture::new("stage_all");
    let r = f.repo.display().to_string();
    let cases = vec![
        (format!("git -C {r} add -A"), "ask"),
        (format!("git -C {r} add --all"), "ask"),
        (format!("git -C {r} add ."), "ask"),
        (format!("git -C {r} commit -a -m wip"), "ask"),
        (format!("git -C {r} commit -am wip"), "ask"),
        (format!("git -C {r} commit --all -m wip"), "ask"),
        // Controls: explicit paths only.
        (format!("git -C {r} add tracked.txt"), "allow"),
        (format!("git -C {r} commit -m msg"), "allow"),
        (format!("git -C {r} commit -m msg -- tracked.txt"), "allow"),
    ];
    check_all(&f, &cases);
}

/// Item 16 (memory `scratch-artifacts-in-deletable-places`).
#[test]
fn scratch_tree_outside_managed_places_is_ask() {
    let f = Fixture::new("scratch_place");
    let r = f.repo.display().to_string();
    let h = f.home.display().to_string();
    let hw = f.base.join("src/.harness-worktrees").display().to_string();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tmp = format!(
        "/private/tmp/blastguard-audit-b-scratch-{}-{nanos}",
        std::process::id()
    );
    let cases = vec![
        // Unmanaged: a bare directory under $HOME.
        (format!("git clone {r} {h}/scratch-clone"), "ask"),
        (format!("mkdir -p {h}/scratch/a/b"), "ask"),
        (format!("cp -r {r} {h}/scratch-copy"), "ask"),
        // Controls: managed places.
        (format!("git clone {r} {tmp}/clone"), "allow"),
        (format!("git clone {r} {hw}/clone"), "allow"),
        (format!("mkdir -p {r}/build/a"), "allow"),
        (format!("mkdir -p {tmp}/a/b"), "allow"),
        (format!("cp -r {r}/tracked.txt {tmp}/copy.txt"), "allow"),
    ];
    check_all(&f, &cases);
}
