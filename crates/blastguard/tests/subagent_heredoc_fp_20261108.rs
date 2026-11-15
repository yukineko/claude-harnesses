// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Suspected false positives: two quoted-heredoc commands that blastguard
//! blocked in a SUBAGENT on 2026-11-08 (live hook, session 32935915).
//!
//! 1. agent a6ce041aca72b06d3, `cd <wt> && python3 - <<'EOF' ... EOF` whose
//!    Python body patches a Rust file; the Rust text `Ok(how) => anyhow!(`
//!    sits inside a Python triple-quoted string:
//!    ```text
//!    '> anyhow!(' is a RELATIVE redirect after an earlier cd/pushd/popd whose
//!    target directory blastguard could not statically resolve ...
//!    ```
//! 2. agent adbe89efd4202dc95, `cd <wt>; cat > scripts/test_x.py <<'EOF'`
//!    whose body starts `#!/usr/bin/env python3` and mentions
//!    `.git/precommit-timing.jsonl` as a fixture path (read-only: the real
//!    body only calls `is_file()` / `read_text()` on it), then a
//!    `python3 -m pytest scripts/test_x.py` in the same command:
//!    ```text
//!    `#!/usr/bin/env python3` takes its program from stdin, and that program
//!    names `.git`, a protected gate/config path ...
//!    ```
//!
//! USER RULING 2026-11-08: a quoted-heredoc body is judged by its target
//! language. A Python program fed to `python3` is judged as Python, and a `>`
//! inside a Python string literal is not a shell redirect. A body written to
//! a `.py` file and not run in the same command is data.
//!
//! Real-body check (done against the transcripts, not assumed): body 1 only
//! reads and rewrites the RELATIVE path `crates/condukt/src/worktree.rs`
//! after `cd` into the worktree itself (inside the worktree). Body 2 only
//! constructs and reads `.git/precommit-timing.jsonl`; it never writes under
//! `.git`. So both are Allow. The `failopen_*` / `control_*` cases keep the
//! restrictive side observable: a Python body that really writes
//! `.git/hooks/pre-commit`, and a real shell redirect to it, must not be
//! Allow.
//!
//! `target_*`: must be Allow (RED today). `control_*`: already restrictive or
//! already Allow, pinning the cause. Each command runs as the main agent AND
//! as a subagent through the real binary (the observed blocks were subagent
//! denials). Allow is exactly empty stdout. `{DIR}` is the fixture's throwaway
//! repo path; `{WT}` is a realistic worktree path under
//! /Users/yuki/src/.harness-worktrees/ (text only; nothing is executed).
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

