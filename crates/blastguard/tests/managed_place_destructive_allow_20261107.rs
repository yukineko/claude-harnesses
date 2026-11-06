// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Destructive operations confined to a MANAGED DISPOSABLE PLACE are Allow
//! (user ruling 2026-11-07).
//!
//! Managed places:
//!   * linked git worktrees under `$HOME/.condukt/worktrees/<wt>`;
//!   * linked git worktrees under `<dirname(project)>/.harness-worktrees/<wt>`;
//!   * the temp roots `/tmp` and `/private/tmp` (the session scratchpad lives
//!     under `/private/tmp`).
//!
//! NOT managed (controls; must stay non-Allow): the primary working tree, the
//! storage root itself, anything under `.git` (the worktree's `.git` file and
//! the main repo's `.git/worktrees/<entry>`), and operands that cannot be
//! placed (an unresolvable shell variable).
//!
//! Shape eligibility (bare single commands only, backlog 873651b9) is kept, so
//! every command here is one plain command.
//!
//! The fixture is a REAL repo `<base>/src/repo` (one commit) with REAL linked
//! worktrees at `<base>/home/.condukt/worktrees/RUN-t2` and
//! `<base>/src/.harness-worktrees/wt`. Each tree (primary included) has a dirty
//! tracked file, an untracked file and an untracked subdir with content.
//!
//! The hook runs with `HOME=<base>/home`, payload cwd = `CLAUDE_PROJECT_DIR` =
//! the primary tree, `TMPDIR` / `BLASTGUARD_ASK` / `CLAUDE_CODE_SESSION_ID`
//! removed, so Allow is exactly empty stdout and Ask is a non-empty ask JSON.
//! Every control pins the exact non-Allow verdict it gets.
//!
//! The base is [`neutral_base::neutral_base`] (see that module) so the fixture
//! location itself cannot decide the verdict.

#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DIRTY_FILE: &str = "tracked.txt";
const UNTRACKED_FILE: &str = "untracked.txt";
const SUBDIR: &str = "subdir";

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

/// Dirty tracked file, untracked file, untracked subdir with content.
fn dirty_up(tree: &Path) {
    write(&tree.join(DIRTY_FILE), "dirty\n");
    write(&tree.join(UNTRACKED_FILE), "untracked\n");
    write(&tree.join(SUBDIR).join("a.txt"), "a\n");
    write(&tree.join(SUBDIR).join("nested/b.txt"), "b\n");
}

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    /// Primary working tree (NOT managed).
    repo: PathBuf,
    /// `$HOME/.condukt/worktrees` — the condukt storage root itself.
    condukt_root: PathBuf,
    /// `$HOME/.condukt/worktrees/RUN-t2` — linked worktree.
    cwt: PathBuf,
    /// `<base>/src/.harness-worktrees/wt` — linked worktree.
    hwt: PathBuf,
}

