#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro, 2026-11-28: a live registered `/flow` driver session Denies the
//! repo's own committed scripts.
//!
//! Observed in a driver session (verbatim reasons):
//!
//! * `cd /Users/yuki/src/harness && (scripts/rollout-plugins.sh --canary > <log> 2>&1; ...)`
//!   — "direct execution of the program file `scripts/rollout-plugins.sh`,
//!   whose text is not in the command — blastguard cannot judge what it would
//!   do (Table B.5: Undetermined)"
//! * `cd <worktree> && python3 -m pytest -q scripts/test_x.py 2>&1 | tail -40`
//!   — "`python3`: python -m runs a module — blastguard cannot judge what it
//!   would do (Table B.5: Undetermined)"
//!
//! User ruling 2026-11-28 (verbatim): 「driverで repo スクリプトを許可 (推奨)」,
//! option text: 「この repo の scripts/ にあり、commit 済みで追跡されているファイル
//! （rollout、test_*.py、tests/*.sh）を、plugin cache の bin と同じく driver
//! セッションでも Allow にする。RED テストを書いて blastguard を修正する。未 commit
//! の変更があるスクリプトは Deny のまま。」
//!
//! Expected shape: the same mechanism as the plugin-cache trust (backlog
//! a50833d7, `SafeRoots::with_driver_programs` / `driver_trusts_program`,
//! `b5.rs` `driver_trusted`): in a driver session, a regular file under
//! `<this repo>/scripts/` that git observes as tracked and clean gets no B.5
//! objection, for direct runs, `bash <f>`, `python3 <f>` and
//! `python3 -m pytest ... <scripts/test_*.py>`.
//!
//! "This repo" is read here as BOTH the session's own linked worktree and the
//! main working tree of the same repository (the observed rollout line `cd`s
//! to the main tree from a session). Which of the two the fix must cover is
//! pinned by separate tests so the answer is visible per case.
//!
//! Controls (must stay as they are): an uncommitted-modified script, an
//! untracked script, a script outside the repo (session scratchpad-like dir
//! that is NOT the session scratchpad) are not Allow in a driver session; a
//! tracked script outside `scripts/` (`tools/y.sh`) — out of the ruling's
//! scope, pinned as "not Allow via the new rule" ONLY for the `-m pytest`
//! shape (direct runs of tracked-clean files in the own worktree are already
//! Allow by ruling 2026-11-18 T1, independent of driver mode, so that
//! expectation is pinned as Allow, observed on the pre-fix tree); a tracked
//! script whose text contains `rm -rf /etc` follows the existing B.5 behaviour
//! (the file's text is not read; see the test doc); a non-driver session still
//! Asks for the `-m pytest` shape.
//!
//! Driver registration and fixture layout copied from
//! `backlog_20261121_driver_plugin_bin_flow_watch.rs`.
//!
//! DOES NOT PROVE: `tests/*.sh` outside `scripts/` (the ruling text names it,
//! but its scope sentence says `scripts/`; see the report), pytest's own
//! conftest.py / plugin loading, a stale driver, subagent mode, exact reason
//! text.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SESSION: &str = "driver-repo-scripts-allow-20261128";

