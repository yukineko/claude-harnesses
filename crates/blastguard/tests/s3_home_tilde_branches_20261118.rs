#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! S3 ruling 12 branch tests: `$HOME` / `${HOME}` / leading `~` are the hook
//! environment's `HOME` (blastguard `src/detect/ast/SUBSET.md`, ruling 12 and
//! Table A.3), plus the restrictive branches the S3 implementation keeps
//! (rebinding, `unset`, `export`, `~user`, arithmetic lines, a HOME that is
//! not a plain path).
//!
//! Written by an independent test author (not the S3 implementer; CLAUDE.md
//! §2(a)), against commit 1c6651da4. Expectations come from SUBSET.md; where
//! SUBSET.md is silent and the expectation is the restrictive side chosen by
//! the coordinator, the test doc says so.
//!
//! Content rule: no real destructive payloads. Every target is a
//! non-existent sentinel path; the hook only JUDGES the command text,
//! nothing is executed.
//!
//! Harness (same as `ast_effect_rules_20261117.rs`): the hook binary runs
//! with `env_clear`, `HOME=<fixture>/home` (or an explicit override),
//! `CLAUDECODE=1`, `CLAUDE_CODE_ENTRYPOINT=cli`, `CLAUDE_PROJECT_DIR` = the
//! payload cwd, `BLASTGUARD_APPROVALS_DIR=<fixture>/store`, no `TMPDIR`, in
//! main mode and in subagent mode (`agent_id` in the payload), bounded by
//! `support/bounded.rs`.
//!
//! Assertion vocabulary:
//!   * `allow`     — both modes Allow.
//!   * `not_allow` — main is Ask or Deny AND subagent is Ask or Deny (the
//!     coordinator's requested strength; it does not pin Ask vs Deny).
//!
//! DOES NOT PROVE (whole file): which rule produced a verdict (only the
//! decision is asserted); behaviour with `TMPDIR` set; that the `not_allow`
//! cases are refused for the HOME-specific reason (a stricter unrelated
//! rule would also satisfy them); Ask vs Deny strength in the restrictive
//! cases.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
    /// The `HOME` the hook process sees.
    home: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fx {
    fn proj(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    /// A linked-worktree-shaped checkout under the hermetic
    /// `$HOME/.condukt/worktrees` storage root (a `.git` FILE starting
    /// `gitdir:`), used as the session cwd.
    fn own_worktree(&self) -> PathBuf {
        let wt = self
            .home
            .join(".condukt")
            .join("worktrees")
            .join("bg-s3-home-wt");
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        std::fs::write(
            wt.join(".git"),
            "gitdir: /nonexistent/bg-s3-home/.git/worktrees/bg-s3-home-wt\n",
        )
        .unwrap();
        wt
    }
}

fn fixture_with_home_name(tag: &str, home_name: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "s3-home-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join(home_name);
    std::fs::create_dir_all(home.join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base, home }
}

fn fixture(tag: &str) -> Fx {
    fixture_with_home_name(tag, "home")
}

/// Run the hook once; `(decision, reason)`. Non-zero exit panics before an
/// empty stdout can be read as Allow; non-JSON stdout panics.
fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "s3-home-tilde-20261118",
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
        .env("HOME", &fx.home)
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

fn not_allow_at(fx: &Fx, cwd: &Path, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, cwd, command, subagent);
        assert!(
            d == "ask" || d == "deny",
            "subagent={subagent}: {command:?} expected Ask or Deny, got {d} ({r})"
        );
    }
}

fn allow(tag: &str, command: &str) {
    let fx = fixture(tag);
    allow_at(&fx, &fx.proj(), command);
}

fn not_allow(tag: &str, command: &str) {
    let fx = fixture(tag);
    not_allow_at(&fx, &fx.proj(), command);
}

// ---------------------------------------------------------------------------
// Known HOME -> Scratch (cache root) -> Allow (ruling 12 + Table C Scratch x
// Delete).
// ---------------------------------------------------------------------------

