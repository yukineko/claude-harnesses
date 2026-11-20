#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! S6-5 (S6 retirement, user ruling 2026-11-20): retire the legacy `dd of=`
//! reading (`src/detect.rs`, the `"dd" =>` arm: any `of=` word is
//! `Decision::deny("dd with of= writes raw bytes over a device/file")`)
//! wherever the new layer is Known Allow — `detect::known_allow::known_allow`
//! (S6-3, as S6-4 did for `truncate`): the AST ties the command to evaluated
//! top-level commands whose Table B B.1 `dd` targets (`of=F` → `F`,
//! `Truncate`, `Rewrite` with `conv=notrunc`; `if=` is a read) are all
//! `Literal`, `judge::classify` places every one in **Scratch** or
//! **OwnWorktree**, and none names command-running configuration.
//!
//! Written by an independent test author (not the S6-5 implementer;
//! CLAUDE.md §2(a)). Harness and fixture copied from
//! `s6_4_truncate_retire_20261120.rs`.
//!
//! * REDs (`#[ignore = "RED S6-5: .."]`): `dd of=` shapes with Literal
//!   Scratch / OwnWorktree targets that were MEASURED non-Allow at main
//!   8d6b3403d (2026-11-20) and must become Allow in both modes. The
//!   measured verdict is in each ignore reason.
//! * Controls (not ignored): S6 only retires — raw disks, System, `$HOME`
//!   assets, Protected, command-running config in the own worktree, Project /
//!   Other / OtherWorktree, the temp root itself, `..`, non-Literal targets,
//!   a mix with a non-Allow target, and `dd` without `of=` keep the verdict
//!   MEASURED at the same base. A control is a characterization pin, not an
//!   endorsement.
//! * Fail-open REDs (`#[ignore = "RED fail-open: .."]`, end of file): a
//!   quoted / escaped `of=` word is Allow today even for raw disks; they
//!   expect the unquoted spelling's verdict. Not part of the retirement.
//! * Not pinned (reported to the caller instead): `of=/dev/null` is
//!   deny/deny today; whether it should stay so is not covered by the S6
//!   rulings.
//!
//! Fixture: identical layout to S6-4 (see that file), plus `{T}/1/f.txt`.
//! Placeholders: `{T}` dir strictly inside the fixed temp root, `{D}` the
//! hook's `TMPDIR`, `{H}` fixture `HOME`, `{W}` own worktree (session cwd),
//! `{O}` other worktree, `{P}` project main tree.
//! Nothing is executed; the hook only judges the text.
//!
//! DOES NOT PROVE: that the retirement goes through `known_allow` (only the
//! decision is asserted); verdicts with the session cwd in the main tree or
//! with a session id; symlinked targets; that a shape not listed here keeps
//! its verdict; the Ask/Deny REASON text of controls.

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
    tmp: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

