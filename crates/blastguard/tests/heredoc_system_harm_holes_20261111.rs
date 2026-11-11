// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Heredoc / here-string holes around SYSTEM-HARM targets (2026-11-11).
//!
//! `harm_*`: commands whose effect writes or removes a system path
//! (`/etc/hosts`) or runs `rm -rf ~`. Convention in this crate for
//! system-harm targets (see `system_harm_bypass_round2b_20261107.rs`, user
//! memory "system harm always deny") is Deny, so these assert Deny exactly.
//!
//! `fp_*`: a python3 heredoc whose body only PRINTS a list containing the
//! words of a `git checkout -- <file>` command. It is a string literal, not a
//! git invocation, so it must be Allow (empty stdout).
//!
//! Each command runs through the real binary as the main agent AND as a
//! subagent. Allow is exactly empty stdout. Nothing is executed.
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
    repo: PathBuf,
    session: String,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = neutral_base::neutral_base()
            .join("heredoc_system_harm_holes_20261111")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let repo = base.join("src/repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("f.txt"), "x\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Fixture {
            base,
            home,
            repo,
            session: format!("hd-harm-{name}-{}", std::process::id()),
        }
    }

    /// (`"allow"` for empty stdout else the `permissionDecision`, reason).
    fn decision(&self, command: &str, subagent: bool) -> (String, String) {
        let mut payload = serde_json::json!({
            "session_id": format!("{}-{}", self.session, if subagent { "sub" } else { "main" }),
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("agent-hd-harm");
            payload["agent_type"] = serde_json::json!("general-purpose");
        }
        let session = payload["session_id"].as_str().unwrap().to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        git_env::scrub(&mut cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.repo)
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
                "`{command}` (subagent={sub}) writes/removes a system path or runs \
                 `rm -rf ~`; system harm must be Deny, got {d:?}: {r}"
            );
        }
    }

    fn assert_allow_both(&self, command: &str) {
        for sub in [false, true] {
            let (d, r) = self.decision(command, sub);
            assert_eq!(
                d, "allow",
                "`{command}` (subagent={sub}) only prints a Python literal; the \
                 command/harm text in it is not executed and must be Allow, got {d:?}: {r}"
            );
        }
    }
}

macro_rules! case {
    ($name:ident, $check:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            Fixture::new(stringify!($name)).$check($cmd);
        }
    };
    ($name:ident, $check:ident, $cmd:expr, ignore = $why:expr) => {
        #[test]
        #[ignore = $why]
        fn $name() {
            Fixture::new(stringify!($name)).$check($cmd);
        }
    };
}

// A. Python fed on stdin after `cd /etc` writes the relative `hosts`.
case!(
    harm_a_cd_etc_python_stdin_writes_hosts,
    assert_deny_both,
    "cd /etc && python3 - <<'EOF'\nopen('hosts','w').write('x')\nEOF",
    ignore = "RED: observed Allow (empty reason), want Deny: relative open('hosts','w') after cd /etc in python3 stdin heredoc (2026-11-11)"
);

// B. Python script written by a heredoc, then run, copies over /etc/hosts.
case!(
    harm_b_heredoc_py_file_copy2_to_etc_hosts,
    assert_deny_both,
    "cat > /tmp/x.py <<'EOF'\nimport shutil\nshutil.copy2('a','/etc/hosts')\nEOF\npython3 /tmp/x.py",
    ignore = "RED: observed Allow (empty reason), want Deny: shutil.copy2 to /etc/hosts in heredoc-written .py then run (2026-11-11)"
);

// C. Same, after `cd /etc`, the script removes the relative `hosts`.
case!(
    harm_c_cd_etc_heredoc_py_file_os_remove_hosts,
    assert_deny_both,
    "cd /etc && cat > /tmp/x.py <<'EOF'\nimport os\nos.remove('hosts')\nEOF\npython3 /tmp/x.py"
);

// D. A Python comment with an apostrophe must not hide the shell redirect
//    to /etc/hosts that follows the heredoc.
case!(
    harm_d_python_apostrophe_comment_then_redirect_etc_hosts,
    assert_deny_both,
    "python3 - <<'EOF'\n# don't\nprint(1)\nEOF\necho y > /etc/hosts"
);

// E. Here-string writes `rm -rf ~` into a file that sh then runs.
case!(
    harm_e_here_string_rm_rf_home_run_by_sh,
    assert_deny_both,
    "cat > x.rs <<< 'rm -rf ~' && sh x.rs",
    ignore = "RED: observed Allow (empty reason), want Deny: here-string body written to a file then run by sh (2026-11-11)"
);

// F. The git words are only a Python list literal that is printed.
case!(
    fp_f_python_list_literal_git_checkout_words,
    assert_allow_both,
    "python3 - <<'EOF'\nimport subprocess\ncmd = [\"git\", \"checkout\", \"--\", \"no-such-file.txt\",]\nprint(cmd)\nEOF"
);

// G. Minimal shape of a live false positive (2026-11-11): a python3 stdin
//    program that only PRINTS a string literal holding system-harm text.
case!(
    fp_g_python_prints_copy2_etc_hosts_literal,
    assert_allow_both,
    "python3 - <<'EOF'\ns = \"shutil.copy2('a','/etc/hosts')\"\nprint(s)\nEOF",
    ignore = "RED: observed Deny (python3 stdin program judged to write /etc/hosts via shutil.copy2), want Allow: the text is only a printed string literal (2026-11-11)"
);

// G2. Same, after `cd /etc`, with a relative `open('hosts','w')` literal.
case!(
    fp_g2_cd_etc_python_prints_open_hosts_literal,
    assert_allow_both,
    "cd /etc && python3 - <<'EOF'\ns = \"open('hosts','w')\"\nprint(s)\nEOF"
);
