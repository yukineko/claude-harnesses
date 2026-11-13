#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression (2026-11-13): an unquoted `#` that begins a word starts a comment that
//! runs to the end of the LINE, so an assignment-looking text after it never
//! executes.
//!
//! Measured in real bash: `bash -c 'echo # ; RM=/bin/echo; echo "word=[$RM]"'`
//! prints only an empty line. blastguard's line splitter (`split_segments`,
//! `src/detect/segment.rs`) does not model a mid-line `#` comment, so the
//! same-line variable resolution (`src/detect/vars.rs`,
//! `resolve_expanded_command_word`; redirect targets via `src/detect/redirect.rs`
//! `resolve_redirect_target_at`) can read `RM=/bin/echo` from the comment and
//! invent a Literal value — the permissive direction.
//!
//! OBSERVED 2026-11-13 at 96e8031b: every comment row (r1-r4c) already answers
//! as its unassigned control, while the same text WITHOUT `#` resolves (c3/c4
//! Allow) — so the comment-hidden assignment is not resolved today, and these
//! rows are kept as passing regression tests, not RED. r4a-r4c are only
//! sensitive once c3b/c4b pass (closed and-or list fix); before that they
//! passed vacuously. c3b/c4b were ignored RED until the fix in blastguard
//! 0.2.496 and now run un-ignored.
//!
//! Each case first pins the UNASSIGNED control (`$RM -rf /some/path` /
//! `echo x > $L` with no assignment at all), then requires the comment-hidden
//! assignment variant to answer exactly the same.
//!
//! Controls pin that `#` inside quotes and `#` not at word start are NOT
//! comments: the assignment after them still resolves (Deny for `/bin/rm`).
//!
//! DOES NOT PROVE: `#` after `$(`/backticks/`${`, here-documents, `#` in
//! `[[ ]]`/arithmetic contexts, `interactive_comments` off, or any non-Bash
//! tool path. RED rows pin the exact verdict of the unassigned control only.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let base = neutral_base::neutral_base()
            .join("comment_hides_assignment_20261113")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(proj.join("scratch")).unwrap();
        Fx { base, home, proj }
    }

    fn out_log(&self) -> String {
        self.proj.join("scratch/out.log").display().to_string()
    }

    fn bash(&self, command: &str) -> &'static str {
        let payload = serde_json::json!({
            "session_id": "comment-hides-assignment-20261113",
            "hook_event_name": "PreToolUse",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDECODE", "1")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.base);
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        let v = verdict(&s);
        println!("[{command:?}] => {v} {:?}", s.trim());
        v
    }
}

fn verdict(out: &str) -> &'static str {
    if out.trim().is_empty() {
        "allow"
    } else if out.contains(r#""permissionDecision":"deny""#) {
        "deny"
    } else if out.contains(r#""permissionDecision":"ask""#) {
        "ask"
    } else {
        "other"
    }
}

/// Pin the unassigned control (must not be Allow), then require `hidden`
/// (assignment only inside a comment) to answer exactly as the control.
#[track_caller]
fn same_as_unassigned(f: &Fx, unassigned: &str, hidden: &str) {
    let ctl = f.bash(unassigned);
    assert_ne!(ctl, "allow", "control `{unassigned}` must NOT be Allow");
    assert_ne!(ctl, "other", "control `{unassigned}`: unrecognised output");
    let v = f.bash(hidden);
    assert_ne!(
        v, "allow",
        "`{hidden:?}` must NOT be Allow (comment-hidden assignment)"
    );
    assert_eq!(
        v, ctl,
        "`{hidden:?}` must answer as the unassigned control `{unassigned}` ({ctl}); got {v}"
    );
}

const CMD_UNASSIGNED: &str = "$RM -rf /some/path";

// ------------------------------------------- comment-hidden assignment ----

#[test]
fn r1_comment_after_command_hides_rm_assignment() {
    let f = Fx::new("r1");
    same_as_unassigned(
        &f,
        CMD_UNASSIGNED,
        "echo hi # ; RM=/bin/echo\n$RM -rf /some/path",
    );
}

