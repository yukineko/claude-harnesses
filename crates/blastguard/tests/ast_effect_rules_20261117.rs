#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! S2 `red-tests` for the AST EFFECT x TARGET judgement (blastguard
//! `src/detect/ast/SUBSET.md`, rulings 1-13, 2026-11-17).
//!
//! Written by an independent test author (not the S3 implementer; CLAUDE.md
//! §2(a)). Every expectation below is derived from SUBSET.md only (Tables A,
//! B, C and the numbered rulings), never from today's behaviour. Tests that
//! fail against the S1 stub carry `#[ignore = "RED: S<n> <row>"]`; tests that
//! already pass stay un-ignored as controls.
//!
//! Content rule (user ruling 2026-11-17): no real destructive payloads. Every
//! target is a non-existent sentinel path (`/etc/bg-sentinel-*`,
//! `/tmp/bg-s2-sentinel-*`, `$HOME/.cache/bg-sentinel`) or a benign file; the
//! hook only JUDGES the command text, nothing is executed.
//!
//! Harness: the hook binary is run with a hermetic environment (`env_clear`,
//! `HOME=<fixture>/home`, `CLAUDE_PROJECT_DIR` = payload cwd,
//! `BLASTGUARD_APPROVALS_DIR=<fixture>/store`, `CLAUDECODE=1`,
//! `CLAUDE_CODE_ENTRYPOINT=cli`, no `TMPDIR`) in main mode and in subagent
//! mode (`agent_id` in the payload). The fixture base is
//! `support/neutral_base.rs`, so the fixture LOCATION cannot decide a verdict
//! (a base under `$TMPDIR` = `/private/var/folders` is a system directory for
//! deletion, measured in that module).
//!
//! Assertion vocabulary:
//!   * `allow`   — both modes Allow.
//!   * `restrict` (an Ask cell, or Undetermined) — main is NOT Allow (Ask or
//!     stricter) and subagent is Deny (Table C: subagent column = main column
//!     with Ask -> Deny).
//!   * `deny`    — both modes Deny (System cells, ruling 3).
//!
//! Cells marked `legacy` in Table C (OtherWorktree, Project, Other, Protected
//! git cells, OwnWorktree x GitDiscard) and the deferred Open questions
//! (`find -exec`, sed `w`/`e` scripts) are NOT tested here: S3 must reproduce
//! today's verdict there, which is pinned by the existing tests.
//!
//! Backlog 06c51772 note: that item reports `. "$HOME/.cargo/env"` being
//! refused as a false positive. Rulings 13 / 16: `.` / `source` become Ask
//! (main) / Deny (subagent) together with the quarantine plugin; from then
//! on refusing the `.` itself is the SPECIFIED verdict, not an FP. Until the
//! plugin exists the verdict is today's (legacy). The python-heredoc
//! write-path half of 06c51772 is S5 (ruling 15).
//! `b3_dot_source_home_cargo_env_restricts` pins the ruling-13/16 half.
//!
//! DOES NOT PROVE: which rule produced a verdict (only the decision is
//! asserted, not the reason), behaviour with `TMPDIR` set, any `legacy`
//! cell, harmful payloads, or that S3/S4/S5 implement Table A enforcement
//! beyond the shapes listed.
//!
//! Not written (spec gap, not a skip for content): `touch /etc/bg-sentinel`
//! via `xargs` / `find`. `touch` has no Table B row, so SUBSET.md answers
//! Allow for it (no effect model); pinning that Allow on a System target
//! would freeze a possible fail-open, so it is left to a ruling. The `xargs`
//! / `find -delete` shapes are tested with `rm` / `-delete` instead.
//!
//! Stage labels on the `dd` / `truncate` / `find -delete` Scratch twins are
//! S6: ruling 14 keeps the whole legacy string layer raise-only until S6, and
//! today's legacy answer there is Deny / Ask. The `.` / `source` REDs are
//! labelled `quarantine` (ruling 16: they become Ask together with the
//! quarantine plugin, not in S4).

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
    /// `$HOME/.condukt/worktrees` storage root: a real dir with a `.git` FILE
    /// starting `gitdir:` (what `git worktree add` writes).
    fn own_worktree(&self) -> PathBuf {
        let wt = self
            .home()
            .join(".condukt")
            .join("worktrees")
            .join("bg-s2-wt");
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        std::fs::write(
            wt.join(".git"),
            "gitdir: /nonexistent/bg-s2/.git/worktrees/bg-s2-wt\n",
        )
        .unwrap();
        wt
    }
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "ast-effect-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home").join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("home").join("Library").join("Caches")).unwrap();
    std::fs::create_dir_all(base.join("src").join("proj")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    Fx { base }
}