/// Control: the absolute spelling of the cache-root target allows. Anchors
/// the three spellings below to the same cell.
/// DOES NOT PROVE: anything about `$HOME` / `~` resolution.
#[test]
fn control_rm_r_cache_absolute_allows() {
    let fx = fixture("abs");
    let t = fx.home.join(".cache").join("bg-sentinel");
    allow_at(&fx, &fx.proj(), &format!("rm -r {}", t.display()));
}

/// Ruling 12: a bare word-start `~` is the hook HOME, so `~/.cache/x` is the
/// cache deletion root (Scratch) -> Allow.
/// DOES NOT PROVE: that `~` resolves to THIS fixture's HOME rather than any
/// cache-root-looking path (the HOME-rebinding tests below cover that).
#[test]
fn tilde_cache_allows() {
    allow("tilde", "rm -r ~/.cache/bg-sentinel");
}

/// Ruling 12: `$HOME` is the hook HOME.
/// DOES NOT PROVE: the quoted `"$HOME/..."` spelling.
#[test]
fn dollar_home_cache_allows() {
    allow("dhome", "rm -r $HOME/.cache/bg-sentinel");
}

/// Ruling 12: `${HOME}` is the hook HOME.
/// DOES NOT PROVE: other parameter operations on HOME (`${HOME:-x}`).
#[test]
fn braced_home_cache_allows() {
    allow("bhome", "rm -r ${HOME}/.cache/bg-sentinel");
}

// ---------------------------------------------------------------------------
// HOME rebound / unset / exported on the line -> not the hook HOME.
// ---------------------------------------------------------------------------

/// Table A.3: HOME assigned on the line follows the named-variable rule;
/// `/etc/bg-sentinel` is a System target -> not Allow.
/// DOES NOT PROVE: Deny strength (System cell) — only "not Allow".
#[test]
fn home_rebound_then_dollar_home_not_allow() {
    not_allow("rebound", "HOME=/etc; rm -r $HOME/bg-sentinel");
}

/// `unset HOME`: bash then expands `~` from the passwd entry, not the hook
/// HOME; the hook HOME binding must be dropped -> not Allow.
/// DOES NOT PROVE: how `$HOME` (rather than `~`) is treated after `unset`.
#[test]
fn unset_home_then_tilde_not_allow() {
    not_allow("unset", "unset HOME; rm -r ~/.cache/bg-sentinel");
}

/// `export HOME=/etc` rebinds HOME like a plain assignment -> not Allow.
/// DOES NOT PROVE: `export` without a value, or `declare`/`local` forms.
#[test]
fn export_home_rebound_not_allow() {
    not_allow("export", "export HOME=/etc; rm -r $HOME/bg-sentinel");
}

// ---------------------------------------------------------------------------
// Restrictive branches kept by S3.
// ---------------------------------------------------------------------------

/// Table A.3: `~user` is not covered by ruling 12 -> value Undetermined ->
/// Unresolved -> not Allow.
/// DOES NOT PROVE: `~+` / `~-` forms.
#[test]
fn tilde_user_not_allow() {
    not_allow("tuser", "rm -r ~root/bg-sentinel");
}

/// A line containing arithmetic (`$(( ))`) keeps the restrictive verdict even
/// for an otherwise Allow tilde target (coordinator's stated implementation
/// scope; SUBSET.md does not itself require it).
/// DOES NOT PROVE: that a correct arithmetic model would refuse it.
#[test]
fn arithmetic_line_keeps_restrictive() {
    not_allow("arith", "echo $(( ~1 )); rm -r ~/.cache/bg-sentinel");
}