#[derive(Clone, Copy)]
enum Root {
    Condukt,
    Harness,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("managed_place_destructive_allow_20261107")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        let condukt_root = home.join(".condukt/worktrees");
        let cwt = condukt_root.join("RUN-t2");
        let hw = base.join("src/.harness-worktrees");
        let hwt = hw.join("wt");

        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        write(&repo.join(DIRTY_FILE), "clean\n");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);

        std::fs::create_dir_all(&condukt_root).unwrap();
        std::fs::create_dir_all(&hw).unwrap();
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "RUN-t2",
                cwt.to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "wt", hwt.to_str().unwrap()],
        );
        for t in [&repo, &cwt, &hwt] {
            dirty_up(t);
        }
        assert!(
            cwt.join(".git").is_file(),
            "linked worktree has a .git file"
        );
        assert!(repo.join(".git/worktrees/wt").is_dir());

        Fixture {
            base,
            home,
            repo,
            condukt_root,
            cwt,
            hwt,
        }
    }

    fn wt(&self, root: Root) -> &Path {
        match root {
            Root::Condukt => &self.cwt,
            Root::Harness => &self.hwt,
        }
    }

    /// Hook stdout for a Bash `command` (cwd = CLAUDE_PROJECT_DIR = primary).
    fn bash(&self, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": "managed-place-destructive-allow",
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

/// `"allow"` (empty stdout), `"deny"`, `"ask"`, or `"other"` (non-empty
/// output that is neither — never treated as Allow).
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
fn assert_allow(case: &str, out: &str) {
    assert_eq!(
        verdict(out),
        "allow",
        "{case}: must be Allow (empty stdout); got {out}"
    );
}

#[track_caller]
fn assert_verdict(case: &str, out: &str, want: &str) {
    assert_eq!(verdict(out), want, "{case}: must be {want}; got {out}");
}

/// A uniquely named dir directly under `/private/tmp`, removed on drop.
struct TmpPlace(PathBuf);

impl TmpPlace {
    fn new(tag: &str) -> TmpPlace {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = PathBuf::from(format!(
            "/private/tmp/blastguard-managed-place-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir(&p).unwrap();
        write(&p.join("inner/a.txt"), "a\n");
        write(&p.join("inner/nested/b.txt"), "b\n");
        TmpPlace(p)
    }
}

impl Drop for TmpPlace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---- target helpers --------------------------------------------------------

fn restore_in(root: Root, name: &str) -> (String, String) {
    let f = Fixture::new(name);
    let wt = f.wt(root);
    let case = format!("git -C {} restore {DIRTY_FILE}", wt.display());
    let out = f.bash(&case);
    (case, out)
}

fn reset_hard_in(root: Root, name: &str) -> (String, String) {
    let f = Fixture::new(name);
    let case = format!("git -C {} reset --hard", f.wt(root).display());
    let out = f.bash(&case);
    (case, out)
}

fn clean_fd_in(root: Root, name: &str) -> (String, String) {
    let f = Fixture::new(name);
    let case = format!("git -C {} clean -fd", f.wt(root).display());
    let out = f.bash(&case);
    (case, out)
}

fn rm_subdir_in(root: Root, name: &str) -> (String, String) {
    let f = Fixture::new(name);
    let case = format!("rm -rf {}", f.wt(root).join(SUBDIR).display());
    let out = f.bash(&case);
    (case, out)
}

fn rm_subdir_wildcard_in(root: Root, name: &str) -> (String, String) {
    let f = Fixture::new(name);
    let case = format!("rm -rf {}/*", f.wt(root).join(SUBDIR).display());
    let out = f.bash(&case);
    (case, out)
}

// ---- TARGETS: must be Allow --------------------------------------------------

#[test]
#[ignore = "RED: current verdict deny (git restore discards working-tree changes)"]
fn target_restore_in_condukt_worktree_allow() {
    let (case, out) = restore_in(Root::Condukt, "t_restore_condukt");
    assert_allow(&case, &out);
}

#[test]
#[ignore = "RED: current verdict deny (git restore discards working-tree changes)"]
fn target_restore_in_harness_worktree_allow() {
    let (case, out) = restore_in(Root::Harness, "t_restore_hw");
    assert_allow(&case, &out);
}

#[test]
#[ignore = "RED: current verdict deny (git reset --hard discards working-tree changes)"]
fn target_reset_hard_in_condukt_worktree_allow() {
    let (case, out) = reset_hard_in(Root::Condukt, "t_reset_condukt");
    assert_allow(&case, &out);
}

#[test]
#[ignore = "RED: current verdict deny (git reset --hard discards working-tree changes)"]
fn target_reset_hard_in_harness_worktree_allow() {
    let (case, out) = reset_hard_in(Root::Harness, "t_reset_hw");
    assert_allow(&case, &out);
}

#[test]
#[ignore = "RED: current verdict ask (git clean -f with -d/-x is confined to a safe root)"]
fn target_clean_fd_in_condukt_worktree_allow() {
    let (case, out) = clean_fd_in(Root::Condukt, "t_clean_condukt");
    assert_allow(&case, &out);
}

#[test]
#[ignore = "RED: current verdict ask (git clean -f with -d/-x is confined to a safe root)"]
fn target_clean_fd_in_harness_worktree_allow() {
    let (case, out) = clean_fd_in(Root::Harness, "t_clean_hw");
    assert_allow(&case, &out);
}

#[test]
fn target_rm_rf_subdir_in_condukt_worktree_allow() {
    let (case, out) = rm_subdir_in(Root::Condukt, "t_rm_condukt");
    assert_allow(&case, &out);
}

#[test]
fn target_rm_rf_subdir_in_harness_worktree_allow() {
    let (case, out) = rm_subdir_in(Root::Harness, "t_rm_hw");
    assert_allow(&case, &out);
}

#[test]
#[ignore = "RED: current verdict deny (recursive rm (-r) can delete an entire directory tree)"]
fn target_rm_rf_subdir_wildcard_in_condukt_worktree_allow() {
    let (case, out) = rm_subdir_wildcard_in(Root::Condukt, "t_rmglob_condukt");
    assert_allow(&case, &out);
}

#[test]
#[ignore = "RED: current verdict deny (recursive rm (-r) can delete an entire directory tree)"]
fn target_rm_rf_subdir_wildcard_in_harness_worktree_allow() {
    let (case, out) = rm_subdir_wildcard_in(Root::Harness, "t_rmglob_hw");
    assert_allow(&case, &out);
}

#[test]
fn target_rm_rf_dir_inside_private_tmp_place_allow() {
    let f = Fixture::new("t_rm_private_tmp");
    let place = TmpPlace::new("rm");
    let case = format!("rm -rf {}", place.0.join("inner").display());
    let out = f.bash(&case);
    assert_allow(&case, &out);
}

// ---- CONTROLS: must stay non-Allow ------------------------------------------

#[test]
fn control_restore_in_primary_tree_not_allow() {
    let f = Fixture::new("c_restore_primary");
    let case = format!("git -C {} restore {DIRTY_FILE}", f.repo.display());
    let out = f.bash(&case);
    assert_verdict(&case, &out, "deny");
}

#[test]
fn control_reset_hard_in_primary_tree_not_allow() {
    let f = Fixture::new("c_reset_primary");
    let case = format!("git -C {} reset --hard", f.repo.display());
    let out = f.bash(&case);
    assert_verdict(&case, &out, "deny");
}

#[test]
fn control_clean_fd_in_primary_tree_not_allow() {
    let f = Fixture::new("c_clean_primary");
    let case = format!("git -C {} clean -fd", f.repo.display());
    let out = f.bash(&case);
    assert_verdict(&case, &out, "ask");
}

#[test]
fn control_restore_with_unresolvable_var_not_allow() {
    let f = Fixture::new("c_restore_var");
    let case = format!("git -C \"$WT\" restore {DIRTY_FILE}");
    let out = f.bash(&case);
    assert_verdict(&case, &out, "deny");
}

#[test]
fn control_reset_hard_with_unresolvable_var_not_allow() {
    let f = Fixture::new("c_reset_var");
    let case = "git -C \"$WT\" reset --hard";
    let out = f.bash(case);
    assert_verdict(case, &out, "deny");
}

#[test]
fn control_clean_fd_with_unresolvable_var_not_allow() {
    let f = Fixture::new("c_clean_var");
    let case = "git -C \"$WT\" clean -fd";
    let out = f.bash(case);
    assert_verdict(case, &out, "ask");
}

#[test]
fn control_rm_rf_condukt_storage_root_itself_not_allow() {
    let f = Fixture::new("c_rm_root");
    let case = format!("rm -rf {}", f.condukt_root.display());
    let out = f.bash(&case);
    assert_verdict(&case, &out, "deny");
}

#[test]
fn control_rm_rf_worktree_dot_git_file_not_allow() {
    let f = Fixture::new("c_rm_dotgit");
    let case = format!("rm -rf {}", f.cwt.join(".git").display());
    let out = f.bash(&case);
    assert_verdict(&case, &out, "deny");
}

#[test]
fn control_redirect_write_onto_worktree_dot_git_file_not_allow() {
    let f = Fixture::new("c_redirect_dotgit");
    let case = format!("echo x > {}", f.cwt.join(".git").display());
    let out = f.bash(&case);
    assert_verdict(&case, &out, "deny");
}

#[test]
fn control_rm_rf_main_repo_git_worktrees_entry_not_allow() {
    let f = Fixture::new("c_rm_gitwt_entry");
    let case = format!("rm -rf {}", f.repo.join(".git/worktrees/wt").display());
    let out = f.bash(&case);
    assert_verdict(&case, &out, "deny");
}

#[test]
fn control_rm_rf_primary_tree_subdir_not_allow() {
    let f = Fixture::new("c_rm_primary_subdir");
    let case = format!("rm -rf {}", f.repo.join(SUBDIR).display());
    let out = f.bash(&case);
    assert_verdict(&case, &out, "deny");
}
