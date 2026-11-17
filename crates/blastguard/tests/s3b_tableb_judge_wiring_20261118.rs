#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! S3b `red-tests`: wiring the AST judge (`src/detect/ast/judge.rs`) into the
//! final decision ONLY for commands that have a Table B row
//! (`src/detect/ast/SUBSET.md`). User ruling 2026-11-18 (S3b):
//!
//! * for a Table B command, an Undetermined result — an option not listed in
//!   its row ("an option not in the row's list ... → Undetermined", Table B
//!   common rules) or an operand the evaluator cannot resolve (Table C
//!   Unresolved row) — is Ask (main) / Deny (subagent), never Allow;
//! * for a command WITHOUT a Table B row the analyze-wide Undetermined
//!   handling is unchanged (this file pins today's decision there as a
//!   control and asserts no change);
//! * an unknown program (no effect model) stays Allow.
//!
//! Written by an independent test author (not the S3b implementer; CLAUDE.md
//! §2(a)). Expectations come from SUBSET.md and the ruling, not from today's
//! behaviour, except for the (c) controls, which are measured pins by design.
//! Tests observed failing at origin/main carry `#[ignore = "RED: ..."]`.
//!
//! Content rule: no real destructive payloads. Every target is a
//! non-existent sentinel (`/tmp/bg-s3b-sentinel-*`) or a path inside the
//! hermetic fixture; the hook only JUDGES the text, nothing is executed.
//!
//! Harness: identical to `ast_effect_rules_20261117.rs` (hermetic env,
//! `support/neutral_base.rs` fixture base, main and subagent mode).
//!
//! Assertion vocabulary:
//!   * `allow`    — both modes Allow.
//!   * `restrict` — main is Ask or stricter, subagent is Deny.
//!   * `pinned`   — both modes equal the decision measured at origin/main
//!     (1dfa60681, 2026-11-18).
//!
//! DOES NOT PROVE: which rule produced a verdict (only the decision is
//! asserted, not the reason); that an unlisted option is judged
//! Undetermined for every Table B row (only the rows below are probed);
//! behaviour with `TMPDIR` set; System / Protected / Project / Other cells
//! (covered elsewhere or legacy); that the (c) pins are the RIGHT verdict —
//! they only detect a change.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    /// The plain project dir (no git): the default payload cwd.
    fn proj(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    /// A linked-worktree-shaped checkout under the hermetic
    /// `$HOME/.condukt/worktrees` storage root (a `.git` FILE starting
    /// `gitdir:`), with a `sub/f` inside.
    fn own_worktree(&self) -> PathBuf {
        let wt = self
            .home()
            .join(".condukt")
            .join("worktrees")
            .join("bg-s3b-wt");
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        std::fs::write(wt.join("sub").join("f"), "x\n").unwrap();
        std::fs::write(
            wt.join(".git"),
            "gitdir: /nonexistent/bg-s3b/.git/worktrees/bg-s3b-wt\n",
        )
        .unwrap();
        wt
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "s3b-tableb-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home").join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base }
}

/// Run the hook once; `(decision, reason)`.
///
/// Order is load-bearing: (1) a non-zero / signalled exit panics; (2) only
/// then is an EMPTY stdout read as Allow; (3) non-JSON stdout panics.
fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "s3b-tableb-judge-20261118",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", fx.home())
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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

fn allow_at(fx: &Fx, cwd: &Path, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, cwd, command, subagent);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: {command:?} expected Allow, got {d} ({r})"
        );
    }
}

fn restrict_at(fx: &Fx, cwd: &Path, command: &str) {
    let (dm, rm) = run_as(fx, cwd, command, false);
    assert!(
        dm == "ask" || dm == "deny",
        "main: {command:?} expected Ask or stricter, got {dm} ({rm})"
    );
    let (ds, rs) = run_as(fx, cwd, command, true);
    assert_eq!(
        ds, "deny",
        "subagent: {command:?} expected Deny, got {ds} ({rs})"
    );
}

fn pinned_at(fx: &Fx, cwd: &Path, command: &str, main: &str, sub: &str) {
    let (dm, rm) = run_as(fx, cwd, command, false);
    assert_eq!(
        dm, main,
        "main: {command:?} pinned {main} (origin/main 1dfa60681), got {dm} ({rm})"
    );
    let (ds, rs) = run_as(fx, cwd, command, true);
    assert_eq!(
        ds, sub,
        "subagent: {command:?} pinned {sub} (origin/main 1dfa60681), got {ds} ({rs})"
    );
}