const WT: &str = "/Users/yuki/src/.harness-worktrees/fix-condukt-merge-timeout";

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
            .join("subagent_heredoc_fp_20261108")
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
            session: format!("hd-sub-{name}-{}", std::process::id()),
        }
    }

    /// (`"allow"` for empty stdout else the `permissionDecision`, reason).
    fn decision(&self, command: &str, subagent: bool) -> (String, String) {
        let command = command
            .replace("{DIR}", &self.repo.display().to_string())
            .replace("{WT}", WT);
        let mut payload = serde_json::json!({
            "session_id": format!("{}-{}", self.session, if subagent { "sub" } else { "main" }),
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("agent-hd-sub");
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

    fn assert_allow_both(&self, command: &str) {
        for sub in [false, true] {
            let (d, r) = self.decision(command, sub);
            assert_eq!(
                d, "allow",
                "`{command}` (subagent={sub}) has its hazard text only inside a QUOTED \
                 heredoc body judged by its target language (a Python string literal, or \
                 data written to a .py file) and must be Allow, got {d:?}: {r}"
            );
        }
    }

    fn assert_not_allow_both(&self, command: &str) {
        for sub in [false, true] {
            let (d, r) = self.decision(command, sub);
            assert_ne!(
                d, "allow",
                "`{command}` (subagent={sub}) really writes a protected path (a shell \
                 redirect, or Python code that is then executed) and must not be Allow \
                 (reason was {r:?})"
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

// ===================================================================
// Case 1: `=> anyhow!(` inside a Python string in `python3 - <<'EOF'`
// ===================================================================

// The observed command (163-line body), delta-debugged (ddmin on the lines of
// the two triple-quoted strings, 8 parallel probes against the real binary as
// a subagent) down to this: cd into the worktree, feed a quoted-heredoc
// Python program to `python3 -`, whose two triple-quoted strings hold the Rust
// text `Err(d) => bail!(` and `Ok(how) => anyhow!(`. Smaller or simpler
// variants (single arrow arm, no apostrophe line, arrow at line start only)
// were measured Allow; this exact combination is the smallest still blocked.
// Real-body check: the Python only reads and rewrites the RELATIVE file
// `crates/condukt/src/worktree.rs` inside the worktree it `cd`ed into.
case!(
    target_cd_python_stdin_rust_arrow_in_string,
    assert_allow_both,
    "cd {WT} && python3 - <<'EOF'\ns=open('a.rs').read()\ns=s.replace('''            ),\n            Err(d) => bail!(''','''/// return the error to report (always an error; see that function's doc).\n        Ok(how) => anyhow!(\n''')\nopen('a.rs','w').write(s)\nEOF"
);

// Bisect: the same body without the leading `cd`, and the same cd with a
// body that has no `>` in a string. If these are Allow, the cause is the
// `>`-in-string x earlier-cd combination.
case!(
    control_python_stdin_rust_arrow_no_cd,
    assert_allow_both,
    "python3 - <<'EOF'\np='{DIR}/f.txt'\ns=open(p).read()\ns=s.replace('''x''','''        Ok(how) => anyhow!(\n            \"rolled back\"\n        ),''')\nopen(p,'w').write(s)\nEOF"
);
case!(
    control_cd_python_stdin_no_arrow,
    assert_allow_both,
    "cd {WT} && python3 - <<'EOF'\np='crates/condukt/src/worktree.rs'\ns=open(p).read()\ns=s.replace('x','y')\nopen(p,'w').write(s)\nEOF"
);

// Fail-open guard: a real relative shell redirect after the same heredoc must
// stay non-Allow (the target is a protected hook path).
case!(
    control_cd_python_stdin_then_real_redirect_to_git_hooks,
    assert_not_allow_both,
    "cd {WT} && python3 - <<'EOF'\nprint('ok')\nEOF\necho x > ../../.git/hooks/pre-commit"
);
// And Python that really writes a protected path through `python3 -` must
// not be Allow either (the judge reads the Python, not just skips it).
case!(
    failopen_cd_python_stdin_writes_git_hooks,
    assert_not_allow_both,
    "cd {WT} && python3 - <<'EOF'\nopen('.git/hooks/pre-commit','w').write('#!/bin/sh\\nexit 0\\n')\nEOF"
);

// ===================================================================
// Case 2: shebang + `.git/...` fixture path in a heredoc written to .py
// ===================================================================

// Body written to a .py file and NOT run in the same command: data (user
// ruling). The body only builds a path under .git for a read.
case!(
    target_cat_py_shebang_dot_git_fixture_path_not_run,
    assert_allow_both,
    "cd {WT}; cat > scripts/test_precommit_timing.py <<'EOF'\n#!/usr/bin/env python3\nfrom pathlib import Path\nlog_path = Path('.') / \".git\" / \"precommit-timing.jsonl\"\nprint(log_path.is_file())\nEOF"
);

// As observed: the file is then run via pytest in the same command. The
// Python it contains only READS the .git path (never writes), so judged as
// Python it is Allow.
case!(
    target_cat_py_shebang_dot_git_fixture_path_then_pytest,
    assert_allow_both,
    "cd {WT}; cat > scripts/test_precommit_timing.py <<'EOF'\n#!/usr/bin/env python3\nfrom pathlib import Path\nlog_path = Path('.') / \".git\" / \"precommit-timing.jsonl\"\nprint(log_path.is_file())\nEOF\npython3 -m pytest scripts/test_precommit_timing.py 2>&1 | tail -2"
);

// Bisect: without the shebang line the verdict is still non-Allow (measured:
// Ask, "`log_path` is a command blastguard has no rule for, and .git is a
// protected gate/config path"), so the shebang is not the cause: the `.py`
// body is read as shell lines. Same expected verdict, same RED.
case!(
    target_cat_py_no_shebang_dot_git_fixture_path_not_run,
    assert_allow_both,
    "cd {WT}; cat > scripts/test_precommit_timing.py <<'EOF'\nfrom pathlib import Path\nlog_path = Path('.') / \".git\" / \"precommit-timing.jsonl\"\nprint(log_path.is_file())\nEOF"
);

// Fail-open guards: Python that really writes under .git, when run, must not
// be Allow; a shell redirect into .git/hooks must not be Allow.
case!(
    failopen_cat_py_shebang_writes_git_hooks_then_run,
    assert_not_allow_both,
    "cd {WT}; cat > scripts/evil.py <<'EOF'\n#!/usr/bin/env python3\nopen('.git/hooks/pre-commit','w').write('#!/bin/sh\\nexit 0\\n')\nEOF\npython3 scripts/evil.py"
);
case!(
    control_cat_redirect_to_git_hooks,
    assert_not_allow_both,
    "cd {WT}; cat > .git/hooks/pre-commit <<'EOF'\n#!/bin/sh\nexit 0\nEOF"
);
