// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Suspected false positive: blastguard reads backticks inside a QUOTED
//! heredoc body (`<<'EOF'` ... `EOF`) as command substitution and judges the
//! "substituted" text as a command.
//!
//! Observed 2026-11-08 (live hook, subagent ab748b9fc26de406c), on
//! `cd <wt>/crates/blastguard && cat > <scratchpad>/edit.py <<'EOF'` whose
//! Python body held string literals such as
//! `'        "stash" => {\n            // \`git stash clear\` drops every stash entry'`,
//! followed by `EOF` and `python3 <scratchpad>/edit.py && ...`:
//!
//! ```text
//! PreToolUse:Bash hook error: git stash clear/drop irreversibly deletes stashed changes
//! ```
//!
//! With a quoted delimiter bash performs no expansion in the body, so the
//! backticks are literal bytes. The Python program never invokes git.
//!
//! Bisected cause: the redirect target's extension. See the "minimal
//! targets" section below.
//!
//! `target_*`: must be Allow (RED today), except `target_failopen_*`, which
//! must NOT be Allow (also RED today: Python code that really runs git). `control_*`: either already Allow
//! (data-extension and Python-string shapes, pinning the cause to the
//! extension) or a real command substitution (unquoted heredoc, or outside any
//! heredoc) that must stay restrictive, so this harness can observe the rule
//! at all. Each command runs as the main agent AND as a subagent through the
//! real binary. Allow is exactly empty stdout. `{DIR}` is the fixture's
//! absolute throwaway repo path. Nothing here is executed by a shell.
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
            .join("quoted_heredoc_backtick_fp_20261108")
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
            session: format!("hd-bt-{name}-{}", std::process::id()),
        }
    }

    /// (`"allow"` for empty stdout else the `permissionDecision`, reason).
    fn decision(&self, command: &str, subagent: bool) -> (String, String) {
        let command = command.replace("{DIR}", &self.repo.display().to_string());
        let mut payload = serde_json::json!({
            "session_id": format!("{}-{}", self.session, if subagent { "sub" } else { "main" }),
            "hook_event_name": "PreToolUse",
            "cwd": self.repo.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("agent-hd-bt");
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
                 heredoc body (literal data, no expansion, never run as shell) and must \
                 be Allow, got {d:?}: {r}"
            );
        }
    }

    fn assert_not_allow_both(&self, command: &str) {
        for sub in [false, true] {
            let (d, r) = self.decision(command, sub);
            assert_ne!(
                d, "allow",
                "`{command}` (subagent={sub}) really runs the command (a shell command \
                 substitution, or Python code that is then executed) and must not be \
                 Allow (reason was {r:?})"
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
}

// ---- controls: a quoted body headed to a DATA extension (.md) is already
// Allow, backticks or not (`detect/heredoc.rs` DATA_FILE_EXTENSIONS) ----

case!(
    control_quoted_md_backtick_git_stash_clear,
    assert_allow_both,
    "cat > {DIR}/notes.md <<'EOF'\n# notes\nRun `git stash clear` to drop every entry.\nEOF"
);
case!(
    control_quoted_md_backtick_rm_rf_root,
    assert_allow_both,
    "cat > {DIR}/notes.md <<'EOF'\n# notes\nNever run `rm -rf /`.\nEOF"
);
// A `.py` body whose backticks sit inside a Python quoted string is already
// Allow (the line's shell parse keeps them inside single quotes).
case!(
    control_observed_shape_python_string_backtick,
    assert_allow_both,
    "cd {DIR} && cat > {DIR}/edit.py <<'EOF'\ns = '        // `git stash clear` drops every stash entry'\nprint(s)\nEOF\npython3 {DIR}/edit.py && git diff --stat"
);

// ---- the exact observed command (verbatim from the subagent transcript) ----
// Paths are only text here: nothing is executed.

const OBSERVED_HEAD: &str = "cd /Users/yuki/src/.harness-worktrees/blastguard-stash-drop-allow/crates/blastguard && cat > /private/tmp/claude-502/-Users-yuki-src-harness/32935915-6275-4a76-b256-56d899bb4e1a/scratchpad/edit.py <<'EOF'\n";
const OBSERVED_BODY: &str = "import re\np='/Users/yuki/src/.harness-worktrees/blastguard-stash-drop-allow/crates/blastguard/src/detect/git.rs'\ns=open(p).read()\nold_start=s.index('        \"stash\" => {\\n            // `git stash clear` drops every stash entry')\nold_end=s.index('            // Round 2: the scan above is `.find(|t| !t.starts_with(\\'-\\'))`, so it\\n')\nnew='''        \"stash\" => {\n            // `git stash drop [<stash>]` / `git stash clear` / `git stash pop`\n            // are Allow: user ruling 2026-11-08, verbatim \"stash\u{306f}\u{81ea}\u{5206}\u{3067}\u{6d88}\u{3057}\u{3066}\n            // \u{3044}\u{3044}\u{3068}\u{3057}\u{305f}\u{3044}\" (Claude may delete stashes itself). Only the\n            // untracked-sweep flags below (`-u`/`-a`, the `git clean`\n            // hazard) are still judged in this arm.\n'''\ns=s[:old_start]+new+s[old_end:]\ns=s.replace('''            // Round 2: the scan above is `.find(|t| !t.starts_with('-'))`, so it\n            // SKIPS every flag \u{2014} and the flags are where the destruction is.\n            // `git stash -a`/`--all` and `git stash push -u` sweep untracked''','''            // Round 2: a first-non-flag subcommand scan SKIPS every flag \u{2014} and\n            // the flags are where the destruction is.\n            // `git stash -a`/`--all` and `git stash push -u` sweep untracked''')\ns=s.replace('''    // the object-store rule right after this, `stash clear/drop` and every\n    // protected-path check are unaffected.''','''    // the object-store rule right after this and every protected-path check\n    // are unaffected.''')\nopen(p,'w').write(s)\np='/Users/yuki/src/.harness-worktrees/blastguard-stash-drop-allow/crates/blastguard/src/rule_id.rs'\ns=open(p).read()\na=s.index('    // `git stash clear/drop`. Round 1 added the rule')\nb=s.index('    // Round 2: `git stash -u`/`-a`, which sweeps')\ns=s[:a]+s[b:]\ns=s.replace('''    // working tree. Its own id, not `git-stash-discard`: this one destroys\n    // UNTRACKED work (git has no copy), which is the `git clean` hazard, while\n    // clear/drop destroys already-stashed work.''','''    // working tree: UNTRACKED work (git has no copy), the `git clean` hazard.\n    // (`git stash clear/drop` and its `git-stash-discard` id were removed by\n    // user ruling 2026-11-08: Claude may delete stashes itself.)''')\ns=s.replace('''            (\"Bash\", json!({ \"command\": \"git stash clear\" })),\\n''','')\nopen(p,'w').write(s)\n";
const OBSERVED_TAIL: &str = "EOF\npython3 /private/tmp/claude-502/-Users-yuki-src-harness/32935915-6275-4a76-b256-56d899bb4e1a/scratchpad/edit.py && git diff --stat && sed -n 1248,1262p src/detect/git.rs; grep -n \"subcmd\" src/detect/git.rs | head";

fn observed_exact() -> String {
    format!("{OBSERVED_HEAD}{OBSERVED_BODY}{OBSERVED_TAIL}")
}

#[test]
fn target_observed_exact_command() {
    Fixture::new("target_observed_exact_command").assert_allow_both(&observed_exact());
}

// ---- minimal targets (bisected from the observed body) ----
//
// Bisection result (measured on main 5ee79efc's binary): the trigger is the
// redirect TARGET's extension, not a Python construct. A quoted body headed
// to `.md`/`.txt` is Allow; the same body headed to `.py`/`.rs`/`.sh`/no
// extension is shell-parsed line by line. A body line holding an unquoted
// backtick span then reads as command substitution (Deny via the stash rule);
// the same line without backticks is the unknown-verb Ask (subagent: Deny).
// Of the candidates named in the task, `'''`, `\'`, `"stash" => {` and
// `json!(...)` are each NOT needed: the single line below suffices.
//
// USER RULING 2026-11-08 (supersedes the extension part of 6cf12ce9): judge a
// quoted-heredoc body by the language of its target file. `.sh` and
// extension-less targets stay shell-judged (controls below). Non-shell code
// targets (`.py`, `.rs`, ...) are NOT read as shell lines; when the same
// command then runs the file with its interpreter (`python3 edit.py`), the
// body goes through the existing interpreter analysis.

case!(
    target_minimal_py_backtick_line,
    assert_allow_both,
    "cat > {DIR}/edit.py <<'EOF'\n`git stash clear`\nEOF"
);
case!(
    target_minimal_py_comment_backtick_line,
    assert_allow_both,
    "cat > {DIR}/edit.py <<'EOF'\n            // `git stash clear` drops every stash entry\nEOF"
);
// Same text without backticks, inside a VALID Python docstring: shows the FP
// is not backtick-specific. (A bare English line would be invalid Python,
// which a Python-aware judge may correctly Ask on.)
case!(
    target_minimal_py_plain_docstring,
    assert_allow_both,
    "cat > {DIR}/edit.py <<'EOF'\n\"\"\"\ngit stash clear drops every entry.\n\"\"\"\nEOF"
);

// ---- fail-open targets: a `.py` body that REALLY runs the command, and is
// then run, must not be Allow. Measured Allow on main 5ee79efc (RED). The same
// fix (judge a `.py` body as Python, not as shell lines) covers these and the
// false positives above. ----

case!(
    target_failopen_py_subprocess_run,
    assert_not_allow_both,
    "cat > {DIR}/edit.py <<'EOF'\nimport subprocess\nsubprocess.run([\"git\", \"stash\", \"clear\"])\nEOF\npython3 {DIR}/edit.py"
);
case!(
    target_failopen_py_os_system,
    assert_not_allow_both,
    "cat > {DIR}/edit.py <<'EOF'\nimport os\nos.system(\"git stash clear\")\nEOF\npython3 {DIR}/edit.py"
);

// ---- probes: same bodies with no backticks (disambiguates the cause) ----

case!(
    control_quoted_md_plain_git_stash_clear,
    assert_allow_both,
    "cat > {DIR}/notes.md <<'EOF'\n# notes\nRun git stash clear to drop every entry.\nEOF"
);
case!(
    control_observed_shape_python_string_plain,
    assert_allow_both,
    "cd {DIR} && cat > {DIR}/edit.py <<'EOF'\ns = '        // git stash clear drops every stash entry'\nprint(s)\nEOF\npython3 {DIR}/edit.py && git diff --stat"
);

// ---- controls: real substitution must stay restrictive ----

// Unquoted delimiter: bash DOES expand backticks in the body.
case!(
    control_unquoted_md_backtick_git_stash_clear,
    assert_not_allow_both,
    "cat > {DIR}/notes.md <<EOF\n# notes\nRun `git stash clear` to drop every entry.\nEOF"
);
case!(
    control_unquoted_md_backtick_rm_rf_root,
    assert_not_allow_both,
    "cat > {DIR}/notes.md <<EOF\n# notes\nNever run `rm -rf /`.\nEOF"
);
// Backtick substitution outside any heredoc.
case!(
    control_plain_backtick_git_stash_clear,
    assert_not_allow_both,
    "echo `git stash clear`"
);
case!(
    control_plain_backtick_rm_rf_root,
    assert_not_allow_both,
    "echo `rm -rf /`"
);