fn allow(tag: &str, command: &str) {
    let fx = fixture(tag);
    allow_at(&fx, &fx.proj(), command);
}

fn restrict(tag: &str, command: &str) {
    let fx = fixture(tag);
    restrict_at(&fx, &fx.proj(), command);
}

fn pinned(tag: &str, command: &str, main: &str, sub: &str) {
    let fx = fixture(tag);
    pinned_at(&fx, &fx.proj(), command, main, sub);
}

// ---------------------------------------------------------------------------
// (a) Table B command + unlisted option or unresolvable operand on an
// otherwise Scratch target -> Undetermined / Unresolved -> Ask / Deny.
// The target is Scratch on purpose: the only restrictive factor is the
// unmodelled option (or the unplaceable operand).
// ---------------------------------------------------------------------------

/// B.1 `rm` lists `-r/-R/--recursive -f -i -I -v -d`; `-P` is not listed.
#[test]
#[ignore = "RED: S3b Table B rm option -P not modelled -> Undetermined"]
fn a_rm_unlisted_option_restricts() {
    restrict("arm", "rm -P /tmp/bg-s3b-sentinel-rm");
}

/// B.1 `cp` (destination `Truncate`): `-p` is not in the row (the S3 status
/// line of SUBSET.md names `cp -p` as an unmodelled option).
#[test]
#[ignore = "RED: S3b Table B cp option -p not modelled -> Undetermined"]
fn a_cp_unlisted_option_restricts() {
    restrict(
        "acp",
        "cp -p /tmp/bg-s3b-sentinel-cp-a /tmp/bg-s3b-sentinel-cp-b",
    );
}

/// B.1 `mv` lists `-f -i -n -v -T -t`; `-b` is not listed.
#[test]
#[ignore = "RED: S3b Table B mv option -b not modelled -> Undetermined"]
fn a_mv_unlisted_option_restricts() {
    restrict(
        "amv",
        "mv -b /tmp/bg-s3b-sentinel-mv-a /tmp/bg-s3b-sentinel-mv-b",
    );
}

/// B.1 `tee` lists `-a -i -p`; `-u` is not listed.
#[test]
#[ignore = "RED: S3b Table B tee option -u not modelled -> Undetermined"]
fn a_tee_unlisted_option_restricts() {
    restrict("atee", "echo hi | tee -u /tmp/bg-s3b-sentinel-tee");
}

/// B.1 `chmod` models `-R` (and `--reference=F`); `-h` is not listed.
#[test]
#[ignore = "RED: S3b Table B chmod option -h not modelled -> Undetermined"]
fn a_chmod_unlisted_option_restricts() {
    restrict("achm", "chmod -h 644 /tmp/bg-s3b-sentinel-chmod");
}

/// B.1 `rm` + A.3 "unquoted glob → value Undetermined" → Table C
/// Unresolved x Delete.
#[test]
#[ignore = "RED: S3b Table C Unresolved x Delete (rm unquoted glob operand)"]
fn a_rm_unquoted_glob_operand_restricts() {
    restrict("armglob", "rm /tmp/bg-s3b-sentinel-glob-*");
}

/// B.1 `mv` destination + A.3 positional parameter → Unresolved x Truncate.
#[test]
fn a_mv_positional_dest_restricts() {
    restrict("amvpos", "mv /tmp/bg-s3b-sentinel-mv-a \"$1\"");
}

/// B.1 `chmod` + A.3 `~user` → value Undetermined → Unresolved x Metadata.
#[test]
#[ignore = "RED: S3b Table C Unresolved x Metadata (chmod ~user operand)"]
fn a_chmod_tilde_user_operand_restricts() {
    restrict("achmtu", "chmod 644 ~bgs3bnouser/f");
}

/// B.1 `cp` destination + A.3 command substitution → Unresolved x Truncate.
#[test]
#[ignore = "RED: S3b Table C Unresolved x Truncate (cp dest from command substitution)"]
fn a_cp_cmdsubst_dest_restricts() {
    restrict(
        "acpsub",
        "cp /tmp/bg-s3b-sentinel-cp-a \"$(cat /tmp/bg-s3b-sentinel-list)\"",
    );
}

// ---------------------------------------------------------------------------
// (b) Controls: same commands, listed options, resolvable Scratch or
// own-worktree targets -> Table C Scratch / OwnWorktree = Allow.
// ---------------------------------------------------------------------------

