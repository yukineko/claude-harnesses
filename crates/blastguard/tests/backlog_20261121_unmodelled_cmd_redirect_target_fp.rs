#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! False positive observed 2026-11-21 (main 366eab917, session c1b3e891,
//! recorded in `~/.blastguard/blocks/blocks.jsonl`): a write whose redirect
//! target is a fixed, literal path was Ask because ANOTHER command on the same
//! line — one blastguard does not model (`jq`, `git commit`) — "may create or
//! rename an entry on this path". Reasons verbatim:
//!
//! 1. "'> …/scratchpad/vh.json': `jq '.tasks[0].title=\"fix rsync\" | …'
//!    decomp.routed.json` may create or rename an entry on this path and is
//!    not modelled, so blastguard cannot tell which file the write really
//!    lands on"
//! 2. "'> /Users/yuki/.condukt/worktrees/run-20261121-091940-12305-t1/crates/
//!    blastguard/tests/rsync_protected_destination_20261121.rs': `git commit
//!    -qm \"blastguard: RED tests for rsync protected destination\" --trailer
//!    \"Condukt-Task: $TASK_ID\"` may create or rename an entry on this path
//!    and is not modelled"
//!
//! Mechanism read from `src/detect/relink.rs` (`parse` / `is_relink_word` /
//! `mentions`) at 366eab917 — a reading, the test asserts only the verdict:
//! the word `rsync` INSIDE a quoted argument (a jq filter, a commit message)
//! makes the command "an unmodelled command that names a relinking verb", and
//! `mentions` then fires on a glob metachar (`[0]` of the jq filter) or an
//! unresolvable expansion (`$TASK_ID`) anywhere in that command, whatever path
//! the redirect targets. Commands on other segments count when the line has a
//! loop (`done` — in case 2 the word came from the heredoc BODY of a test
//! file) or runs before the write.
//!
//! Readings:
//! (a) FP — `jq <filter> <file>` and `git commit -m <msg> --trailer <t>` (no
//!     pathspec) cannot create or rename the redirect target: the `rsync` they
//!     carry is data (a jq string, a commit message), not an argv word that
//!     names a path or runs a relinking program.
//! (b) intended — an unmodelled command that names a relinking verb is
//!     undetermined, so Ask.
//! How to tell: whether the unmodelled command's argv can NAME the target path
//! (an operand equal to / resolving to it, or a glob/expansion in an OPERAND
//! position that could). Here the only glob/expansion is inside a jq filter
//! string / a `--trailer` value, so (a). The (b) control below (`ditto src
//! vh.json` — a real relinking program whose operand is the target) must stay
//! Ask.
//!
//! Written by an independent test author (not the implementer, CLAUDE.md
//! §2(a)). Fixture: `support/neutral_base.rs` location rule for the git main
//! tree `<base>/src/proj` and its linked worktree `<base>/src/
//! .harness-worktrees/wt-own` (session cwd and `CLAUDE_PROJECT_DIR`),
//! `<base>/home` = hook HOME; the scratch dir is a fixed literal directory
//! under the fixed temp root (`support/fixed_tmp.rs`), standing in for the
//! session scratchpad. Nothing is executed — the hook only judges the text.
//!
//! DOES NOT PROVE: subagent-mode verdicts; the reason text; that `jq` / `git
//! commit` are safe in general (only that these argv cannot name the target);
//! the exact elided (`…`) parts of the original commands.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/fixed_tmp.rs"]
mod fixed_tmp;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    scratch_root: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
        let _ = std::fs::remove_dir_all(&self.scratch_root);
    }
}

const TARGET: &str = "crates/x/tests/rsync_protected_destination_20261121.rs";

/// A Rust test body holding a shell `for … done` in a string literal, the way
/// a blastguard RED test file does.
/// `done` stands as its own whitespace-separated word (a comment), because
/// `relink::relinked` counts a later segment only when some segment holds the
/// word `done` exactly (measured at 3c792ee32: a body with only `done";` was
/// Allow).
const BODY: &str = "#[test]\nfn t() {\n    // for f in a b; do echo $f; done\n}\n";

