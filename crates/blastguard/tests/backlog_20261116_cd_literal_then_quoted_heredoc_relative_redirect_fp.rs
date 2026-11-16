#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED pin for a blastguard false positive logged 2026-11-16 (subagent, hook
//! call recorded in the session scratchpad as `b332.jsonl`): Ask -> Deny with
//!
//! > '> backlog_20261116_resolve_merge_task_named_flag.rs' is a RELATIVE
//! > redirect after an earlier cd/pushd/popd whose target directory blastguard
//! > could not statically resolve
//!
//! for a command of the shape (12 KB in the log)
//!
//! ```text
//! cd <literal absolute dir> && cat > <name>.rs <<'EOF'
//! <~12 KB of Rust source>
//! EOF
//! cd .. && . "$HOME/.cargo/env" && cargo fmt -p condukt 2>&1|tail -3; cd <literal absolute dir> && cargo test -p condukt --test <t> 2>&1 | tail -40
//! ```
//!
//! The `cd` target IS a literal absolute path, so "could not statically
//! resolve" is false: the FP.
//!
//! WHAT REPRODUCES (observed by driving the built binary, main mode; each
//! with the worktree path replaced by a fixture dir; verbatim reasons in the
//! `fp_*` test docs):
//!
//!  (a) the full original body (11 961 bytes): Ask (subagent: Deny).
//!  (b) a SHORT body of plain text, or of Rust-like text with `'` `"` `{}`:
//!      Allow. Not a reproducer.
//!  (d) the body padded past 8 KiB with plain text only: Allow. Not a
//!      reproducer. Size alone is not the trigger either.
//!  The reproducer is the CONJUNCTION of three things, found by delta-reducing
//!  the original body (kept 2 lines, then 2 lines of 4 chars each):
//!    1. a body line with an apostrophe (`it's`),
//!    2. a LATER body line containing a bare `>` token (`a > b`; `->` and
//!       `Vec<u8>` do not count, and `>` on the same line as, or before, the
//!       apostrophe does not count),
//!    3. the whole command longer than ~8 KiB (4.2 KB: Allow; 8.2 KB: Ask; the
//!       cap quoted in the task is the 8 KiB AST parse cap, so over it the AST
//!       layer is Undetermined and the old string reading decides),
//!  and the command must carry a later non-inert command after the close
//!  (the `cd .. && . "$HOME/.cargo/env" && cargo ...` tail): without it the
//!  body is dropped from the shell reading and the command is Allow.
//!
//! Layout:
//!  * `fp_*` (RED today, no `#[ignore]`): the reproducing shapes; each asserts
//!    Allow in BOTH modes (main = no `agent_id`, subagent = `agent_id` set).
//!    Why Allow is the right verdict: the resolved twin (`sanity_*`, same
//!    command with a body that does not reproduce) is Allow today in both
//!    modes, so nothing else in the command (cargo fmt / cargo test / the
//!    redirect into the literal cd target inside the fixture project) is
//!    blocked; only the misread of the heredoc body differs.
//!  * `sanity_*` (GREEN today): the non-reproducing twins above (short plain,
//!    short with the two trigger lines, padded plain only, padded with only
//!    one of the two trigger lines). They show the FP is caused by the
//!    body-content x size conjunction and not by the fixture.
//!  * `control_*` (GREEN today, must stay GREEN after the fix): the same
//!    commands with `cd $X` (a value only the shell knows) must stay blocked:
//!    exact `(main, subagent) = (ask, deny)` pair, with the reason of the main
//!    verdict naming "RELATIVE redirect" for the padded reproducer shape
//!    (that reason is CORRECT there, which is why it is pinned).
//!
//! DOES NOT PROVE: the fix location (any fix that makes the line-level
//! redirect/cwd bookkeeping ignore a quoted heredoc body to a code file would
//! turn the `fp_*` green); the exact size threshold (only 4.2 KB Allow and
//! 8.2 KB Ask were observed, and the 8 KiB figure is the task's, not measured
//! here); that other body characters (backticks, `$(`, ...) are fine: a body
//! containing a backtick pair (`` `a` ``) is a DIFFERENT, still-open reading
//! in the same neighbourhood (reason "the command word `` `a` `` is an
//! expansion ...", Ask even at 150 bytes) and is not covered here; the
//! `agent_id` (subagent) reason text beyond its not being asserted; behaviour
//! with a cwd other than the project dir; that `cargo fmt` / `cargo test`
//! would be Allow for a body that really did something (the body is inert
//! text here).

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

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn new(name: &str) -> Fx {
        let base = neutral_base::neutral_base()
            .join("backlog_20261116_cd_literal_heredoc_fp")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let proj = base.join("src/proj");
        std::fs::create_dir_all(&home).unwrap();
        for d in [
            ".claude-plugin",
            "crates/condukt/.claude-plugin",
            "crates/condukt/src",
            "crates/condukt/tests",
        ] {
            std::fs::create_dir_all(proj.join(d)).unwrap();
        }
        Fx { base, home, proj }
    }

    /// `@PROJ@` in `command` is the fixture project dir (also the payload
    /// `cwd` and `CLAUDE_PROJECT_DIR`).
    fn run(&self, command: &str, subagent: bool) -> (&'static str, String) {
        let command = command.replace("@PROJ@", &self.proj.display().to_string());
        let mut payload = serde_json::json!({
            "session_id": "cd-literal-heredoc-fp-20261116",
            "hook_event_name": "PreToolUse",
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if subagent {
            payload["agent_id"] = serde_json::json!("test-subagent");
        }
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
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        let v = verdict(&s);
        let reason = if v == "allow" {
            String::new()
        } else {
            let doc: serde_json::Value = serde_json::from_str(s.trim()).unwrap();
            doc["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap_or("")
                .to_string()
        };
        (v, reason)
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

/// `<cd> && cat > <file> <<'EOF' <body> EOF <tail>` with the tail of the
/// logged command (a `cd ..`, `$HOME`, `2>&1|tail` and a second literal `cd`).
fn command(cd_target: &str, file: &str, body: &str) -> String {
    format!(
        "cd {cd_target} && cat > {file} <<'EOF'\n{body}\nEOF\n\
cd .. && . \"$HOME/.cargo/env\" && cargo fmt -p condukt 2>&1|tail -3; cd @PROJ@ && \
cargo test -p condukt --test t 2>&1 | tail -40"
    )
}

const LITERAL_CD: &str = "@PROJ@/crates/condukt/tests";
const FILE: &str = "backlog_20261116_resolve_merge_task_named_flag.rs";

/// Plain lines (no quote, no `>`, no backtick, no `$`) appended to `body`
/// until the whole command is at least `min_bytes` long.
fn pad_to(body: &str, min_bytes: usize, cd_target: &str) -> String {
    let mut b = body.to_string();
    let mut i = 0;
    while command(cd_target, FILE, &b).len() < min_bytes {
        b.push_str(&format!(
            "\n// padding line {i} plain words only no special characters"
        ));
        i += 1;
    }
    b
}

/// Both-mode Allow for `command`.
fn assert_allow_both(tag: &str, command: &str) {
    let fx = Fx::new(tag);
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = fx.run(command, subagent);
        println!(
            "[{tag}] bytes={} subagent={subagent} => {d} {r:?}",
            command.len()
        );
        if d != "allow" {
            bad.push(format!("subagent={subagent} => {d} ({r})"));
        }
    }
    assert!(
        bad.is_empty(),
        "{tag}: expected Allow in both modes:\n{}",
        bad.join("\n")
    );
}

/// Control: the exact `(main, subagent)` pair; when `main_reason_has` is set
/// the main reason must contain it.
fn assert_blocked(tag: &str, command: &str, main_reason_has: Option<&str>) {
    let fx = Fx::new(tag);
    let (m, mr) = fx.run(command, false);
    let (s, sr) = fx.run(command, true);
    println!(
        "[{tag}] bytes={} main => {m} {mr:?}\n[{tag}] sub => {s} {sr:?}",
        command.len()
    );
    assert_eq!(
        (m, s),
        ("ask", "deny"),
        "control {tag} (main, subagent)\n main reason={mr}\n sub reason={sr}"
    );
    if let Some(needle) = main_reason_has {
        assert!(mr.contains(needle), "control {tag}: reason {mr:?}");
    }
}

// ---------------------------------------------------------------------
// FP targets (RED today)
// ---------------------------------------------------------------------

/// MINIMAL reproducer: two trigger lines (`it's`, then `a > b`) padded with
/// plain lines to ~8.4 KB, literal absolute `cd`.
///
/// Observed at 0.2.674 (main): `ask` "'> backlog_20261116_resolve_merge_task_
/// named_flag.rs' is a RELATIVE redirect after an earlier cd/pushd/popd whose
/// target directory blastguard could not statically resolve - ..." (the file
/// name is the one in `FILE`; the log's is the same). Subagent: `deny`
/// "blastguard: this call was made inside a subagent, ... denied instead of
/// asked" with the same reason below it.
#[test]
#[ignore = "RED: 2026-11-16 bgr-b332e799fdde — literal `cd /abs && cat > f.rs <<'EOF'` over 8 KiB whose body has an apostrophe line then a bare `>` line is refused as a RELATIVE redirect after an unresolvable cd — remove when fixed"]
fn fp_minimal_apostrophe_then_gt_line_padded_past_8kib() {
    let body = pad_to("it's\na > b", 8400, LITERAL_CD);
    assert_allow_both("fp_minimal", &command(LITERAL_CD, FILE, &body));
}

/// Same with the trigger written like the original (a doc comment with an
/// apostrophe, then a Rust comparison), over 9 KB.
#[test]
#[ignore = "RED: 2026-11-16 bgr-b332e799fdde — same FP with a Rust-like apostrophe doc comment then a comparison — remove when fixed"]
fn fp_rust_like_apostrophe_doc_then_comparison_padded_past_8kib() {
    let body = pad_to(
        "//! The guard isn't applied when the merge named no task.\nfn f(n: u32) -> bool {\n    n > 3\n}",
        9000,
        LITERAL_CD,
    );
    assert_allow_both("fp_rust_like", &command(LITERAL_CD, FILE, &body));
}

/// A realistic ~12 KB Rust test body (apostrophe in a doc comment, `->`,
/// `Vec<u8>`, `{}`, `"` strings, a comparison), the size of the logged call.
#[test]
#[ignore = "RED: 2026-11-16 bgr-b332e799fdde — same FP at the logged call's size (full Rust source body) — remove when fixed"]
fn fp_full_size_rust_source_body() {
    let mut body = String::from(
        "#![allow(clippy::unwrap_used)]\n//! Today's guard: the hold entry records whether the merge named a task.\n",
    );
    let mut i = 0;
    while command(LITERAL_CD, FILE, &body).len() < 12_000 {
        body.push_str(&format!(
            "\n#[test]\nfn case_{i}() -> Result<(), String> {{\n    let v: Vec<u8> = vec![1, 2, 3];\n    let s = \"a string\";\n    let c = 'x';\n    assert!(v.len() > 2, \"{{}} {{}}\", s, c);\n    Ok(())\n}}\n"
        ));
        i += 1;
    }
    assert_allow_both("fp_full_size", &command(LITERAL_CD, FILE, &body));
}

// ---------------------------------------------------------------------
// Sanity: non-reproducing twins (GREEN today)
// ---------------------------------------------------------------------

#[test]
fn sanity_short_body_with_both_trigger_lines_is_allow() {
    assert_allow_both(
        "sanity_short_both",
        &command(LITERAL_CD, FILE, "it's\na > b"),
    );
}

#[test]
fn sanity_short_plain_body_is_allow() {
    assert_allow_both("sanity_short_plain", &command(LITERAL_CD, FILE, "hello"));
}

#[test]
fn sanity_padded_plain_body_without_trigger_lines_is_allow() {
    let body = pad_to("hello", 9000, LITERAL_CD);
    assert_allow_both("sanity_padded_plain", &command(LITERAL_CD, FILE, &body));
}

#[test]
fn sanity_padded_with_apostrophe_only_is_allow() {
    let body = pad_to("it's", 9000, LITERAL_CD);
    assert_allow_both(
        "sanity_padded_apostrophe_only",
        &command(LITERAL_CD, FILE, &body),
    );
}

#[test]
fn sanity_padded_with_gt_before_apostrophe_is_allow() {
    let body = pad_to("a > b\nit's", 9000, LITERAL_CD);
    assert_allow_both(
        "sanity_padded_gt_then_apostrophe",
        &command(LITERAL_CD, FILE, &body),
    );
}

// ---------------------------------------------------------------------
// Controls: an undeterminable cd target must stay blocked (GREEN today)
// ---------------------------------------------------------------------

/// `cd $X` is a value only the shell knows: the relative redirect after it is
/// genuinely unplaceable, so the RELATIVE-redirect Ask is correct, also for
/// the padded reproducer body.
#[test]
fn control_undeterminable_cd_with_padded_reproducer_body_stays_blocked() {
    let body = pad_to("it's\na > b", 8400, "$X");
    assert_blocked(
        "control_undet_padded",
        &command("$X", FILE, &body),
        Some("RELATIVE redirect"),
    );
}

#[test]
fn control_undeterminable_cd_with_short_body_stays_blocked() {
    assert_blocked(
        "control_undet_short",
        &command("$X", FILE, "hello"),
        Some("RELATIVE redirect"),
    );
}

#[test]
fn control_undeterminable_cd_with_padded_plain_body_stays_blocked() {
    let body = pad_to("hello", 9000, "$X");
    assert_blocked(
        "control_undet_padded_plain",
        &command("$X", FILE, &body),
        Some("RELATIVE redirect"),
    );
}
