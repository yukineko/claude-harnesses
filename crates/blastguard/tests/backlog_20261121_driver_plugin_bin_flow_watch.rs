#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro, 2026-11-21: in a registered `/flow` driver session the flow
//! skill's own mandatory command (Step 2.5)
//! `${CLAUDE_PLUGIN_ROOT}/bin/flow-watch --session S --project P` expands to
//! `$HOME/.claude/plugins/cache/yukineko/flow/<ver>/bin/flow-watch`. Outside a
//! driver session blastguard answers Ask ("direct execution of the program
//! file ..., whose text is not in the command ... Table B.5: Undetermined");
//! in a driver session that Ask is hardened to Deny (4ef3a9e2,
//! `driver_trusts_program` trusts only scratchpad and own worktree `target/`),
//! so the skill's own command can never run. Expected: a program under the
//! installed plugin cache `$HOME/.claude/plugins/cache/yukineko/<plugin>/<ver>/bin/`
//! is Allow in a driver session (our own rolled-out code; the same binaries
//! are Allow by bare name via PATH).
//!
//! Control: a binary in an arbitrary dir (`$HOME/random/bin/x`) in the same
//! driver session must NOT be Allow.
//!
//! Driver registration layout copied from
//! `backlog_20261121_flow_driver_session_never_asks.rs`.
//!
//! DOES NOT PROVE: other plugin names, a stale driver, subagent mode, the
//! exact reason text.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SESSION: &str = "driver-plugin-bin-flow-watch-20261121";

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

    fn make_exe(&self, path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("driver-pluginbin-{tag}-{n}"));
    let _ = std::fs::remove_dir_all(&base);
    for d in [
        base.join("home"),
        base.join("src").join("proj"),
        base.join("src").join(".harness-worktrees"),
        base.join("store"),
        base.join("tmpdir"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fx = Fx { base };
    let repo = fx.repo();
    std::fs::write(repo.join("README"), "x\n").unwrap();
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = fx.wt();
    fx.git(
        &repo,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "own"],
    );
    fx
}

fn run_main(fx: &Fx, command: &str) -> (String, String) {
    let cwd = fx.wt();
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
        .env("CLAUDE_PROJECT_DIR", &cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .current_dir(&cwd)
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

#[test]
#[ignore = "RED: driver session denies installed plugin bin flow-watch"]
fn driver_session_allows_installed_plugin_bin_flow_watch() {
    let fx = fixture("main");
    fx.register_driver(SESSION);
    let bin = fx
        .home()
        .join(".claude/plugins/cache/yukineko/flow/0.2.42/bin/flow-watch");
    fx.make_exe(&bin);
    let cmd = format!(
        "{} --session {SESSION} --project {}",
        bin.display(),
        fx.repo().display()
    );
    let (d, r) = run_main(&fx, &cmd);
    assert_eq!(d, "allow", "{cmd:?} => {d} ({r})");
}

#[test]
fn control_driver_session_random_dir_bin_not_allow() {
    let fx = fixture("ctl");
    fx.register_driver(SESSION);
    let bin = fx.home().join("random/bin/x");
    fx.make_exe(&bin);
    let cmd = format!(
        "{} --session {SESSION} --project {}",
        bin.display(),
        fx.repo().display()
    );
    let (d, r) = run_main(&fx, &cmd);
    assert_ne!(d, "allow", "{cmd:?} must not be allow ({r})");
}
