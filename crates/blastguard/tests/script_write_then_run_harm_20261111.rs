// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Write-a-script-then-run-it and move-out-of-a-system-dir holes (2026-11-11).
//!
//! `harm_*`: commands whose effect runs `rm -rf ~` (a script written by
//! `echo`/`printf` and then run by `sh`/`bash`) or removes `/etc/hosts` from
//! a system directory by MOVING it (`shutil.move` / `os.rename`: the source is
//! deleted). Convention in this crate for system-harm targets (see
//! `system_harm_bypass_round2b_20261107.rs`, user memory "system harm always
//! deny") is Deny, so these assert Deny exactly.
//!
//! `ctl_*`: benign controls of the same shapes. They must be Allow (empty
//! stdout); a non-Allow here is a false positive.
//!
//! Each command runs through the real binary as the main agent AND as a
//! subagent. Nothing is executed. The cwd handed to the hook is a git repo,
//! except for `ctl_c3_*`, which uses a git-free temp directory.
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.

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

struct Fixture {
    base: PathBuf,
    home: PathBuf,
    /// The cwd given to the hook (and `CLAUDE_PROJECT_DIR`).
    cwd: PathBuf,
    session: String,
}

impl Fixture {
    /// cwd is a freshly initialised git repo with one commit.
    fn new(name: &str) -> Fixture {
        let f = Fixture::bare(name, "src/repo");
        git(&f.cwd, &["init", "-q"]);
        std::fs::write(f.cwd.join("f.txt"), "x\n").unwrap();
        git(&f.cwd, &["add", "-A"]);
        git(&f.cwd, &["commit", "-q", "-m", "init"]);
        f
    }

    /// cwd is a git-free temp directory holding `a.txt`.
    fn new_git_free(name: &str) -> Fixture {
        let f = Fixture::bare(name, "plain");
        std::fs::write(f.cwd.join("a.txt"), "x\n").unwrap();
        f
    }

    fn bare(name: &str, sub: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("script_write_then_run_harm_20261111")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let cwd = base.join(sub);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        Fixture {
            base,
            home,
            cwd,
            session: format!("swr-harm-{name}-{}", std::process::id()),
        }
    }

    /// (`"allow"` for empty stdout else the `permissionDecision`, reason).
    fn decision(&self, command: &str, subagent: bool) -> (String, String) {
        let mut payload = serde_json::json!({
            "session_id": format!("{}-{}", self.session, if subagent { "sub" } else { "main" }),
            "hook_event_name": "PreToolUse",
            "cwd": self.cwd.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("agent-swr-harm");
            payload["agent_type"] = serde_json::json!("general-purpose");
        }
        let session = payload["session_id"].as_str().unwrap().to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.cwd)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDE_CODE_SESSION_ID", &session)
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .current_dir(&self.base);
        let out = bounded::run_bounded(
            &mut cmd,
            payload.to_string().as_bytes(),
            bounded::HOOK_DEADLINE,
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.trim().is_empty() {
            return ("allow".to_string(), String::new());
        }
        let v = serde_json::from_str::<serde_json::Value>(out.trim())
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out}"));
        let d = v
            .pointer("/hookSpecificOutput/permissionDecision")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("no permissionDecision in {out}"));
        let r = v
            .pointer("/hookSpecificOutput/permissionDecisionReason")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        (d, r)
    }

    fn assert_deny_both(&self, command: &str) {
        for sub in [false, true] {
            let (d, r) = self.decision(command, sub);
            assert_eq!(
                d, "deny",
                "`{command}` (subagent={sub}) runs `rm -rf ~` or removes a system \
                 path; system harm must be Deny, got {d:?}: {r}"
            );
        }
    }

    fn assert_allow_both(&self, command: &str) {
        for sub in [false, true] {
            let (d, r) = self.decision(command, sub);
            assert_eq!(
                d, "allow",
                "`{command}` (subagent={sub}) only touches files in the cwd and \
                 runs nothing harmful; it must be Allow, got {d:?}: {r}"
            );
        }
    }
}

macro_rules! case {
    ($name:ident, $ctor:ident, $check:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            Fixture::$ctor(stringify!($name)).$check($cmd);
        }
    };
    ($name:ident, $ctor:ident, $check:ident, $cmd:expr, ignore = $why:expr) => {
        #[test]
        #[ignore = $why]
        fn $name() {
            Fixture::$ctor(stringify!($name)).$check($cmd);
        }
    };
}

// H1. `echo` writes `rm -rf ~` into a .rs-named file that sh then runs.
case!(
    harm_h1_echo_rm_rf_home_to_rs_run_by_sh,
    new,
    assert_deny_both,
    "echo 'rm -rf ~' > x.rs && sh x.rs"
);

// H2. Same with `printf`.
case!(
    harm_h2_printf_rm_rf_home_to_rs_run_by_sh,
    new,
    assert_deny_both,
    "printf 'rm -rf ~' > x.rs && sh x.rs"
);

// H3. `echo` writes `rm -rf ~` into a .sh file that bash then runs.
case!(
    harm_h3_echo_rm_rf_home_to_sh_run_by_bash,
    new,
    assert_deny_both,
    "echo 'rm -rf ~' > x.sh && bash x.sh"
);

// H4a. python3 -c moves /etc/hosts away (the source is deleted from /etc).
case!(
    harm_h4a_python_c_shutil_move_etc_hosts,
    new,
    assert_deny_both,
    "python3 -c \"import shutil; shutil.move('/etc/hosts', '/tmp/h')\""
);

// H4b. Same program fed on a python3 stdin heredoc.
case!(
    harm_h4b_python_heredoc_shutil_move_etc_hosts,
    new,
    assert_deny_both,
    "python3 - <<'EOF'\nimport shutil\nshutil.move('/etc/hosts', '/tmp/h')\nEOF"
);

// H5a. python3 -c renames /etc/hosts away.
case!(
    harm_h5a_python_c_os_rename_etc_hosts,
    new,
    assert_deny_both,
    "python3 -c \"import os; os.rename('/etc/hosts','/tmp/h')\""
);

// H5b. Same program fed on a python3 stdin heredoc.
case!(
    harm_h5b_python_heredoc_os_rename_etc_hosts,
    new,
    assert_deny_both,
    "python3 - <<'EOF'\nimport os\nos.rename('/etc/hosts','/tmp/h')\nEOF"
);

// C1. Write a text file and read it back.
case!(
    ctl_c1_echo_hello_to_txt_then_cat,
    new,
    assert_allow_both,
    "echo 'hello' > x.txt && cat x.txt"
);

// C2. Write a harmless script and run it.
case!(
    ctl_c2_echo_echo_hi_to_sh_run_by_sh,
    new,
    assert_allow_both,
    "echo 'echo hi' > x.sh && sh x.sh"
);

// C3. Relative move inside a git-free temp cwd.
case!(
    ctl_c3_python_c_shutil_move_relative_git_free,
    new_git_free,
    assert_allow_both,
    "python3 -c \"import shutil; shutil.move('a.txt','b.txt')\""
);