#[test]
fn r2_comment_without_space_hides_rm_assignment() {
    let f = Fx::new("r2");
    same_as_unassigned(
        &f,
        CMD_UNASSIGNED,
        "echo hi #; RM=/bin/echo\n$RM -rf /some/path",
    );
}

#[test]
fn r3_comment_hides_redirect_target_assignment() {
    let f = Fx::new("r3");
    let p = f.out_log();
    same_as_unassigned(
        &f,
        "echo x > $L",
        &format!("echo hi # ; L={p}\necho x > $L"),
    );
}

#[test]
fn r4a_closed_and_or_list_then_comment_hides_rm_assignment() {
    let f = Fx::new("r4a");
    same_as_unassigned(
        &f,
        CMD_UNASSIGNED,
        "true && true; echo hi # ; RM=/bin/echo\n$RM -rf /some/path",
    );
}

#[test]
fn r4b_closed_and_or_list_then_comment_without_space() {
    let f = Fx::new("r4b");
    same_as_unassigned(
        &f,
        CMD_UNASSIGNED,
        "true && true; echo hi #; RM=/bin/echo\n$RM -rf /some/path",
    );
}

#[test]
fn r4c_closed_and_or_list_then_comment_hides_redirect_assignment() {
    let f = Fx::new("r4c");
    let p = f.out_log();
    same_as_unassigned(
        &f,
        "echo x > $L",
        &format!("true && true; echo hi # ; L={p}\necho x > $L"),
    );
}

// ----------------------------------------------------------- CONTROLS ----

#[test]
fn c1_hash_inside_single_quotes_is_not_a_comment() {
    let f = Fx::new("c1");
    let v = f.bash("echo 'a # b'; RM=/bin/rm\n$RM -rf /some/path");
    assert_eq!(
        v, "deny",
        "quoted `#` is not a comment; RM resolves to /bin/rm"
    );
}

/// Sensitivity control: WITHOUT the `#`, the same text does assign, and the
/// resolver is expected to see it. If this were not Allow, the RED rows above
/// could pass merely because `/bin/echo` never resolves at all.
#[test]
fn c3_same_text_without_comment_resolves_command_word() {
    let f = Fx::new("c3");
    let v = f.bash("echo hi ; RM=/bin/echo\n$RM -rf /some/path");
    assert_eq!(v, "allow", "without `#`, RM resolves to /bin/echo");
}

/// Sensitivity control for the redirect rows.
#[test]
fn c4_same_text_without_comment_resolves_redirect_target() {
    let f = Fx::new("c4");
    let p = f.out_log();
    let v = f.bash(&format!("echo hi ; L={p}\necho x > $L"));
    assert_eq!(v, "allow", "without `#`, L resolves to a scratch file");
}

/// Sensitivity control for r4a/r4b. Until the closed-and-or-list fix lands
/// (see `assignment_after_closed_and_or_list_20261113.rs`), this is Ask at
/// HEAD, so r4a/r4b pass there WITHOUT proving anything about comments.
#[test]
fn c3b_without_comment_after_closed_and_or_list_resolves_command_word() {
    let f = Fx::new("c3b");
    let v = f.bash("true && true; echo hi ; RM=/bin/echo\n$RM -rf /some/path");
    assert_eq!(v, "allow", "without `#` after a closed and-or list");
}

/// Sensitivity control for r4c; same dependency as c3b.
#[test]
fn c4b_without_comment_after_closed_and_or_list_resolves_redirect_target() {
    let f = Fx::new("c4b");
    let p = f.out_log();
    let v = f.bash(&format!("true && true; echo hi ; L={p}\necho x > $L"));
    assert_eq!(v, "allow", "without `#` after a closed and-or list");
}

#[test]
fn c2_hash_not_at_word_start_is_not_a_comment() {
    let f = Fx::new("c2");
    let v = f.bash("echo a#b; RM=/bin/rm; $RM -rf /some/path");
    assert_eq!(
        v, "deny",
        "`a#b` is a word, not a comment; RM resolves to /bin/rm"
    );
}
