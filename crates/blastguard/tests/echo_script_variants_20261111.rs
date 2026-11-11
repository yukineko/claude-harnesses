// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Variants of "echo/printf writes a script, another stage runs it" (2026-11-11).
//!
//! Follow-up to `script_write_then_run_harm_20261111.rs` (blastguard 0.2.447
//! judges echo/printf output written to a file another stage runs). Each
//! variant here changes ONE thing about how the file is written or run.
//!
//! Expected decisions come from the rules, not from the implementation:
//! - `harm_*`: the effect runs `rm -rf ~` (or `shutil.rmtree('/etc')`) and
//!   the script content is fully reproducible from the command text. System
//!   harm is Deny exactly (user memory "system harm always deny").
//! - `undet_*`: the script content cannot be reproduced statically (`$X`,
//!   a printf format). Undeterminable resolves to the restrictive side
//!   (CLAUDE.md §3), so anything except Allow passes; Ask or Deny are both
//!   acceptable.
//! - `ctl_*`: harmless (the harmful text is never run, or never written as
//!   code). Must be Allow; non-Allow is a false
//!   positive.
//!
//! Each command runs through the real binary as the main agent AND as a
//! subagent. Nothing is executed. The cwd handed to the hook is a git repo.
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
        let base = neutral_base::neutral_base()
            .join("echo_script_variants_20261111")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let cwd = base.join("src/repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let f = Fixture {
            base,
            home,
            cwd,
            session: format!("esv-{name}-{}", std::process::id()),
        };
        git(&f.cwd, &["init", "-q"]);
        std::fs::write(f.cwd.join("f.txt"), "x\n").unwrap();
        git(&f.cwd, &["add", "-A"]);
        git(&f.cwd, &["commit", "-q", "-m", "init"]);
        f
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
            payload["agent_id"] = serde_json::json!("agent-esv");
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

    fn deny_both(&self, command: &str) {
        for sub in [false, true] {
            let (d, r) = self.decision(command, sub);
            assert_eq!(
                d, "deny",
                "`{command}` (subagent={sub}) runs a script whose reproducible \
                 content is system harm; it must be Deny, got {d:?}: {r}"
            );
        }
    }

    fn not_allow_both(&self, command: &str) {
        for sub in [false, true] {
            let (d, r) = self.decision(command, sub);
            assert!(
                d == "ask" || d == "deny",
                "`{command}` (subagent={sub}) runs a script whose content \
                 cannot be reproduced statically; undeterminable must be Ask \
                 or Deny, got {d:?}: {r}"
            );
        }
    }

    fn allow_both(&self, command: &str) {
        for sub in [false, true] {
            let (d, r) = self.decision(command, sub);
            assert_eq!(
                d, "allow",
                "`{command}` (subagent={sub}) runs nothing harmful; it must be \
                 Allow, got {d:?}: {r}"
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

// ---- harm: write `rm -rf ~`, then run it -------------------------------

// V1. Redirect glued to the quoted word, `&&` sequencing.
case!(
    harm_v01_glued_redirect_and,
    deny_both,
    "echo 'rm -rf ~'>x.sh && sh x.sh"
);

// V1b. Glued redirect, `;` sequencing.
case!(
    harm_v01b_glued_redirect_semicolon,
    deny_both,
    "echo 'rm -rf ~'>x.sh;sh x.sh"
);

// V2. Written via `tee`.
case!(
    harm_v02_tee_then_sh,
    deny_both,
    "echo 'rm -rf ~' | tee x.sh && sh x.sh"
);

// V3. Run by piping the file into sh.
case!(
    harm_v03_cat_pipe_sh,
    deny_both,
    "echo 'rm -rf ~' > x.sh && cat x.sh | sh"
);

// V4. chmod +x and run by path.
case!(
    harm_v04_chmod_dot_slash,
    deny_both,
    "echo 'rm -rf ~' > x.sh && chmod +x x.sh && ./x.sh"
);

// V5. Sourced with `.`.
case!(
    harm_v05_dot_source,
    deny_both,
    "echo 'rm -rf ~' > x.sh && . ./x.sh"
);

// V6. Sourced with `source`.
case!(
    harm_v06_source_builtin,
    deny_both,
    "echo 'rm -rf ~' > x.sh && source x.sh"
);

// V7. Fed to sh on stdin redirect.
case!(
    harm_v07_sh_stdin_redirect,
    deny_both,
    "echo 'rm -rf ~' > x.sh && sh < x.sh"
);

// V8. printf split across `>` then `>>`; the file holds `rm -rf ~`.
case!(
    harm_v08_printf_split_append,
    deny_both,
    "printf 'rm -rf ' > x; printf '~' >> x; sh x"
);

// V9. Run inside a subshell.
case!(
    harm_v09_subshell,
    deny_both,
    "echo 'rm -rf ~' > x.sh && (sh x.sh)"
);

// V10. Run through `env bash`.
case!(
    harm_v10_env_bash,
    deny_both,
    "echo 'rm -rf ~' > x.sh && env bash x.sh"
);

// V11. Unquoted: echo expands `~` to $HOME, so the script is `rm -rf $HOME`.
case!(
    harm_v11_unquoted_bash_e,
    deny_both,
    "echo rm -rf ~ > x.sh && bash -e x.sh"
);

// V12. Run, then overwritten harmlessly AFTER it ran.
case!(
    harm_v12_overwritten_after_run,
    deny_both,
    "echo 'rm -rf ~' > x.sh; sh x.sh; echo hi > x.sh"
);

// V13. echo -e with a trailing escape; first line is still `rm -rf ~`.
case!(
    harm_v13_echo_e_newline,
    deny_both,
    "echo -e 'rm -rf ~\\n' > x.sh && sh x.sh"
);

// V14. Non-.sh file name run by sh.
case!(
    harm_v14_txt_run_by_sh,
    deny_both,
    "echo 'rm -rf ~' > notes.txt && sh notes.txt"
);

// V15. Echoed python script that removes /etc.
case!(
    harm_v15_echo_python_rmtree_etc,
    deny_both,
    "echo \"import shutil; shutil.rmtree('/etc')\" > x.py && python3 x.py"
);

/// V16. Harmful text "overwritten" with a harmless script before it runs.
///
/// USER RULING (2026-11-11): "Back to Deny". Inherited noclobber can make
/// the second `>` fail, so the harmful content can still be what runs, and
/// system harm is Deny. (Formerly `ctl_c01_overwritten_before_run`, Allow.)
#[test]
fn harm_v16_overwritten_before_run_noclobber() {
    Fixture::new("harm_v16_overwritten_before_run_noclobber")
        .deny_both("echo 'rm -rf ~' > x.sh; echo 'echo hi' > x.sh; sh x.sh");
}

// ---- harm: fallback paths (overwrite may fail / write is scoped) -------

// F1. Write failure is swallowed with `|| true`; the script still runs.
case!(
    harm_f01_or_true_then_run,
    deny_both,
    "echo 'rm -rf ~' > x.sh || true; sh x.sh"
);

// F2. Written inside a for loop body.
case!(
    harm_f02_for_loop_write,
    deny_both,
    "for i in 1; do echo 'rm -rf ~' > x.sh; done; sh x.sh"
);

// F3. `set -C` makes the harmless overwrite fail; the harmful script runs.
case!(
    harm_f03_set_noclobber_overwrite_fails,
    deny_both,
    "set -C; echo 'rm -rf ~' > x.sh; echo hi > x.sh; sh x.sh"
);

// F4. `chmod 000` can make the harmless overwrite fail.
case!(
    harm_f04_chmod_000_overwrite_fails,
    deny_both,
    "echo 'rm -rf ~' > x.sh; chmod 000 x.sh; echo hi > x.sh; sh x.sh"
);

// F5. Both writes in one pipeline: their order is a race.
case!(
    harm_f05_pipeline_write_race,
    deny_both,
    "echo 'rm -rf ~' > x.sh | echo hi > x.sh; sh x.sh"
);

// F6. Written inside a subshell, run outside it.
case!(
    harm_f06_subshell_write,
    deny_both,
    "(echo 'rm -rf ~' > x.sh) && sh x.sh"
);

// F7. Echoed node script that recursively deletes $HOME.
case!(
    harm_f07_echo_node_rmsync_home,
    deny_both,
    "echo \"require('fs').rmSync(process.env.HOME,{recursive:true})\" > x.js && node x.js"
);

// ---- undeterminable content: not Allow ---------------------------------

// U1. Content comes from a variable.
case!(
    undet_u01_echo_variable,
    not_allow_both,
    "echo \"$X\" > x.sh && sh x.sh"
);

// U2. printf with a format string.
case!(
    undet_u02_printf_format,
    not_allow_both,
    "printf '%s\\n' 'rm -rf ~' > x.sh && sh x.sh"
);

// ---- controls: Allow ---------------------------------------------------

// C2. Harmful text used as a commit message, never run.
case!(
    ctl_c02_commit_message_file,
    allow_both,
    "echo 'rm -rf ~ is bad' > /tmp/msg && git commit -F /tmp/msg"
);

// C3. Harmful text written to notes and only read back.
case!(
    ctl_c03_notes_cat,
    allow_both,
    "echo 'rm -rf ~' > notes.txt && cat notes.txt"
);

// C4. Harmless script, chmod +x, run by path.
case!(
    ctl_c04_harmless_dot_slash,
    allow_both,
    "echo 'echo hi' > x.sh && chmod +x x.sh && ./x.sh"
);

// C5. Harmless env file sourced.
case!(
    ctl_c05_source_env_file,
    allow_both,
    "echo \"export PATH=/opt/bin\" > env.sh && . env.sh"
);