/// HOME that is not a plain path (contains a space). Ruling 12 literally
/// makes `~` Known = this HOME, which would place the target in the cache
/// root (Allow); the coordinator asked for the restrictive expectation
/// (an unsafe-char HOME is not substituted). The HOME directory exists in
/// the fixture, so a correct non-splitting expansion would be a cache-root
/// target: this test distinguishes "not substituted" from "substituted".
/// DOES NOT PROVE: that SUBSET.md requires this refusal (it is silent on
/// HOME values with special characters); other unsafe chars (`$`, quotes,
/// newline).
#[test]
fn unsafe_char_home_tilde_not_allow() {
    let fx = fixture_with_home_name("unsafe", "a b");
    not_allow_at(&fx, &fx.proj(), "rm -r ~/.cache/bg-sentinel");
}

// ---------------------------------------------------------------------------
// Quoted `~` is a literal directory named `~` under cwd.
// ---------------------------------------------------------------------------

/// A quoted `~` is not a TildeExpansion: the operand is the relative path
/// `<cwd>/~/.cache/bg-sentinel`. With cwd = the own worktree, Table C
/// OwnWorktree x Delete(rec) is Allow / Allow (rulings 5, 9). Asserted per
/// SPEC, not per observation.
/// DOES NOT PROVE: that the quoted `~` was not expanded to HOME (a HOME
/// expansion to the cache root would also Allow here; see the cwd-outside
/// twin below for the discriminating case).
///
/// Observed 2026-11-18 at 1c6651da4: main=deny subagent=deny ("recursive rm
/// (-r) can delete an entire directory tree"). The isolating REDs below show
/// the cause is not specific to `~`: a QUOTED relative operand
/// (`"sub/bg-sentinel"`) is also Deny in the own worktree while the same
/// operand unquoted is Allow.
// test-weakening-justified: new RED
#[test]
#[ignore = "RED: S3 Table C OwnWorktree x Delete(rec) - quoted relative operand (quoted ~) is Deny, spec Allow"]
fn quoted_tilde_is_relative_in_own_worktree_allows() {
    let fx = fixture("qtilde");
    let wt = fx.own_worktree();
    allow_at(&fx, &wt, "rm -r \"~/.cache/bg-sentinel\"");
}

/// Isolating RED: a quoted relative operand without any `~`, cwd = own
/// worktree. Table C OwnWorktree x Delete(rec) = Allow / Allow; quoting does
/// not change the path (Table A: a quoted literal is a literal).
/// Observed 2026-11-18 at 1c6651da4: main=deny subagent=deny, while the
/// unquoted `rm -r sub/bg-sentinel` is main=allow subagent=allow.
/// DOES NOT PROVE: whether this predates S3 (not measured on main).
// test-weakening-justified: new RED
#[test]
#[ignore = "RED: S3 Table C OwnWorktree x Delete(rec) - quoted relative operand is Deny, spec Allow"]
fn quoted_relative_in_own_worktree_allows() {
    let fx = fixture("qrel");
    let wt = fx.own_worktree();
    allow_at(&fx, &wt, "rm -r \"sub/bg-sentinel\"");
}

/// Isolating RED: an unquoted `~` that does NOT start the word
/// (`./~/...`) is not a TildeExpansion in bash (Table A.3 covers only a
/// word-start `~` and `~` after `=` / `:`), so the operand is the literal
/// relative path `<cwd>/~/.cache/bg-sentinel` -> OwnWorktree -> Allow.
/// Observed 2026-11-18 at 1c6651da4: main=deny subagent=deny.
/// DOES NOT PROVE: whether this predates S3 (not measured on main).
// test-weakening-justified: new RED
#[test]
#[ignore = "RED: S3 Table A.3/C - mid-word literal ~ (./~/x) in own worktree is Deny, spec Allow"]
fn midword_literal_tilde_in_own_worktree_allows() {
    let fx = fixture("midtilde");
    let wt = fx.own_worktree();
    allow_at(&fx, &wt, "rm -r ./~/.cache/bg-sentinel");
}