/// Run the hook once; `(decision, reason)`.
///
/// Order is load-bearing: (1) a non-zero / signalled exit panics; (2) only
/// then is an EMPTY stdout read as Allow (a crashed hook never becomes an
/// Allow); (3) a non-empty stdout that is not JSON panics.
fn run_as(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "ast-effect-rules-20261117",
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
    // (1) Exit status first: everything below runs only for exit 0.
    assert!(
        out.status.success(),
        "hook exited {:?} for {command:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    // (2) Empty stdout = Allow, reachable only after (1).
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    // (3) Non-JSON stdout panics.
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

fn deny_at(fx: &Fx, cwd: &Path, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, cwd, command, subagent);
        assert_eq!(
            d, "deny",
            "subagent={subagent}: {command:?} expected Deny, got {d} ({r})"
        );
    }
}

fn allow(tag: &str, command: &str) {
    let fx = fixture(tag);
    allow_at(&fx, &fx.proj(), command);
}

fn restrict(tag: &str, command: &str) {
    let fx = fixture(tag);
    restrict_at(&fx, &fx.proj(), command);
}

fn deny(tag: &str, command: &str) {
    let fx = fixture(tag);
    deny_at(&fx, &fx.proj(), command);
}

// ---------------------------------------------------------------------------
// B1 — Scratch / OwnWorktree delete -> Allow (rulings 5, 8, 9; Table C
// Scratch and OwnWorktree rows, Delete column).
// ---------------------------------------------------------------------------

/// Table C Scratch x Delete(rec): `/tmp` is a temp deletion root (ruling 8).
#[test]
fn b1_rm_r_under_tmp_allows() {
    allow("b1tmp", "rm -r /tmp/bg-s2-sentinel-b1/dir");
}

/// Table C Scratch x Delete(rec): `/private/tmp` is a temp deletion root.
#[test]
fn b1_rm_r_under_private_tmp_allows() {
    allow("b1ptmp", "rm -r /private/tmp/bg-s2-sentinel-b1/dir");
}

/// Table C Scratch x Delete(rec): `$HOME/.cache` (hermetic HOME, absolute
/// spelling) is a cache deletion root (ruling 8).
#[test]
fn b1_rm_r_under_cache_root_allows() {
    let fx = fixture("b1cache");
    let target = fx.home().join(".cache").join("bg-sentinel");
    allow_at(&fx, &fx.proj(), &format!("rm -r {}", target.display()));
}

/// Table C Scratch x Delete(rec): `$HOME/Library/Caches` is a cache root.
#[test]
fn b1_rm_r_under_library_caches_allows() {
    let fx = fixture("b1libc");
    let target = fx.home().join("Library").join("Caches").join("bg-sentinel");
    allow_at(&fx, &fx.proj(), &format!("rm -r {}", target.display()));
}

/// Table C OwnWorktree x Delete(rec) (ruling 9): absolute target inside the
/// checkout that contains the session cwd.
#[test]
fn b1_rm_r_inside_own_worktree_absolute_allows() {
    let fx = fixture("b1wtabs");
    let wt = fx.own_worktree();
    allow_at(&fx, &wt, &format!("rm -r {}", wt.join("sub").display()));
}

/// Table C OwnWorktree x Delete(rec): relative target placed against the hook
/// cwd (Table B common rules), cwd = the own worktree.
#[test]
fn b1_rm_r_inside_own_worktree_relative_allows() {
    let fx = fixture("b1wtrel");
    let wt = fx.own_worktree();
    allow_at(&fx, &wt, "rm -r sub");
}

/// OwnWorktree control: the SAME `rm -r <checkout>/sub` with the session cwd
/// OUTSIDE the checkout (the fixture's plain `src/proj`). Ruling 9 makes the
/// target OtherWorktree ("today's verdict (confined Ask)"), while the
/// `scope.rs` module doc says recursive rm strictly inside a worktree storage
/// root is Allow today. Those two disagree, so per the coordinator's
/// instruction (2026-11-17) this is NOT asserted: it is a probe that panics
/// with the observed decisions, kept ignored. Observed 2026-11-17:
/// `main=allow () | subagent=allow ()` — today's verdict is the scope.rs
/// storage-root Allow, not ruling 9's parenthetical "confined Ask".
#[test]
#[ignore = "probe: OtherWorktree rm -r, ruling 9 vs scope.rs storage-root Allow (undecided)"]
fn b1_probe_rm_r_own_worktree_target_with_cwd_outside() {
    let fx = fixture("b1wtout");
    let wt = fx.own_worktree();
    let command = format!("rm -r {}", wt.join("sub").display());
    let (dm, rm) = run_as(&fx, &fx.proj(), &command, false);
    let (ds, rs) = run_as(&fx, &fx.proj(), &command, true);
    panic!("PROBE main={dm} ({rm}) | subagent={ds} ({rs})");
}