impl Fx {
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn scratch(&self) -> PathBuf {
        self.scratch_root.join("scratchpad")
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
            "fixture git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("unmodelled-redir-{tag}-{n}"));
    let scratch_root = fixed_tmp::canonical_fixed_temp_root().join(format!(
        "bg-unmodelled-redir-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&scratch_root);
    for d in [
        base.join("home"),
        base.join("src").join("proj"),
        base.join("src").join(".harness-worktrees"),
        base.join("store"),
        base.join("tmpdir"),
        scratch_root.join("scratchpad"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fx = Fx { base, scratch_root };
    std::fs::write(fx.scratch().join("in.json"), "{}\n").unwrap();
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
    std::fs::create_dir_all(wt.join("crates/x/tests")).unwrap();
    fx
}

/// Run the hook once in the MAIN session (cwd = `CLAUDE_PROJECT_DIR` = own
/// worktree). Empty stdout is pass-through = Allow.
fn run_main(fx: &Fx, command: &str) -> (String, String) {
    let cwd = fx.wt();
    let p = serde_json::json!({
        "session_id": "unmodelled-cmd-redirect-target-fp-20261121",
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

/// Every command whose main-session decision is not `want`, with the
/// decision and reason (so one run shows every failing shape).
fn mismatches(fx: &Fx, cmds: &[String], want: &str) -> Vec<String> {
    cmds.iter()
        .filter_map(|c| {
            let (d, r) = run_main(fx, c);
            (d != want).then(|| format!("{c:?} => {d} ({r})"))
        })
        .collect()
}

#[test]
fn jq_filter_naming_rsync_does_not_relink_literal_redirect_target() {
    let fx = fixture("jq");
    let dir = fx.scratch().to_string_lossy().into_owned();
    let cmds = vec![
        // The observed shape (1): literal scratch dir via a variable, a loop,
        // and a jq filter whose STRING holds `rsync` and whose path holds `[0]`.
        format!(
            "S={dir}; cd $S && for v in h i; do case $v in h) jq '.tasks[0].title=\"fix rsync\"' in.json;; i) jq '.a=1' in.json;; esac > v$v.json; done"
        ),
        // Same, without the variable for the dir.
        format!(
            "cd {dir} && for v in h i; do case $v in h) jq '.tasks[0].title=\"fix rsync\"' in.json;; i) jq '.a=1' in.json;; esac > v$v.json; done"
        ),
        // Minimal: jq runs before the write, on its own segment.
        format!("cd {dir} && jq '.tasks[0].title=\"fix rsync\"' in.json; echo x > out.json"),
    ];
    let bad = mismatches(&fx, &cmds, "allow");
    assert!(bad.is_empty(), "expected Allow (FP):\n{}", bad.join("\n"));
}

#[test]
fn git_commit_message_naming_rsync_does_not_relink_heredoc_target() {
    let fx = fixture("commit");
    let wt = fx.wt().to_string_lossy().into_owned();
    let commit = "git commit -qm \"blastguard: RED tests for rsync protected destination\" --trailer \"Condukt-Task: $TASK_ID\"";
    let cmds = vec![
        // The observed shape (2): heredoc write of a test file, git add, git
        // commit with a message naming rsync and a `$TASK_ID` trailer.
        format!("cd {wt} && cat > {TARGET} <<'EOF'\n{BODY}EOF\ngit add {TARGET}\n{commit}"),
        format!("cat > {TARGET} <<'EOF'\n{BODY}EOF\ngit add {TARGET}\n{commit}"),
    ];
    let bad = mismatches(&fx, &cmds, "allow");
    assert!(bad.is_empty(), "expected Allow (FP):\n{}", bad.join("\n"));
}

/// Reading (b) control: an unmodelled command that really relinks — `ditto`
/// with the redirect target as its operand — stays Ask. Passing now and after
/// the fix is the point: the fix must not widen to real relinks.
#[test]
fn control_unmodelled_relinker_naming_target_stays_ask() {
    let fx = fixture("ctl-b");
    let dir = fx.scratch().to_string_lossy().into_owned();
    let cmds = vec![format!(
        "cd {dir} && for v in h; do ditto src vh.json; echo x > v$v.json; done"
    )];
    let bad = mismatches(&fx, &cmds, "ask");
    assert!(
        bad.is_empty(),
        "expected Ask (control):\n{}",
        bad.join("\n")
    );
}

/// System-harm control: the same shapes writing `/etc/hosts` stay Deny.
#[test]
fn control_same_shapes_into_etc_hosts_stay_deny() {
    let fx = fixture("ctl-sys");
    // The jq + `echo x > /etc/hosts` row was measured Ask (not Deny) at
    // 3c792ee32 — a separate fail-open, kept in its own file
    // `backlog_20261121_relink_unmodelled_ask_masks_system_write.rs`.
    let cmds = vec![
        "cat > /etc/hosts <<'EOF'\nx\nEOF\ngit commit -qm \"rsync\" --trailer \"Condukt-Task: $TASK_ID\""
            .to_string(),
    ];
    let bad = mismatches(&fx, &cmds, "deny");
    assert!(
        bad.is_empty(),
        "expected Deny (control):\n{}",
        bad.join("\n")
    );
}
