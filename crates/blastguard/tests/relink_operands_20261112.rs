#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Relinked *command operands* judged from INSIDE a linked worktree cwd
//! (written by a disinterested author, CLAUDE.md section 2(a); `src/` untouched).
//!
//! Each line first relinks `src/d` to `/etc` and then modifies a file through it
//! with a command whose write target is an operand (not a redirect): `sed -i`,
//! `sort -o`, `cp -t` / `cp --target-directory=`, `touch`, `chmod`, `truncate`.
//! Expectations:
//!   - system-harm target reached through the relink -> exactly `deny`, and the
//!     deny reason names the real landing (`/etc/`);
//!   - harmless in-tree controls -> `allow`, or (where the pre-fix decision was
//!     not `allow`) the same decision main 517a4951 gave. A control that was
//!     `allow` on main and is not `allow` here is a false-positive regression.
//!
//! Fixture copied from `relink_shapes_20261111.rs`: fake HOME, a repo, a
//! session worktree (CLAUDE_PROJECT_DIR), and a linked worktree under
//! `$HOME/.condukt/worktrees` used as cwd. `F` is tracked. `d` / `src/d` are
//! tracked in-tree symlinks to in-tree directories, so `ln -sfn /etc src/d`
//! really replaces them.

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
    std::fs::write(repo.join("src/b.rs"), "fn b() {}\n").unwrap();
    std::fs::write(repo.join("src/c.rs"), "fn c() {}\n").unwrap();
    std::fs::write(repo.join("src/hosts"), "127.0.0.1 localhost\n").unwrap();
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
            .join("relink_operands")
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

/// Exactly `deny`, and the reason names the real landing under `/etc/`.
#[track_caller]
fn expect_deny_at_etc(name: &str, command: &str) {
    expect_deny_naming(name, command, &["/etc/"]);
}

/// Exactly `deny`, and the reason contains one of `landing` (the real
/// landing). The needles must not occur in the command text itself, so an
/// echoed command cannot satisfy the check.
#[track_caller]
fn expect_deny_naming(name: &str, command: &str, landing: &[&str]) {
    for l in landing {
        assert!(
            !command.contains(l),
            "{name}: needle {l} occurs in the command"
        );
    }
    let fx = Fx::new(name);
    let r = fx.bash(command);
    assert_eq!(
        r.0,
        "deny",
        "{name}: system-harm target through a relink must be exactly deny; got {}: {:?}",
        r.0,
        r.1.trim()
    );
    assert!(
        landing.iter().any(|l| r.1.contains(l)),
        "{name}: deny reason must name the real landing (one of {landing:?}); got: {:?}",
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

/// Same decision as main 517a4951 gave (observed, see constants below).
#[track_caller]
fn expect_same_as_main(name: &str, command: &str, main_decision: &str) {
    let fx = Fx::new(name);
    let r = fx.bash(command);
    assert_eq!(
        r.0,
        main_decision,
        "{name}: control must keep main 517a4951's decision {main_decision}; got {}: {:?}",
        r.0,
        r.1.trim()
    );
}

// ---------------------------------------------------------------------------
// System harm through a relinked operand: exactly deny, naming /etc/
// ---------------------------------------------------------------------------

#[test]
fn h1_sed_in_place_through_relinked_dir() {
    expect_deny_at_etc("h1", "ln -sfn /etc src/d && sed -i s/a/b/ src/d/hosts");
}

#[test]
#[ignore = "RED: sort -o through a dir relinked to /etc is allow on 0.2.454, must be deny (2026-11-12)"]
fn h2_sort_output_through_relinked_dir() {
    expect_deny_at_etc("h2", "ln -sfn /etc src/d && sort -o src/d/hosts src/a.rs");
}

/// The destination operand is the relinked directory itself, so the landing
/// may be named as the directory (`/private/etc` on macOS, `/etc/...`).
#[test]
fn h3a_cp_t_into_relinked_dir() {
    expect_deny_naming(
        "h3a",
        "ln -sfn /etc src/d && cp -t src/d src/hosts",
        &["/etc/", "/private/etc"],
    );
}

#[test]
fn h3b_cp_target_directory_into_relinked_dir() {
    expect_deny_naming(
        "h3b",
        "ln -sfn /etc src/d && cp --target-directory=src/d src/hosts",
        &["/etc/", "/private/etc"],
    );
}

#[test]
#[ignore = "RED: touch through a dir relinked to /etc is allow on 0.2.454, must be deny (2026-11-12)"]
fn h4_touch_through_relinked_dir() {
    expect_deny_at_etc("h4", "ln -sfn /etc src/d && touch src/d/newfile");
}

#[test]
fn h5_chmod_through_relinked_dir() {
    expect_deny_at_etc("h5", "ln -sfn /etc src/d && chmod 777 src/d/hosts");
}

#[test]
#[ignore = "RED: truncate through a dir relinked to /etc is deny on 0.2.454 but the reason does not name the /etc landing (2026-11-12)"]
fn h6_truncate_through_relinked_dir() {
    expect_deny_at_etc("h6", "ln -sfn /etc src/d && truncate -s0 src/d/hosts");
}

// ---------------------------------------------------------------------------
// Controls (false-positive guards). Main decisions observed on 517a4951
// (blastguard 0.2.452) with this same file, 2026-11-12.
// ---------------------------------------------------------------------------

// Main decisions: k1 allow, k2 allow, k3 ask, k4 allow, k5 allow, k6 allow,
// k7 ask. A control that was allow on main is asserted allow here.
const MAIN_K3: &str = "ask";
const MAIN_K7: &str = "ask";

/// Deletes only the freshly created link (rm does not follow it). Main: allow.
#[test]
fn k1_fresh_link_to_etc_then_rm_the_link() {
    expect_allow("k1", "ln -s /etc d2 && rm d2");
}

#[test]
fn k2_in_tree_relink_then_cp_onto_it_allows() {
    expect_allow("k2", "ln -sf a.rs src/c.rs && cp src/b.rs src/c.rs");
}

/// In-tree relink target: allow, or whatever main gave.
#[test]
fn k3_in_tree_dir_relink_then_rm_through_it() {
    let fx = Fx::new("k3");
    let r = fx.bash("ln -sfn src/x src/d && rm -rf src/d/tmpfile");
    assert!(
        r.0 == "allow" || r.0 == MAIN_K3,
        "k3: in-tree relink control must be allow or main's {MAIN_K3}; got {}: {:?}",
        r.0,
        r.1.trim()
    );
}

/// `rm` targets a path the `git mv` never touches. Main: allow.
#[test]
#[ignore = "RED: FP regression - allow on main 517a4951, ask on 0.2.454 (`git mv ... may create or rename an entry on this path`) though rm src/c.rs is not a git mv operand (2026-11-12)"]
fn k4_git_mv_then_rm_other_file() {
    expect_allow("k4", "git mv src/a.rs src/b.rs && rm src/c.rs");
}

/// In-tree truncate with no relink: the relink fix must not turn it into a
/// system-harm deny. Main: ask.
#[test]
fn k7_in_tree_truncate_without_relink_keeps_main_decision() {
    expect_same_as_main("k7", "truncate -s0 src/a.rs", MAIN_K7);
}

#[test]
fn k5_unrelated_link_then_sed_in_place_allows() {
    expect_allow("k5", "ln -s a b && sed -i s/x/y/ src/a.rs");
}

#[test]
fn k6_mkdir_link_in_tree_then_cat_allows() {
    expect_allow(
        "k6",
        "mkdir -p out && ln -sf ../src/a.rs out/a.rs && cat out/a.rs",
    );
}