/// Discriminating twin: quoted `~` with cwd = the plain project dir (not a
/// worktree). If the quote were wrongly expanded to the hook HOME the target
/// would be the cache root -> Allow; read literally it is
/// `<proj>/~/.cache/bg-sentinel`, a Project target whose verdict is
/// `legacy` in Table C. The legacy verdict is not pinned here, so this
/// asserts only that the quoted `~` is judged the same as the absolute
/// literal spelling of `<proj>/~/...` (observed 2026-11-18: both Deny).
/// DOES NOT PROVE: what the legacy Project verdict is; and since both
/// spellings could be refused for one unrelated shared reason (e.g. any
/// path containing `~`), it does not prove the quoted `~` was read as a
/// literal — only that it was not expanded to the cache-root HOME.
#[test]
fn quoted_tilde_matches_absolute_literal_spelling() {
    let fx = fixture("qtilde-proj");
    let lit = fx.proj().join("~").join(".cache").join("bg-sentinel");
    for subagent in [false, true] {
        let (dq, rq) = run_as(&fx, &fx.proj(), "rm -r \"~/.cache/bg-sentinel\"", subagent);
        let (da, ra) = run_as(
            &fx,
            &fx.proj(),
            &format!("rm -r '{}'", lit.display()),
            subagent,
        );
        assert_eq!(
            dq, da,
            "subagent={subagent}: quoted ~ ({dq}: {rq}) differs from its absolute literal spelling ({da}: {ra})"
        );
    }
}

/// Probe (not an assertion): prints the observed decisions of every command
/// in this file, both modes. Kept ignored; it always panics with the table.
/// DOES NOT PROVE: anything; it records observations for the report.
#[test]
#[ignore = "probe: observed decisions table"]
fn probe_observed_decisions() {
    let fx = fixture("probe");
    let wt = fx.own_worktree();
    let ufx = fixture_with_home_name("probe-unsafe", "a b");
    let abs = fx.home.join(".cache").join("bg-sentinel");
    let lit = fx.proj().join("~").join(".cache").join("bg-sentinel");
    let rows: Vec<(&Fx, PathBuf, String)> = vec![
        (&fx, fx.proj(), format!("rm -r {}", abs.display())),
        (&fx, fx.proj(), "rm -r ~/.cache/bg-sentinel".into()),
        (&fx, fx.proj(), "rm -r $HOME/.cache/bg-sentinel".into()),
        (&fx, fx.proj(), "rm -r ${HOME}/.cache/bg-sentinel".into()),
        (&fx, fx.proj(), "HOME=/etc; rm -r $HOME/bg-sentinel".into()),
        (
            &fx,
            fx.proj(),
            "unset HOME; rm -r ~/.cache/bg-sentinel".into(),
        ),
        (
            &fx,
            fx.proj(),
            "export HOME=/etc; rm -r $HOME/bg-sentinel".into(),
        ),
        (&fx, fx.proj(), "rm -r ~root/bg-sentinel".into()),
        (
            &fx,
            fx.proj(),
            "echo $(( ~1 )); rm -r ~/.cache/bg-sentinel".into(),
        ),
        (&ufx, ufx.proj(), "rm -r ~/.cache/bg-sentinel".into()),
        (&fx, wt.clone(), "rm -r \"~/.cache/bg-sentinel\"".into()),
        (&fx, wt.clone(), "rm -r \"sub/bg-sentinel\"".into()),
        (&fx, wt.clone(), "rm -r sub/bg-sentinel".into()),
        (&fx, wt.clone(), "rm -r ./~/.cache/bg-sentinel".into()),
        (&fx, fx.proj(), "rm -r \"~/.cache/bg-sentinel\"".into()),
        (&fx, fx.proj(), format!("rm -r '{}'", lit.display())),
    ];
    let mut table = String::new();
    for (f, cwd, c) in &rows {
        let (dm, _) = run_as(f, cwd, c, false);
        let (ds, rs) = run_as(f, cwd, c, true);
        table.push_str(&format!(
            "{c:?} @ {} => main={dm} sub={ds} ({rs})\n",
            cwd.display()
        ));
    }
    panic!("PROBE\n{table}");
}