struct Fx {
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn repo(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    fn wt(&self) -> PathBuf {
        self.base
            .join("src")
            .join(".harness-worktrees")
            .join("wt-own")
    }
    fn git(&self, dir: &Path, args: &[&str]) {
        let mut cmd = git_env::pinned(dir);
        cmd.args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.home());
        let out =
            bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
        assert!(
            out.status.success(),
            "fixture git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn register_driver(&self, session: &str) {
        let project = self.repo().canonicalize().unwrap();
        let project = project.to_string_lossy().into_owned();
        let slug = format!("{:016x}", harness_core::hash::fnv1a64(project.as_bytes()));
        let file = format!(
            "{:016x}.driver",
            harness_core::hash::fnv1a64(session.as_bytes())
        );
        let dir = self.home().join(".backlog").join("drivers").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let info = serde_json::json!({
            "session_id": session,
            "pid": std::process::id(),
            "project": project,
            "registered_at": now,
            "heartbeat_at": now,
        });
        std::fs::write(dir.join(file), serde_json::to_string_pretty(&info).unwrap()).unwrap();
    }
}

fn write_exe(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// Repo with committed `scripts/x.sh`, `scripts/x.py`, `scripts/test_x.py`,
/// `scripts/harm.sh` (text contains `rm -rf /etc`), `tools/y.sh`, then a
/// linked worktree `wt-own` on branch `own` (so the same files are tracked
/// and clean there too). Driver registered iff `driver`.
fn fixture(tag: &str, driver: bool) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("driver-reposcripts-{tag}-{n}"));
    let _ = std::fs::remove_dir_all(&base);
    for d in [
        base.join("home"),
        base.join("src").join("proj"),
        base.join("src").join(".harness-worktrees"),
        base.join("store"),
        base.join("tmpdir"),
        base.join("outside"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fx = Fx { base };
    let repo = fx.repo();
    std::fs::write(repo.join("README"), "x\n").unwrap();
    write_exe(&repo.join("scripts/x.sh"), "#!/bin/sh\necho ok\n");
    write_exe(
        &repo.join("scripts/x.py"),
        "#!/usr/bin/env python3\nprint('ok')\n",
    );
    std::fs::write(
        repo.join("scripts/test_x.py"),
        "def test_x():\n    assert True\n",
    )
    .unwrap();
    write_exe(&repo.join("scripts/harm.sh"), "#!/bin/sh\nrm -rf /etc\n");
    write_exe(&repo.join("tools/y.sh"), "#!/bin/sh\necho ok\n");
    std::fs::write(
        repo.join("tools/test_y.py"),
        "def test_y():\n    assert True\n",
    )
    .unwrap();
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = fx.wt();
    fx.git(
        &repo,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "own"],
    );
    if driver {
        fx.register_driver(SESSION);
    }
    fx
}

/// Run the hook with payload cwd `cwd`; returns (decision, reason). An empty
/// stdout is "allow" (no objection).
fn run_hook(fx: &Fx, cwd: &Path, command: &str) -> (String, String) {
    let p = serde_json::json!({
        "session_id": SESSION,
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", fx.home())
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", fx.base.join("tmpdir"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    git_env::scrub(&mut cmd);
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(
        out.status.success(),
        "hook exited {:?} for {command:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout for {command:?} is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

fn expect(fx: &Fx, cwd: &Path, cmd: &str, want: &str) {
    let (d, r) = run_hook(fx, cwd, cmd);
    assert_eq!(d, want, "{cmd:?} (cwd {}) => {d} ({r})", cwd.display());
}

fn expect_not_allow(fx: &Fx, cwd: &Path, cmd: &str) {
    let (d, r) = run_hook(fx, cwd, cmd);
    assert_ne!(
        d,
        "allow",
        "{cmd:?} (cwd {}) must not be allow ({r})",
        cwd.display()
    );
}

// ---------------------------------------------------------------- RED ----

/// The observed pytest Deny, in the session's own worktree.
#[test]
#[ignore = "RED pending"]
fn red_driver_own_worktree_python_m_pytest_tracked_scripts_test_is_allow() {
    let fx = fixture("pytest-wt", true);
    let wt = fx.wt();
    let cmd = format!(
        "cd {} && python3 -m pytest -q scripts/test_x.py 2>&1 | tail -40",
        wt.display()
    );
    expect(&fx, &wt, &cmd, "allow");
    expect(&fx, &wt, "python3 -m pytest -q scripts/test_x.py", "allow");
}

/// The observed rollout shape: session cwd is its own worktree, the line
/// `cd`s to the MAIN working tree and runs a tracked clean script there in a
/// subshell with redirects.
#[test]
#[ignore = "RED pending"]
fn red_driver_cd_main_tree_subshell_script_is_allow() {
    let fx = fixture("rollout-shape", true);
    let wt = fx.wt();
    let log = fx.base.join("tmpdir").join("rollout.log");
    let cmd = format!(
        "cd {} && (scripts/x.sh --canary > {} 2>&1; echo $?)",
        fx.repo().display(),
        log.display()
    );
    expect(&fx, &wt, &cmd, "allow");
}

/// The four shapes the ruling names, run against the MAIN working tree's
/// `scripts/` from a session whose cwd is its own worktree.
#[test]
#[ignore = "RED pending"]
fn red_driver_main_tree_scripts_four_shapes_are_allow() {
    let fx = fixture("main-four", true);
    let wt = fx.wt();
    let r = fx.repo();
    let r = r.display();
    for cmd in [
        format!("{r}/scripts/x.sh"),
        format!("bash {r}/scripts/x.sh"),
        format!("python3 {r}/scripts/x.py"),
        format!("cd {r} && python3 -m pytest -q scripts/test_x.py"),
    ] {
        expect(&fx, &wt, &cmd, "allow");
    }
}

// ----------------------------------------- already Allow on pre-fix tree ----

/// The three non-`-m` shapes in the session's OWN worktree. Pinned as Allow;
/// these pass on the pre-fix tree via ruling 2026-11-18 T1 (`b5.rs`
/// `tracked_unmodified`), not via driver trust. Kept un-ignored as a guard.
#[test]
fn driver_own_worktree_scripts_direct_bash_python_are_allow() {
    let fx = fixture("wt-three", true);
    let wt = fx.wt();
    for cmd in ["scripts/x.sh", "bash scripts/x.sh", "python3 scripts/x.py"] {
        expect(&fx, &wt, cmd, "allow");
    }
}

// ----------------------------------------------------------- controls ----

#[test]
fn control_driver_dirty_script_not_allow() {
    let fx = fixture("dirty", true);
    let wt = fx.wt();
    write_exe(&wt.join("scripts/x.sh"), "#!/bin/sh\necho changed\n");
    std::fs::write(
        wt.join("scripts/test_x.py"),
        "def test_x():\n    assert 1\n",
    )
    .unwrap();
    for cmd in [
        "scripts/x.sh",
        "bash scripts/x.sh",
        "python3 -m pytest -q scripts/test_x.py",
    ] {
        expect(&fx, &wt, cmd, "deny");
    }
}

#[test]
fn control_driver_staged_but_uncommitted_script_not_allow() {
    let fx = fixture("staged", true);
    let wt = fx.wt();
    write_exe(&wt.join("scripts/x.sh"), "#!/bin/sh\necho staged\n");
    fx.git(&wt, &["add", "scripts/x.sh"]);
    expect(&fx, &wt, "scripts/x.sh", "deny");
}

#[test]
fn control_driver_untracked_script_not_allow() {
    let fx = fixture("untracked", true);
    let wt = fx.wt();
    write_exe(&wt.join("scripts/new.sh"), "#!/bin/sh\necho new\n");
    std::fs::write(wt.join("scripts/test_new.py"), "def test_n():\n    pass\n").unwrap();
    for cmd in [
        "scripts/new.sh",
        "bash scripts/new.sh",
        "python3 -m pytest -q scripts/test_new.py",
    ] {
        expect(&fx, &wt, cmd, "deny");
    }
}

#[test]
fn control_driver_script_outside_repo_not_allow() {
    // A dir outside any repo and NOT the session scratchpad layout.
    let fx = fixture("outside", true);
    let wt = fx.wt();
    let out = fx.base.join("outside");
    write_exe(&out.join("scripts/x.sh"), "#!/bin/sh\necho ok\n");
    std::fs::write(out.join("scripts/test_x.py"), "def test_x():\n    pass\n").unwrap();
    let o = out.display();
    for cmd in [
        format!("{o}/scripts/x.sh"),
        format!("bash {o}/scripts/x.sh"),
        format!("python3 -m pytest -q {o}/scripts/test_x.py"),
    ] {
        expect(&fx, &wt, &cmd, "deny");
    }
}

/// Outside `scripts/` is out of the ruling's scope. Pinned here:
/// `-m pytest` on a tracked clean `tools/test_y.py` is NOT allowed by the new
/// rule (stays Deny in a driver session).
#[test]
fn control_driver_tracked_outside_scripts_dir_pytest_not_allow() {
    let fx = fixture("tools", true);
    let wt = fx.wt();
    expect(&fx, &wt, "python3 -m pytest -q tools/test_y.py", "deny");
    // Main tree `tools/y.sh` from the own worktree: not this session's T1
    // tree, not under scripts/ → not Allow.
    let cmd = format!("{}/tools/y.sh", fx.repo().display());
    expect_not_allow(&fx, &wt, &cmd);
}

/// The script's text is NOT in the command; B.5 never reads the file, so a
/// tracked clean script containing `rm -rf /etc` is judged exactly like any
/// other tracked clean script (existing T1 behaviour: own-worktree direct run
/// is Allow on the pre-fix tree). This pins the existing behaviour; whether
/// file contents should be scanned is a separate question for the user.
#[test]
fn control_driver_tracked_harmful_text_script_follows_existing_b5() {
    let fx = fixture("harm", true);
    let wt = fx.wt();
    expect(&fx, &wt, "scripts/harm.sh", "allow");
    // A harmful command ON THE LINE stays Deny regardless of the trust.
    expect(&fx, &wt, "scripts/x.sh && rm -rf /etc", "deny");
}

/// Non-driver session: the `-m pytest` shape and an untracked script stay Ask.
#[test]
fn control_non_driver_session_still_asks() {
    let fx = fixture("nodrv", false);
    let wt = fx.wt();
    expect(&fx, &wt, "python3 -m pytest -q scripts/test_x.py", "ask");
    write_exe(&wt.join("scripts/new.sh"), "#!/bin/sh\necho new\n");
    expect(&fx, &wt, "scripts/new.sh", "ask");
    let cmd = format!(
        "cd {} && python3 -m pytest -q scripts/test_x.py",
        fx.repo().display()
    );
    expect(&fx, &wt, &cmd, "ask");
}