impl Fx {
    fn home(&self) -> PathBuf {
        self.base.join("home")
    }
    fn repo(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    fn wt(&self, name: &str) -> PathBuf {
        self.base.join("src").join(".harness-worktrees").join(name)
    }
    fn tmpdir(&self) -> PathBuf {
        self.base.join("tmpdir")
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

fn write(p: &Path, body: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("s6-5-dd-{tag}-{n}"));
    let tmp = fixed_tmp::canonical_fixed_temp_root()
        .join(format!("bg-s6-5-dd-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&tmp);
    for d in [
        base.join("home").join(".cache"),
        base.join("src").join("proj"),
        base.join("src").join(".harness-worktrees"),
        base.join("store"),
        base.join("tmpdir"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fx = Fx { base, tmp };
    let repo = fx.repo();
    write(&repo.join("t.txt"), "a\n");
    write(&repo.join(".claude").join("settings.json"), "{}\n");
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    for (name, br) in [("wt-own", "own"), ("wt-other", "other")] {
        let wt = fx.wt(name);
        fx.git(
            &repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", br],
        );
        write(&wt.join("u.txt"), "a\n");
        write(&wt.join("sub").join("f.txt"), "a\n");
    }
    write(&repo.join("u.txt"), "a\n");
    write(&fx.tmp.join("sub").join("f.txt"), "a\n");
    write(&fx.tmp.join("0").join("f.txt"), "a\n");
    write(&fx.tmp.join("1").join("f.txt"), "a\n");
    write(&fx.tmp.join("s0").join("f.txt"), "a\n");
    let own = fx.wt("wt-own");
    write(&own.join(".claude").join("settings.local.json"), "{}\n");
    write(&own.join("hooks").join("hooks.json"), "{}\n");
    write(
        &own.join("crates")
            .join("x")
            .join("hooks")
            .join("hooks.json"),
        "{}\n",
    );
    write(&own.join(".githooks").join("pre-commit"), "#!/bin/sh\n");
    write(&own.join("scripts").join("check-x.py"), "pass\n");
    write(&fx.tmpdir().join("sub").join("f.txt"), "a\n");
    write(&fx.home().join(".cache").join("x").join("f.txt"), "a\n");
    write(&fx.home().join(".ssh").join("id_rsa"), "k\n");
    write(&fx.home().join(".zshrc"), "x\n");
    write(&fx.home().join("docs").join("f.txt"), "a\n");
    fx
}

/// Run the hook once with the session cwd in `wt-own`; `(decision, reason)`.
/// A non-zero exit panics; only then is an empty stdout read as Allow;
/// non-JSON stdout panics.
fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let cwd = fx.wt("wt-own");
    let mut p = serde_json::json!({
        "session_id": "s6-5-dd-retire-20261120",
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
        .env("TMPDIR", fx.tmpdir())
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", fx.repo())
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

fn expand(fx: &Fx, t: &str) -> String {
    t.replace("{T}", fx.tmp.to_str().unwrap())
        .replace("{D}", fx.tmpdir().to_str().unwrap())
        .replace("{H}", fx.home().to_str().unwrap())
        .replace("{W}", fx.wt("wt-own").to_str().unwrap())
        .replace("{O}", fx.wt("wt-other").to_str().unwrap())
        .replace("{P}", fx.repo().to_str().unwrap())
}

/// `"main/subagent"` for the template, plus both reasons.
fn measure(fx: &Fx, template: &str) -> (String, String, String) {
    let c = expand(fx, template);
    let (m, rm) = run_as(fx, &c, false);
    let (s, rs) = run_as(fx, &c, true);
    (format!("{m}/{s}"), rm, rs)
}

/// RED body: the shape must be Allow in both modes.
fn expect_allow(tag: &str, template: &str) {
    let fx = fixture(tag);
    let (got, rm, rs) = measure(&fx, template);
    assert_eq!(
        got, "allow/allow",
        "{template:?} (Literal Scratch / OwnWorktree dd of= target) must be Allow in both \
         modes after S6-5\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

// ---------------------------------------------------------------------------
// REDs: Literal Scratch targets.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_of_only_existing() {
    expect_allow("r-of-only", "dd of={T}/sub/f.txt");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_absent_file() {
    expect_allow(
        "r-absent",
        "dd if=/dev/zero of={T}/sub/new.bin bs=1k count=4",
    );
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_temp_dir_direct_child() {
    expect_allow("r-child", "dd if=/dev/zero of={T}/f.bin bs=512 count=1");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_if_scratch_to_scratch() {
    expect_allow("r-if-scratch", "dd if={T}/sub/f.txt of={T}/sub/g.txt");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_conv_notrunc() {
    expect_allow(
        "r-notrunc",
        "dd if=/dev/zero of={T}/sub/f.txt bs=1 count=1 conv=notrunc",
    );
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_status_none() {
    expect_allow(
        "r-status",
        "dd if=/dev/zero of={T}/sub/f.txt bs=1M count=1 status=none",
    );
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_of_first_then_options() {
    expect_allow("r-of-first", "dd of={T}/sub/f.txt if=/dev/zero count=0");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_bin_dd_spelling() {
    expect_allow(
        "r-bin",
        "/bin/dd if=/dev/zero of={T}/sub/f.txt bs=1 count=0",
    );
}

/// Allow ALREADY at 8d6b3403d — but only because the quoted word misses the
/// legacy arm (see the fail-open REDs at the end), not because it is placed.
/// Pinned as the target behaviour; not a RED.
#[test]
fn scratch_whole_word_single_quoted() {
    expect_allow("r-sq-word", "dd if=/dev/zero 'of={T}/sub/f.txt' count=0");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_value_single_quoted() {
    expect_allow("r-sq-val", "dd if=/dev/zero of='{T}/sub/f.txt' count=0");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_hook_tmpdir() {
    expect_allow("r-tmpdir", "dd if=/dev/zero of={D}/sub/f.txt bs=1 count=0");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_home_cache() {
    expect_allow(
        "r-cache",
        "dd if=/dev/zero of={H}/.cache/x/f.txt bs=1 count=0",
    );
}

/// A path component equal to option values in the same command (`bs=1`,
/// `count=0`) must not break the AST tie (fixture paths are random, so the
/// shape is not otherwise guaranteed).
#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_path_component_zero_with_count_zero() {
    expect_allow("r-dir0", "dd if=/dev/zero of={T}/0/f.txt bs=1 count=0");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_path_component_one_with_bs_one() {
    expect_allow("r-dir1", "dd if=/dev/zero of={T}/1/f.txt bs=1 count=1");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_file_named_one_with_bs_one() {
    expect_allow("r-file1", "dd if=/dev/zero of={T}/sub/1 bs=1 count=1");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_path_component_s0() {
    expect_allow("r-s0", "dd if=/dev/zero of={T}/s0/f.txt bs=1 count=0");
}

/// `if=` is a read (Table B); a System source does not make the write
/// non-Scratch.
#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_if_system_file_is_a_read() {
    expect_allow("r-if-sys", "dd if=/etc/hosts of={T}/sub/f.txt");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_if_project_file_is_a_read() {
    expect_allow("r-if-proj", "dd if={P}/t.txt of={T}/sub/f.txt");
}

/// Literal-dir glob ruling 2026-11-19: a final-component glob under a
/// literal Scratch dir is placed by the parent's class.
#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_scratch_final_component_glob() {
    expect_allow("r-glob", "dd if=/dev/zero of={T}/sub/*.txt count=0");
}

// ---------------------------------------------------------------------------
// REDs: Literal OwnWorktree targets (session cwd = wt-own).
// ---------------------------------------------------------------------------

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_own_worktree_absolute() {
    expect_allow("r-own-abs", "dd if=/dev/zero of={W}/u.txt bs=1 count=0");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_own_worktree_absolute_new_file() {
    expect_allow(
        "r-own-new",
        "dd if=/dev/zero of={W}/sub/new.bin bs=1k count=1",
    );
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_own_worktree_relative() {
    expect_allow("r-own-rel", "dd if=/dev/zero of=u.txt bs=1 count=0");
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_own_worktree_relative_dot_subdir() {
    expect_allow(
        "r-own-rel-sub",
        "dd if=/dev/zero of=./sub/f.txt bs=1 count=0",
    );
}

#[test]
#[ignore = "RED S6-5: deny/deny at 8d6b3403d"]
fn red_own_worktree_copy_from_project() {
    expect_allow(
        "r-own-cp",
        "dd if={P}/t.txt of={W}/sub/f.txt conv=notrunc status=none",
    );
}

// ---------------------------------------------------------------------------
// Controls: measured at 8d6b3403d, must not move.
// ---------------------------------------------------------------------------

/// `(condition, template, "main/subagent")`.
type Pin = (&'static str, &'static str, &'static str);

fn check(table: &str, pins: &[Pin]) {
    let fx = fixture(table);
    let mut bad = Vec::new();
    for (cond, t, exp) in pins {
        let (got, rm, rs) = measure(&fx, t);
        println!("MEASURE [{table}] {cond}: {t:?} => {got}\n    main: {rm}\n    sub:  {rs}");
        if got != *exp {
            bad.push(format!(
                "[{table}] {cond}: {t:?} expected {exp}, got {got}\n      main reason: {rm}\n      sub reason:  {rs}"
            ));
        }
    }
    assert!(!pins.is_empty(), "[{table}] no pin was run");
    assert!(
        bad.is_empty(),
        "[{table}] {} of {} control pins moved (S6 only retires):\n{}",
        bad.len(),
        pins.len(),
        bad.join("\n")
    );
}

const A: &str = "allow/allow";
const D: &str = "deny/deny";

#[rustfmt::skip]
const DEVICES_SYSTEM_HOME: &[Pin] = &[
    ("raw disk /dev/disk2", "dd if=/dev/zero of=/dev/disk2 bs=1m", D),
    ("raw disk /dev/rdisk0", "dd if=/dev/zero of=/dev/rdisk0", D),
    ("raw disk /dev/sda", "dd if=/dev/zero of=/dev/sda bs=1M", D),
    ("raw partition /dev/nvme0n1p1", "dd if=/dev/zero of=/dev/nvme0n1p1", D),
    ("System /etc/hosts", "dd if=/dev/zero of=/etc/hosts", D),
    ("System /usr/lib", "dd if={T}/sub/f.txt of=/usr/lib/libc.so", D),
    ("HOME asset ssh key", "dd if=/dev/zero of={H}/.ssh/id_rsa", D),
    ("HOME asset shell rc", "dd if=/dev/zero of={H}/.zshrc", D),
    ("Other (plain HOME dir)", "dd if=/dev/zero of={H}/docs/f.txt", D),
];

#[test]
fn control_devices_system_home() {
    check("dev-sys-home", DEVICES_SYSTEM_HOME);
}

#[rustfmt::skip]
const PROTECTED: &[Pin] = &[
    ("Protected .git/config, main tree", "dd if=/dev/zero of={P}/.git/config", D),
    ("Protected .git/HEAD, main tree", "dd if=/dev/zero of={P}/.git/HEAD", D),
    ("Protected .claude/settings.json, main tree", "dd if=/dev/zero of={P}/.claude/settings.json", D),
    ("Protected .git file of own worktree", "dd if=/dev/zero of={W}/.git", D),
    // Ruling 2026-11-20: command-running config inside the own worktree
    // keeps today's verdict.
    ("Protected .claude/settings.json, own worktree", "dd if=/dev/zero of={W}/.claude/settings.json", D),
    ("command-running config .claude/settings.local.json, own worktree", "dd if=/dev/zero of={W}/.claude/settings.local.json", D),
    ("command-running config hooks/hooks.json, own worktree", "dd if=/dev/zero of={W}/hooks/hooks.json", D),
    ("command-running config crates/x/hooks/hooks.json, own worktree", "dd if=/dev/zero of={W}/crates/x/hooks/hooks.json", D),
    ("command-running config .githooks/pre-commit, own worktree", "dd if=/dev/zero of={W}/.githooks/pre-commit", D),
    ("command-running config scripts/check-x.py, own worktree", "dd if=/dev/zero of={W}/scripts/check-x.py", D),
    ("command-running config, relative, own worktree", "dd if=/dev/zero of=.githooks/pre-commit", D),
];

#[test]
fn control_protected() {
    check("protected", PROTECTED);
}

#[rustfmt::skip]
const PLACEMENT: &[Pin] = &[
    ("temp root itself", "dd if=/dev/zero of=/tmp", D),
    ("other worktree", "dd if=/dev/zero of={O}/u.txt", D),
    ("project main tree", "dd if=/dev/zero of={P}/u.txt", D),
    ("Scratch + System: last of= wins in dd, both judged", "dd of={T}/sub/f.txt of=/etc/hosts", D),
];

#[test]
fn control_placement() {
    check("placement", PLACEMENT);
}

#[rustfmt::skip]
const DOTDOT: &[Pin] = &[
    ("`..` escaping temp to System", "dd if=/dev/zero of={T}/sub/../../../etc/hosts", D),
    ("`..` staying inside temp", "dd if=/dev/zero of={T}/sub/../sub/f.txt", D),
    ("`..` from own worktree to other worktree, absolute", "dd if=/dev/zero of={W}/../wt-other/u.txt", D),
    ("`..` relative to other worktree", "dd if=/dev/zero of=../wt-other/u.txt", D),
];

#[test]
fn control_dotdot() {
    check("dotdot", DOTDOT);
}

#[rustfmt::skip]
const NON_LITERAL: &[Pin] = &[
    ("unquoted var, assigned to a temp literal", "F={T}/sub/f.txt; dd if=/dev/zero of=$F", D),
    ("unquoted var, unassigned", "dd if=/dev/zero of=$F", D),
    ("quoted env expansion", "dd if=/dev/zero of=\"$TMPDIR/sub/f.txt\"", D),
    ("unquoted env expansion", "dd if=/dev/zero of=$TMPDIR/sub/f.txt", D),
    ("middle-component glob in temp", "dd if=/dev/zero of={T}/*/f.txt", D),
    ("non key=value operand", "dd if=/dev/zero of={T}/sub/f.txt junk", D),
];

#[test]
fn control_non_literal() {
    check("nonlit", NON_LITERAL);
}

#[rustfmt::skip]
const NO_OF: &[Pin] = &[
    ("dd to stdout from scratch", "dd if={T}/sub/f.txt", A),
    ("dd to stdout with options", "dd if=/dev/zero bs=1 count=1 status=none", A),
    ("dd reading a raw disk to stdout", "dd if=/dev/disk0 bs=512 count=1", A),
];

#[test]
fn control_no_of() {
    check("no-of", NO_OF);
}

// ---------------------------------------------------------------------------
// FAIL-OPEN REDs (found while writing S6-5; NOT a retirement): the legacy
// arm tests `t.starts_with("of=")` on the RAW token, so a quoted or
// backslash-escaped `of=` word (`'of=/dev/sda'`, `"of=..."`, `\of=...`) —
// which the shell hands to dd as exactly `of=/dev/sda` — misses the arm and
// falls to Allow. Measured allow/allow at 8d6b3403d; the unquoted spelling
// of each is deny/deny (controls above). Each must reach the unquoted
// spelling's verdict. The `.ssh` case is caught by another layer and is a
// control.
// ---------------------------------------------------------------------------

fn expect_same_as_unquoted(tag: &str, template: &str, exp: &str) {
    let fx = fixture(tag);
    let (got, rm, rs) = measure(&fx, template);
    assert_eq!(
        got, exp,
        "{template:?} is the same argv as its unquoted spelling and must get its verdict\n  \
         main reason: {rm}\n  sub reason:  {rs}"
    );
}

#[test]
#[ignore = "RED fail-open: allow/allow at 8d6b3403d (quoted of= misses legacy dd arm)"]
fn red_failopen_single_quoted_of_raw_disk() {
    expect_same_as_unquoted("fo-sda", "dd if=/dev/zero 'of=/dev/sda'", D);
}

#[test]
#[ignore = "RED fail-open: allow/allow at 8d6b3403d (quoted of= misses legacy dd arm)"]
fn red_failopen_double_quoted_of_raw_disk() {
    expect_same_as_unquoted("fo-disk2", "dd if=/dev/zero \"of=/dev/disk2\"", D);
}

#[test]
#[ignore = "RED fail-open: allow/allow at 8d6b3403d (escaped of= misses legacy dd arm)"]
fn red_failopen_backslash_escaped_of_raw_disk() {
    expect_same_as_unquoted("fo-bs", "dd if=/dev/zero \\of=/dev/sda", D);
}

#[test]
#[ignore = "RED fail-open: allow/allow at 8d6b3403d (quoted of= misses legacy dd arm)"]
fn red_failopen_single_quoted_of_system() {
    expect_same_as_unquoted("fo-etc", "dd if=/dev/zero 'of=/etc/hosts'", D);
}

#[test]
#[ignore = "RED fail-open: allow/allow at 8d6b3403d (quoted of= misses legacy dd arm)"]
fn red_failopen_single_quoted_of_own_worktree_settings() {
    expect_same_as_unquoted(
        "fo-settings",
        "dd if=/dev/zero 'of={W}/.claude/settings.json'",
        D,
    );
}

#[test]
#[ignore = "RED fail-open: allow/allow at 8d6b3403d (quoted of= misses legacy dd arm)"]
fn red_failopen_single_quoted_of_project() {
    expect_same_as_unquoted("fo-proj", "dd if=/dev/zero 'of={P}/u.txt'", D);
}

#[rustfmt::skip]
const QUOTED_CAUGHT_ELSEWHERE: &[Pin] = &[
    ("single-quoted of= .ssh key (caught by the Q3 deny-target layer)", "dd if=/dev/zero 'of={H}/.ssh/id_rsa'", D),
];

#[test]
fn control_quoted_caught_elsewhere() {
    check("quoted-ssh", QUOTED_CAUGHT_ELSEWHERE);
}