// ---------------------------------------------------------------------------
// B2 — perl / ruby / node inline programs are OUT of the subset (ruling 4;
// Table B.5 row "perl / ruby -e/-E, -i, stdin, node -e/-p/stdin") ->
// Undetermined -> main Ask, subagent Deny.
// ---------------------------------------------------------------------------

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 perl -e out of subset (ruling 4)"]
fn b2_perl_inline_restricts() {
    restrict("b2perl", "perl -e 'print 1'");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 ruby -e out of subset (ruling 4)"]
fn b2_ruby_inline_restricts() {
    restrict("b2ruby", "ruby -e 'puts 1'");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 node -e out of subset (ruling 4)"]
fn b2_node_inline_restricts() {
    restrict("b2node", "node -e '1'");
}

/// Same row, `node -p`.
// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 node -p out of subset (ruling 4)"]
fn b2_node_print_inline_restricts() {
    restrict("b2nodep", "node -p '1'");
}

/// Same row, `perl -E`.
// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 perl -E out of subset (ruling 4)"]
fn b2_perl_big_e_inline_restricts() {
    restrict("b2perlE", "perl -E 'say 1'");
}

// ---------------------------------------------------------------------------
// B3 — `$HOME` / `${HOME}` / `~` are Known (ruling 12, Table A.3); `.` /
// `source` of a file stay Ask / Deny (ruling 13, Table B.5).
// ---------------------------------------------------------------------------

/// Ruling 12: `"$HOME/..."` must get the same verdict as its absolute
/// spelling, in both modes (consistency only, no fixed value).
fn same_as_absolute(tag: &str, spelled: &str, rel_under_home: &str) {
    let fx = fixture(tag);
    let abs = format!("rm -r {}", fx.home().join(rel_under_home).display());
    for subagent in [false, true] {
        let (da, ra) = run_as(&fx, &fx.proj(), &abs, subagent);
        let (dv, rv) = run_as(&fx, &fx.proj(), spelled, subagent);
        assert_eq!(
            da, dv,
            "subagent={subagent}: absolute {abs:?} => {da} ({ra}) but {spelled:?} => {dv} ({rv})"
        );
    }
}

// test-weakening-justified: new RED for unbuilt S3 behaviour (2026-11-17)
#[test]
fn b3_dollar_home_matches_absolute() {
    same_as_absolute(
        "b3home",
        "rm -r \"$HOME/.cache/bg-sentinel\"",
        ".cache/bg-sentinel",
    );
}

// test-weakening-justified: new RED for unbuilt S3 behaviour (2026-11-17)
#[test]
fn b3_braced_home_matches_absolute() {
    same_as_absolute(
        "b3bhome",
        "rm -r \"${HOME}/.cache/bg-sentinel\"",
        ".cache/bg-sentinel",
    );
}

// test-weakening-justified: new RED for unbuilt S3 behaviour (2026-11-17)
#[test]
fn b3_tilde_matches_absolute() {
    same_as_absolute(
        "b3tilde",
        "rm -r ~/.cache/bg-sentinel",
        ".cache/bg-sentinel",
    );
}

/// Ruling 12 + Table C Scratch x Delete(rec): the `$HOME` spelling of a
/// cache-root target is Scratch, hence Allow.
// test-weakening-justified: new RED for unbuilt S3 behaviour (2026-11-17)
#[test]
fn b3_dollar_home_cache_delete_allows() {
    allow("b3homeok", "rm -r \"$HOME/.cache/bg-sentinel\"");
}

/// Ruling 13: `.` of a file is Ask (main) / Deny (subagent). See the module
/// doc for the backlog 06c51772 note.
// test-weakening-justified: new RED for unbuilt quarantine-plugin behaviour (2026-11-17)
#[test]
#[ignore = "RED: quarantine B.5 . file Ask together with the quarantine plugin (rulings 13, 16)"]
fn b3_dot_source_home_cargo_env_restricts() {
    restrict("b3dot", ". \"$HOME/.cargo/env\"");
}

/// Ruling 13: `source` of a relative file is Ask / Deny.
// test-weakening-justified: new RED for unbuilt quarantine-plugin behaviour (2026-11-17)
#[test]
#[ignore = "RED: quarantine B.5 source file Ask together with the quarantine plugin (rulings 13, 16)"]
fn b3_source_relative_restricts() {
    restrict("b3src", "source ./x.sh");
}

// ---------------------------------------------------------------------------
// B4 — an interpreter running a program file whose body is not in the text is
// Undetermined (ruling 10, Table B.5 script-file row) -> Ask / Deny.
// ---------------------------------------------------------------------------

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 script-file row (ruling 10)"]
fn b4_bash_script_file_restricts() {
    restrict("b4bash", "bash script.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 script-file row sh (ruling 10)"]
fn b4_sh_script_file_restricts() {
    restrict("b4sh", "sh ./script.sh");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 script-file row python3 (ruling 10)"]
fn b4_python3_script_file_restricts() {
    restrict("b4py", "python3 x.py");
}

// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 script-file row perl (ruling 10)"]
fn b4_perl_script_file_restricts() {
    restrict("b4pl", "perl x.pl");
}

/// Table B.5 wrapper row: unwrap (`env`, B.3), then the script-file row.
// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 wrapper -> script-file row (ruling 10)"]
fn b4_env_wrapped_script_file_restricts() {
    restrict("b4env", "env bash script.sh");
}

/// Ruling 17 (Q2): direct execution of a file whose body is not in the text
/// (`./run.sh`) is Undetermined -> Ask / Deny.
// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 direct-execution row ./ (ruling 17)"]
fn b4_direct_exec_dot_slash_restricts() {
    restrict("b4dot", "./run.sh");
}

/// Ruling 17: a relative path with a directory component.
// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 direct-execution row relative dir (ruling 17)"]
fn b4_direct_exec_relative_dir_restricts() {
    restrict("b4rel", "scripts/foo.sh");
}

/// Ruling 17: an absolute path (non-existent sentinel under `/tmp`).
// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 direct-execution row absolute (ruling 17)"]
fn b4_direct_exec_absolute_restricts() {
    restrict("b4abs", "/tmp/bg-s2-sentinel-exec/run.sh");
}

/// Ruling 17 + B.3: a wrapper in front of a direct execution.
// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 wrapper -> direct-execution row (ruling 17)"]
fn b4_direct_exec_env_wrapped_restricts() {
    restrict("b4envd", "env ./run.sh");
}

// ---------------------------------------------------------------------------
// B5 — Table B.1 / B.2 effect rows x Table C System (Deny, ruling 3) and
// Scratch (Allow, rulings 5/8) on non-existent sentinel paths; Unresolved
// targets -> Ask / Deny.
// ---------------------------------------------------------------------------

/// B.1 `rm` (non-rec Delete) x System.
#[test]
fn b5_rm_system_denies() {
    deny("b5rmsys", "rm /etc/bg-sentinel-rm");
}

/// B.1 `rm` (non-rec Delete) x Scratch.
#[test]
fn b5_rm_scratch_allows() {
    allow("b5rmscr", "rm /tmp/bg-s2-sentinel-rm");
}

/// B.1 `mv` destination (Truncate) x System.
#[test]
fn b5_mv_dst_system_denies() {
    deny(
        "b5mvdsys",
        "mv /tmp/bg-s2-sentinel-mv-a /etc/bg-sentinel-mv",
    );
}

/// B.1 `mv` source (Delete rec) x System.
#[test]
fn b5_mv_src_system_denies() {
    deny(
        "b5mvssys",
        "mv /etc/bg-sentinel-mvsrc /tmp/bg-s2-sentinel-mv-b",
    );
}

/// B.1 `mv` source and destination x Scratch.
#[test]
fn b5_mv_scratch_allows() {
    allow(
        "b5mvscr",
        "mv /tmp/bg-s2-sentinel-mv-a /tmp/bg-s2-sentinel-mv-b",
    );
}

/// B.1 `cp` destination (Truncate) x System.
#[test]
fn b5_cp_dst_system_denies() {
    deny("b5cpsys", "cp /tmp/bg-s2-sentinel-cp-a /etc/bg-sentinel-cp");
}

/// B.1 `cp` destination (Truncate) x Scratch.
#[test]
fn b5_cp_dst_scratch_allows() {
    allow(
        "b5cpscr",
        "cp /tmp/bg-s2-sentinel-cp-a /tmp/bg-s2-sentinel-cp-b",
    );
}

/// B.1 `chmod` (Metadata) x System.
#[test]
fn b5_chmod_system_denies() {
    deny("b5chmsys", "chmod 644 /etc/bg-sentinel-chmod");
}

/// B.1 `chmod` (Metadata) x Scratch.
#[test]
fn b5_chmod_scratch_allows() {
    allow("b5chmscr", "chmod 644 /tmp/bg-s2-sentinel-chmod");
}

/// B.1 `chown` (Metadata) x System.
#[test]
fn b5_chown_system_denies() {
    deny("b5chosys", "chown nobody /etc/bg-sentinel-chown");
}

/// B.1 `chown` (Metadata) x Scratch.
#[test]
fn b5_chown_scratch_allows() {
    allow("b5choscr", "chown nobody /tmp/bg-s2-sentinel-chown");
}

/// B.1 `sed -i` (Rewrite) x System. The `-i.bak` suffix form and `-e` are
/// used so the script operand is unambiguous (B.1 sed row: BSD `-i ''` /
/// `-i.bak`; script operand dropped when `-e` given).
#[test]
fn b5_sed_i_system_denies() {
    deny("b5sedsys", "sed -i.bak -e 's/a/b/' /etc/bg-sentinel-sed");
}

/// B.1 `sed -i` (Rewrite) x Scratch.
#[test]
fn b5_sed_i_scratch_allows() {
    allow("b5sedscr", "sed -i.bak -e 's/a/b/' /tmp/bg-s2-sentinel-sed");
}

/// B.1 `tee` (Truncate) x System.
#[test]
fn b5_tee_system_denies() {
    deny("b5teesys", "echo hi | tee /etc/bg-sentinel-tee");
}

/// B.1 `tee` (Truncate) x Scratch.
#[test]
fn b5_tee_scratch_allows() {
    allow("b5teescr", "echo hi | tee /tmp/bg-s2-sentinel-tee");
}

/// B.1 `tee -a` (Append) x System.
#[test]
fn b5_tee_append_system_denies() {
    deny("b5teeasys", "echo hi | tee -a /etc/bg-sentinel-teea");
}

/// B.1 `truncate` (Truncate) x System.
#[test]
fn b5_truncate_system_denies() {
    deny("b5trsys", "truncate -s 0 /etc/bg-sentinel-truncate");
}

/// B.1 `truncate` (Truncate) x Scratch.
// test-weakening-justified: new RED for unbuilt S6 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S6 Table C Scratch x Truncate truncate (legacy confined Ask raise-only until S6, ruling 14)"]
fn b5_truncate_scratch_allows() {
    allow("b5trscr", "truncate -s 0 /tmp/bg-s2-sentinel-truncate");
}

/// B.2 `>` (Truncate) x System.
#[test]
fn b5_redirect_truncate_system_denies() {
    deny("b5gtsys", "echo hi > /etc/bg-sentinel-redir");
}

/// B.2 `>` (Truncate) x Scratch.
#[test]
fn b5_redirect_truncate_scratch_allows() {
    allow("b5gtscr", "echo hi > /tmp/bg-s2-sentinel-redir");
}

/// B.2 `>>` (Append) x System.
#[test]
fn b5_redirect_append_system_denies() {
    deny("b5ggsys", "echo hi >> /etc/bg-sentinel-append");
}

/// B.2 `>>` (Append) x Scratch.
#[test]
fn b5_redirect_append_scratch_allows() {
    allow("b5ggscr", "echo hi >> /tmp/bg-s2-sentinel-append");
}

/// B.1 `dd of=` (Truncate) x System.
#[test]
fn b5_dd_of_system_denies() {
    deny(
        "b5ddsys",
        "dd if=/tmp/bg-s2-sentinel-dd-in of=/etc/bg-sentinel-dd",
    );
}

/// B.1 `dd of=` (Truncate) x Scratch.
// test-weakening-justified: new RED for unbuilt S6 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S6 Table C Scratch x Truncate dd of= (legacy dd reading raise-only until S6, ruling 14)"]
fn b5_dd_of_scratch_allows() {
    allow(
        "b5ddscr",
        "dd if=/tmp/bg-s2-sentinel-dd-in of=/tmp/bg-s2-sentinel-dd",
    );
}

/// Table C Unresolved x Delete: `$x` unbound on the line (A.3 value
/// Undetermined -> B common rule -> Unresolved).
#[test]
fn b5_rm_unresolved_unset_var_restricts() {
    restrict("b5unrm", "rm -r \"$x\"");
}

/// Table C Unresolved x Truncate (redirect), `$x` unbound.
#[test]
fn b5_redirect_unresolved_unset_var_restricts() {
    restrict("b5ungt", "echo hi > \"$x\"");
}

/// Table C Unresolved x Metadata: `$x` from a command substitution (A.3:
/// value Undetermined).
#[test]
fn b5_chmod_unresolved_dynamic_var_restricts() {
    restrict(
        "b5unchm",
        "x=$(cat /tmp/bg-s2-sentinel-list); chmod 644 \"$x\"",
    );
}

/// Table C Unresolved x Truncate (`cp` destination), dynamic `$x`.
#[test]
fn b5_cp_dst_unresolved_dynamic_var_restricts() {
    restrict(
        "b5uncp",
        "x=$(cat /tmp/bg-s2-sentinel-list); cp /tmp/bg-s2-sentinel-cp-a \"$x\"",
    );
}

/// Table C Unresolved x Truncate (`tee`), positional parameter (A.3).
#[test]
fn b5_tee_unresolved_positional_restricts() {
    restrict("b5untee", "echo hi | tee \"$1\"");
}

/// Table C Unresolved x Truncate (`truncate`), dynamic `$x`.
#[test]
fn b5_truncate_unresolved_dynamic_var_restricts() {
    restrict(
        "b5untr",
        "x=$(cat /tmp/bg-s2-sentinel-list); truncate -s 0 \"$x\"",
    );
}

// ---------------------------------------------------------------------------
// B6 — structural fail-open shapes with benign effects / sentinel targets.
// ---------------------------------------------------------------------------

/// B.1 `xargs` with `StdinSource::Literal` (`echo lit |`): the stdin operand
/// is the target; `rm` x System -> Deny.
#[test]
fn b6_xargs_literal_stdin_system_target_denies() {
    deny("b6xlit", "echo /etc/bg-sentinel-xargs | xargs rm");
}

/// B.1 `xargs` with a non-literal producer (`FromCommand`): stdin operands
/// are Unresolved -> Table C Unresolved x Delete -> Ask / Deny.
#[test]
fn b6_xargs_from_command_stdin_restricts() {
    restrict("b6xcmd", "cat /tmp/bg-s2-sentinel-list | xargs rm");
}

/// B.1 `find -delete` on a System search root -> Deny.
#[test]
fn b6_find_delete_system_root_denies() {
    deny("b6fsys", "find /etc/bg-sentinel-find -delete");
}

/// B.1 `find -delete` on a Scratch search root -> Allow.
// test-weakening-justified: new RED for unbuilt S6 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S6 Table C Scratch x Delete find -delete (legacy confined Ask raise-only until S6, ruling 14)"]
fn b6_find_delete_scratch_root_allows() {
    allow("b6fscr", "find /tmp/bg-s2-sentinel-find -delete");
}

/// B.5 wrapper row + shell stdin row: `env` unwrapped, `bash` stdin is a
/// here-string with an undetermined body -> Undetermined -> Ask / Deny.
#[test]
fn b6_env_bash_herestring_dynamic_restricts() {
    restrict("b6envhs", "env bash <<<\"$x\"");
}

/// B.5 shell stdin row, `StdinSource::Fd` (backlog cf0bf4ca shape): Fd ->
/// Undetermined -> Ask / Deny, even though the fd holds a benign body.
// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S4 B.5 shell stdin Fd -> Undetermined"]
fn b6_exec_fd_herestring_bash_dup_restricts() {
    restrict("b6fd", "exec 3<<<'touch /etc/bg-sentinel'; bash <&'3'");
}

/// B.5 python row (Undetermined until S5), backlog c1873b73 shape: a
/// computed attribute lookup on `os`.
// test-weakening-justified: new RED for unbuilt S5 behaviour (2026-11-17)
#[test]
#[ignore = "RED: S5 B.5 python -c Undetermined"]
fn b6_python_dunder_dict_computed_name_restricts() {
    restrict(
        "b6pydict",
        "python3 -c \"import os; os.__dict__['sys'+'tem']('touch /etc/bg-sentinel')\"",
    );
}

/// B.5 python row: `getattr(os, n)()` with a benign attribute name.
#[test]
fn b6_python_getattr_dynamic_restricts() {
    restrict(
        "b6pyga",
        "python3 -c \"import os; n='getcwd'; getattr(os, n)()\"",
    );
}