/// Table C Scratch x Delete(non-rec), listed `-f`.
#[test]
fn b_rm_listed_option_scratch_allows() {
    allow("brm", "rm -f /tmp/bg-s3b-sentinel-rm");
}

/// Table C Scratch x Truncate (cp destination), no option.
#[test]
fn b_cp_scratch_allows() {
    allow(
        "bcp",
        "cp /tmp/bg-s3b-sentinel-cp-a /tmp/bg-s3b-sentinel-cp-b",
    );
}

/// Table C Scratch x Delete(rec) + Truncate (mv), listed `-f`.
#[test]
fn b_mv_listed_option_scratch_allows() {
    allow(
        "bmv",
        "mv -f /tmp/bg-s3b-sentinel-mv-a /tmp/bg-s3b-sentinel-mv-b",
    );
}

/// Table C Scratch x Append (tee -a), listed `-a`.
#[test]
fn b_tee_listed_option_scratch_allows() {
    allow("btee", "echo hi | tee -a /tmp/bg-s3b-sentinel-tee");
}

/// Table C Scratch x Metadata (chmod -R), listed `-R`. Observed at
/// origin/main: main Ask from the legacy confined-`chmod -R` reading, which
/// ruling 14 keeps raise-only until S6 (so S3b is not expected to make it
/// GREEN).
#[test]
#[ignore = "RED: S6 Table C Scratch x Metadata chmod -R (legacy confined Ask raise-only until S6, ruling 14)"]
fn b_chmod_listed_option_scratch_allows() {
    allow("bchm", "chmod -R 644 /tmp/bg-s3b-sentinel-chmod");
}

/// Table C Scratch x Metadata (chmod, non-recursive): the passing twin of
/// the `-R` case above.
#[test]
fn b_chmod_scratch_allows() {
    allow("bchm0", "chmod 644 /tmp/bg-s3b-sentinel-chmod");
}

/// Table C OwnWorktree x Metadata, relative target, cwd = own worktree.
#[test]
fn b_chmod_own_worktree_relative_allows() {
    let fx = fixture("bchmwt");
    let wt = fx.own_worktree();
    allow_at(&fx, &wt, "chmod 644 sub/f");
}

/// Table C OwnWorktree x Truncate (cp destination), relative, no option.
#[test]
fn b_cp_own_worktree_relative_allows() {
    let fx = fixture("bcpwt");
    let wt = fx.own_worktree();
    allow_at(&fx, &wt, "cp sub/f sub/g");
}

/// Table C OwnWorktree x Delete(non-rec), relative, listed `-f`.
#[test]
fn b_rm_own_worktree_relative_allows() {
    let fx = fixture("brmwt");
    let wt = fx.own_worktree();
    allow_at(&fx, &wt, "rm -f sub/f");
}

// ---------------------------------------------------------------------------
// (c) Controls: commands WITHOUT a Table B row with an Undetermined
// construct. The S3b ruling leaves their handling unchanged, so today's
// decision (measured at origin/main 1dfa60681, 2026-11-18) is pinned.
// ---------------------------------------------------------------------------

/// `touch` has no Table B row; operand from a command substitution.
#[test]
fn c_touch_cmdsubst_operand_pinned() {
    pinned(
        "ctouch",
        "touch \"$(cat /tmp/bg-s3b-sentinel-list)\"",
        "allow",
        "allow",
    );
}

/// `ls` has no Table B row; unquoted glob and positional operand.
#[test]
fn c_ls_glob_positional_pinned() {
    pinned("cls", "ls /tmp/bg-s3b-sentinel-* \"$1\"", "allow", "allow");
}

/// `mkdir` has no Table B row; operand from an unbound variable.
#[test]
fn c_mkdir_unbound_var_pinned() {
    pinned("cmkdir", "mkdir -p \"$x\"", "allow", "allow");
}

// ---------------------------------------------------------------------------
// (d) Unknown program (no effect model) -> Allow (ruling 2).
// ---------------------------------------------------------------------------

#[test]
fn d_unknown_program_allows() {
    allow(
        "dunk",
        "bg-s3b-unknown-prog --frob /tmp/bg-s3b-sentinel-unk",
    );
}

/// Unknown program with an option and an unresolvable operand: still no
/// effect model -> Allow.
#[test]
fn d_unknown_program_unresolved_operand_allows() {
    allow("dunk2", "bg-s3b-unknown-prog -Z \"$x\"");
}
