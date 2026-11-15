#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Relink shapes judged from INSIDE a linked worktree cwd (written by a
//! disinterested author, CLAUDE.md section 2(a); `src/` untouched).
//!
//! Each line first relinks a path (symlink / hard link / mv of a symlink /
//! relink through `sh -c`) and then writes, copies, tees or deletes through it.
//! Expectations:
//!   - system-harm target (`/etc/...`) reached through the relink -> exactly `deny`;
//!   - relink the hook cannot see through (`sh -c`, `mv` of a fresh link) -> not `allow`;
//!   - harmless in-tree controls -> `allow`.
//!
//! Fixture (same shape as `scope_redirect_relaxation_boundaries_20261108.rs`):
//! fake HOME, a repo, a session worktree (CLAUDE_PROJECT_DIR), and a linked
//! worktree under `$HOME/.condukt/worktrees` used as cwd. `F` is the tracked
//! file `crates/condukt/src/maintree.rs`. `d` / `src/d` are existing in-tree
//! symlinks to in-tree directories, so `ln -sfn /etc d` really replaces them
//! (with a real directory `ln -sfn` would create `d/etc` instead).

#[path = "support/neutral_base.rs"]
mod neutral_base;

#[path = "support/bounded.rs"]
mod bounded;

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const F: &str = "crates/condukt/src/maintree.rs";

struct Fx {
    base: PathBuf,
    home: PathBuf,
    sess: PathBuf,
    wt: PathBuf,
    session: String,
}

fn git(home: &Path, dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", home);
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo(home: &Path, repo: &Path) {
    std::fs::create_dir_all(repo.join("crates/condukt/src")).unwrap();
    std::fs::create_dir_all(repo.join("src/x")).unwrap();
    std::fs::create_dir_all(repo.join("src/y0")).unwrap();
    git(home, repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(F), "fn a() {}\n").unwrap();
    std::fs::write(repo.join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(repo.join("c.rs"), "fn c() {}\n").unwrap();
    std::fs::write(repo.join("src/a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(repo.join("src/x/m.rs"), "fn m() {}\n").unwrap();
    std::fs::write(repo.join("src/y0/n.rs"), "fn n() {}\n").unwrap();
    // `d` and `src/d`: in-tree symlinks to in-tree directories.
    symlink("src/y0", repo.join("d")).unwrap();
    symlink("y0", repo.join("src/d")).unwrap();
    git(home, repo, &["add", "."]);
    git(home, repo, &["commit", "-q", "-m", "init"]);
}

impl Fx {
    fn new(name: &str) -> Fx {
        let uniq = format!("{}-{name}", std::process::id());
        let base = neutral_base::neutral_base()
            .join("relink_shapes")
            .join(&uniq);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let home = base.join("home");
        let repo = base.join("src/harness");
        std::fs::create_dir_all(home.join(".condukt/worktrees")).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        init_repo(&home, &repo);
        let wts = base.join("src/.harness-worktrees");
        std::fs::create_dir_all(&wts).unwrap();
        let sess = wts.join("session-bf349dd5");
        git(
            &home,
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "session-bf349dd5",
                sess.to_str().unwrap(),
            ],
        );
        let wt = home.join(".condukt/worktrees/mab3-3da84f08");
        git(
            &home,
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "mab3/3da84f08",
                wt.to_str().unwrap(),
            ],
        );
        Fx {
            base: std::fs::canonicalize(&base).unwrap(),
            home,
            sess,
            wt,
            session: "bf349dd5-8e0c-4418-af43-0de710b93593".to_string(),
        }
    }

    /// (decision, raw stdout), judged as a Bash call from the linked worktree.
    fn bash(&self, command: &str) -> (String, String) {
        let cwd = &self.wt;
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": self.session,
            "cwd": cwd.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.sess)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(cwd);
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0 ({command:?}); stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let d = if stdout.trim().is_empty() {
            "allow".to_string()
        } else {
            let doc: serde_json::Value = serde_json::from_str(stdout.trim())
                .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
            doc["hookSpecificOutput"]["permissionDecision"]
                .as_str()
                .unwrap_or_else(|| panic!("no permissionDecision: {stdout}"))
                .to_string()
        };
        println!("[{command}] => {d}: {}", stdout.trim());
        (d, stdout)
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[track_caller]
fn expect_deny(name: &str, command: &str) {
    let fx = Fx::new(name);
    let r = fx.bash(command);
    assert_eq!(
        r.0,
        "deny",
        "{name}: system-harm target through a relink must be exactly deny; got {}: {:?}",
        r.0,
        r.1.trim()
    );
}

#[track_caller]
fn expect_not_allow(name: &str, command: &str) {
    let fx = Fx::new(name);
    let r = fx.bash(command);
    assert_ne!(
        r.0,
        "allow",
        "{name}: undeterminable relink must not be allow; got: {:?}",
        r.1.trim()
    );
}

#[track_caller]
fn expect_allow(name: &str, command: &str) {
    let fx = Fx::new(name);
    let r = fx.bash(command);
    assert_eq!(
        r.0,
        "allow",
        "{name}: harmless in-tree control must be allow; got {}: {:?}",
        r.0,
        r.1.trim()
    );
}

// ---------------------------------------------------------------------------
// System-harm through a visible relink: exactly deny
// ---------------------------------------------------------------------------

#[test]
fn r1_ln_sf_file_to_etc_hosts_then_redirect() {
    expect_deny("r1", &format!("ln -sf /etc/hosts {F} && echo x > {F}"));
}

#[test]
fn r2_ln_sfn_dir_to_etc_then_append_through_it() {
    expect_deny("r2", "ln -sfn /etc d && echo x >> d/hosts");
}

#[test]
fn r3_hard_link_etc_hosts_then_redirect() {
    expect_deny("r3", "ln /etc/hosts h && echo x > h");
}

/// Known-open hole: a `cp` destination through a relinked directory.
#[test]
fn r4_ln_sfn_dir_to_etc_then_cp_destination_through_it() {
    expect_deny("r4", "ln -sfn /etc src/d && cp src/a.rs src/d/hosts");
}

#[test]
fn r5_ln_sfn_dir_to_etc_then_rm_rf_through_it() {
    expect_deny("r5", "ln -sfn /etc src/d && rm -rf src/d/hosts");
}

#[test]
fn r8_ln_sfn_dir_to_etc_then_tee_through_it() {
    expect_deny("r8", "ln -sfn /etc d && tee d/hosts < /dev/null");
}

// ---------------------------------------------------------------------------
// Relink the hook cannot follow: not allow
// ---------------------------------------------------------------------------

#[test]
fn r6_relink_inside_sh_c_then_redirect() {
    expect_not_allow(
        "r6",
        &format!("sh -c 'ln -sf /etc/hosts {F}' && echo x > {F}"),
    );
}

#[test]
fn r7_fresh_link_moved_onto_target_then_redirect() {
    expect_not_allow(
        "r7",
        &format!("ln -s /etc/hosts tmp && mv tmp {F} && echo x > {F}"),
    );
}

// ---------------------------------------------------------------------------
// Controls: harmless in-tree lines stay allow
// ---------------------------------------------------------------------------

#[test]
fn c1_in_tree_relink_then_redirect_allows() {
    expect_allow("c1", "ln -sf a.rs c.rs && echo y > c.rs");
}

#[test]
fn c2_in_tree_recursive_copy_then_redirect_allows() {
    expect_allow("c2", "cp -R src/x src/y && ls > out.txt");
}

#[test]
fn c3_build_then_restore_tracked_file_allows() {
    expect_allow("c3", &format!("cargo build && git show HEAD:{F} > {F}"));
}
