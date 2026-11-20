#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! S6-6 (S6 retirement, user ruling 2026-11-20): retire the legacy `tee`
//! reading (`src/detect.rs`, the `"tee" =>` arm: Protected / System target
//! → Deny; a literal `-a`/`--append` token → Allow; every target absent →
//! Allow; otherwise `Decision::deny("tee without -a/--append truncates and
//! overwrites its target file(s)")`) wherever the new layer is Known Allow —
//! `detect::known_allow::known_allow` (S6-3, as S6-4 did for `truncate` and
//! S6-5 for `dd`): the AST ties the command to evaluated top-level commands
//! whose Table B B.1 `tee` targets (every operand; `Truncate`, or `Append`
//! with `-a`; options `-a`/`--append`, `-i`/`--ignore-interrupts`, `-p`) are
//! all `Literal`, `judge::classify` places every one in **Scratch** or
//! **OwnWorktree**, and none names command-running configuration.
//!
//! Written by an independent test author (not the S6-6 implementer;
//! CLAUDE.md §2(a)). Harness and fixture copied from
//! `s6_5_dd_retire_20261120.rs` (fixture adds `{H}/.bashrc`,
//! `{T}/sub/g.txt`).
//!
//! * REDs (`#[ignore = "RED S6-6: .."]`): `tee` shapes with Literal
//!   Scratch / OwnWorktree targets that were MEASURED non-Allow at main
//!   5b2b3a6b8 (2026-11-20) and must become Allow in both modes. The
//!   measured verdict is in each ignore reason.
//! * Target behaviour without an ignore (`already_allow`): Scratch /
//!   OwnWorktree shapes ALREADY Allow at 5b2b3a6b8 (append mode, absent
//!   targets, a final-component glob under a literal Scratch / OwnWorktree
//!   dir — literal-dir glob ruling 2026-11-19) — pinned so S6-6 keeps them.
//! * Controls (not ignored): S6 only retires — System, `$HOME` assets,
//!   Protected, `.claude/settings*.json` in the own worktree, Project /
//!   Other / OtherWorktree, the temp root itself, `..`, non-Literal targets,
//!   a mix with a non-Allow target, and an unlisted option keep the verdict
//!   MEASURED at the same base. A control is a characterization pin, not an
//!   endorsement.
//! * Own-worktree config lift (user ruling 2026-11-20, superseding the
//!   earlier same-day ruling "own-worktree command-running config appends →
//!   Ask"): `tee` / `tee -a` / `>>` onto `.githooks/*`, `hooks/hooks.json`,
//!   `crates/*/hooks/*`, `scripts/check-*.py` in the OWN worktree is Allow in
//!   both modes (the 2026-10-03 linked-worktree lift wins):
//!   `own_worktree_config_lift_is_allow` and the `*_is_allow` tests at the
//!   end of file. `.claude/settings*.json` stays Ask / Deny
//!   (`control_protected`).
//! * Fail-open REDs (end of file): the legacy arm probes a final-component
//!   glob word (`{P}/*.txt`) as ONE literal path, which does not exist, so
//!   `NothingToDestroy` makes the truncating write Allow — although the
//!   shell hands `tee` the existing files the glob matches. Each asserts the
//!   glob gets the verdict of its expanded spelling, measured side by side.
//!   Not part of the retirement.
//!   `-a` / `>>` onto ordinary OtherWorktree / Project / HOME files stays
//!   Allow (pinned in `control_placement`).
//!
//! Placeholders: `{T}` dir strictly inside the fixed temp root, `{D}` the
//! hook's `TMPDIR`, `{H}` fixture `HOME`, `{W}` own worktree (session cwd),
//! `{O}` other worktree, `{P}` project main tree.
//! Nothing is executed; the hook only judges the text.
//!
//! DOES NOT PROVE: that the retirement goes through `known_allow` (only the
//! decision is asserted); verdicts with the session cwd in the main tree or
//! with a session id; symlinked targets; that a shape not listed here keeps
//! its verdict; the Ask/Deny REASON text of controls; `tee` reached through
//! a wrapper (`sudo`, `xargs`, `sh -c`) or a heredoc.

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
    let base = neutral_base::neutral_base().join(format!("s6-6-tee-{tag}-{n}"));
    let tmp = fixed_tmp::canonical_fixed_temp_root()
        .join(format!("bg-s6-6-tee-{tag}-{}-{n}", std::process::id()));
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
    write(&fx.tmp.join("sub").join("g.txt"), "a\n");
    write(&fx.tmp.join("0").join("f.txt"), "a\n");
    write(&fx.tmp.join("1").join("f.txt"), "a\n");
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
    write(&fx.home().join(".bashrc"), "x\n");
    write(&fx.home().join("docs").join("f.txt"), "a\n");
    fx
}

/// Run the hook once with the session cwd in `wt-own`; `(decision, reason)`.
/// A non-zero exit panics; only then is an empty stdout read as Allow;
/// non-JSON stdout panics.
fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let cwd = fx.wt("wt-own");
    let mut p = serde_json::json!({
        "session_id": "s6-6-tee-retire-20261120",
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
        "{template:?} (Literal Scratch / OwnWorktree tee target) must be Allow in both \
         modes after S6-6\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

// ---------------------------------------------------------------------------
// REDs: Literal Scratch targets (truncating mode, existing files).
// ---------------------------------------------------------------------------

#[test]
fn red_scratch_printf_pipe() {
    expect_allow("r-printf", "printf x | tee {T}/sub/f.txt");
}

#[test]
fn red_scratch_no_pipe() {
    expect_allow("r-nopipe", "tee {T}/sub/f.txt");
}

#[test]
fn red_scratch_two_targets() {
    expect_allow("r-two", "echo x | tee {T}/sub/f.txt {T}/sub/g.txt");
}

#[test]
fn red_scratch_ignore_interrupts_short() {
    expect_allow("r-i", "echo x | tee -i {T}/sub/f.txt");
}

#[test]
fn red_scratch_ignore_interrupts_long() {
    expect_allow("r-ii", "echo x | tee --ignore-interrupts {T}/sub/f.txt");
}

#[test]
fn red_scratch_p() {
    expect_allow("r-p", "echo x | tee -p {T}/sub/f.txt");
}

/// Bundled `-ai`: append mode, but the legacy arm only recognises the exact
/// token `-a`, so it reads this as a truncating write.
#[test]
fn red_scratch_bundled_ai() {
    expect_allow("r-ai", "echo x | tee -ai {T}/sub/f.txt");
}

#[test]
fn red_scratch_bundled_ap() {
    expect_allow("r-ap", "echo x | tee -ap {T}/sub/f.txt");
}

#[test]
fn red_scratch_usr_bin_tee() {
    expect_allow("r-usrbin", "echo x | /usr/bin/tee {T}/sub/f.txt");
}

#[test]
fn red_scratch_stdout_to_dev_null() {
    expect_allow("r-devnull", "echo x | tee {T}/sub/f.txt > /dev/null");
}

#[test]
fn red_scratch_double_dash() {
    expect_allow("r-dd", "echo x | tee -- {T}/sub/f.txt");
}

#[test]
fn red_scratch_single_quoted() {
    expect_allow("r-sq", "echo x | tee '{T}/sub/f.txt'");
}

#[test]
fn red_scratch_hook_tmpdir() {
    expect_allow("r-tmpdir", "echo x | tee {D}/sub/f.txt");
}

#[test]
fn red_scratch_home_cache() {
    expect_allow("r-cache", "echo x | tee {H}/.cache/x/f.txt");
}

/// A path component equal to a word elsewhere on the line (`echo 0`) must
/// not break the AST tie.
#[test]
fn red_scratch_path_component_zero_with_echo_zero() {
    expect_allow("r-dir0", "echo 0 | tee {T}/0/f.txt");
}

/// Standing user ruling (blastguard-must-resolve-statically): a same-line
/// literal assignment resolves `$F` to a Literal in Scratch.
#[test]
fn red_scratch_var_assigned_quoted() {
    expect_allow("r-var-q", "F={T}/sub/f.txt; echo y | tee \"$F\"");
}

#[test]
fn red_scratch_var_assigned_unquoted() {
    expect_allow("r-var-u", "F={T}/sub/f.txt; echo y | tee $F");
}

// ---------------------------------------------------------------------------
// REDs: Literal OwnWorktree targets (session cwd = wt-own).
// ---------------------------------------------------------------------------

#[test]
fn red_own_worktree_absolute() {
    expect_allow("r-own-abs", "echo x | tee {W}/u.txt");
}

#[test]
fn red_own_worktree_relative() {
    expect_allow("r-own-rel", "echo x | tee u.txt");
}

#[test]
fn red_own_worktree_relative_dot_subdir() {
    expect_allow("r-own-rel-sub", "echo x | tee ./sub/f.txt");
}

#[test]
fn red_own_worktree_and_scratch() {
    expect_allow("r-own-scratch", "echo x | tee {W}/u.txt {T}/sub/f.txt");
}

// ---------------------------------------------------------------------------
// Target behaviour already Allow at 5b2b3a6b8 (not REDs): S6-6 must keep it.
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
/// Ask for the main session, Deny for a subagent (no one to answer).
const AD: &str = "ask/deny";

#[rustfmt::skip]
const ALREADY_ALLOW: &[Pin] = &[
    ("Scratch, -a, two targets", "echo x | tee -a {T}/sub/f.txt {T}/sub/g.txt", A),
    ("Scratch, --append", "echo x | tee --append {T}/sub/f.txt", A),
    ("Scratch, -a, `1` component next to `echo 1`", "echo 1 | tee -a {T}/1/f.txt", A),
    ("Scratch, -a -i -p", "echo x | tee -a -i -p {T}/sub/f.txt", A),
    ("Scratch, absent target", "echo x | tee {T}/sub/new.txt", A),
    // Literal-dir glob ruling 2026-11-19: placed by the parent's class.
    ("Scratch, final-component glob `*`", "echo x | tee {T}/sub/*.txt", A),
    ("Scratch, final-component glob `f.*`", "echo x | tee {T}/sub/f.*", A),
    ("Scratch, final-component glob `?`", "echo x | tee {T}/sub/?.txt", A),
    ("OwnWorktree, -a, two targets", "echo x | tee -a {W}/u.txt {W}/sub/f.txt", A),
    ("OwnWorktree, absent target", "echo x | tee {W}/sub/new.txt", A),
    ("OwnWorktree, final-component glob", "echo x | tee {W}/sub/*.txt", A),
];

#[test]
fn already_allow() {
    check("already-allow", ALREADY_ALLOW);
}

// ---------------------------------------------------------------------------
// Controls: measured at 5b2b3a6b8, must not move.
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const SYSTEM_HOME: &[Pin] = &[
    ("System /etc/hosts", "echo x | tee /etc/hosts", D),
    ("System /etc/hosts, -a", "echo x | tee -a /etc/hosts", D),
    ("System, absent /etc/sudoers.d file", "echo x | tee /etc/sudoers.d/evil", D),
    ("System /usr/lib", "echo x | tee /usr/lib/libc.so", D),
    ("System, final-component glob", "echo x | tee /etc/*.conf", D),
    ("HOME asset ~/.bashrc (tilde)", "echo x | tee ~/.bashrc", AD),
    ("HOME asset ~/.bashrc (tilde), -a", "echo x | tee -a ~/.bashrc", AD),
    ("HOME asset shell rc", "echo x | tee {H}/.zshrc", AD),
    ("HOME asset ssh key", "echo x | tee {H}/.ssh/id_rsa", D),
    ("HOME asset ssh key, -a", "echo x | tee -a {H}/.ssh/id_rsa", D),
    ("Other (plain HOME dir)", "echo x | tee {H}/docs/f.txt", D),
];

#[test]
fn control_system_home() {
    check("sys-home", SYSTEM_HOME);
}

#[rustfmt::skip]
const PROTECTED: &[Pin] = &[
    ("Protected .git/config, main tree", "echo x | tee {P}/.git/config", AD),
    ("Protected .claude/settings.json, main tree", "echo x | tee {P}/.claude/settings.json", AD),
    ("Protected .claude/settings.json, main tree, -a", "echo x | tee -a {P}/.claude/settings.json", AD),
    ("Protected .git file of own worktree", "echo x | tee {W}/.git", D),
    // Ruling 2026-11-20: `.claude/settings*.json` inside the own worktree
    // stays Ask (main) / Deny (subagent) for every writer. The other
    // command-running config rows moved to `OWN_WORKTREE_CONFIG_LIFT`.
    ("command-running config .claude/settings.json, own worktree", "echo x | tee {W}/.claude/settings.json", AD),
    ("command-running config .claude/settings.json, own worktree, -a", "echo x | tee -a {W}/.claude/settings.json", AD),
    ("command-running config .claude/settings.json, relative, -a", "echo x | tee -a .claude/settings.json", AD),
    ("command-running config .claude/settings.local.json, own worktree", "echo x | tee {W}/.claude/settings.local.json", AD),
    ("OwnWorktree + command-running config", "echo x | tee {W}/u.txt {W}/.claude/settings.json", AD),
];

#[test]
fn control_protected() {
    check("protected", PROTECTED);
}

// User ruling 2026-11-20 (supersedes the earlier same-day "own-worktree
// command-running config appends → Ask"): in the OWN worktree every `tee` /
// `tee -a` (literal or final-component glob) onto `.githooks/*`,
// `hooks/hooks.json`, `crates/*/hooks/*`, `scripts/check-*.py` is Allow in
// both modes — the 2026-10-03 linked-worktree lift wins. These rows were
// pinned ask/deny or deny/deny in `control_protected` before the ruling.
#[rustfmt::skip]
const OWN_WORKTREE_CONFIG_LIFT: &[Pin] = &[
    ("hooks/hooks.json, own worktree", "echo x | tee {W}/hooks/hooks.json", A),
    ("crates/x/hooks/hooks.json, own worktree", "echo x | tee {W}/crates/x/hooks/hooks.json", A),
    (".githooks/pre-commit, own worktree", "echo x | tee {W}/.githooks/pre-commit", A),
    (".githooks/pre-commit, own worktree, -a", "echo x | tee -a {W}/.githooks/pre-commit", A),
    (".githooks/pre-commit, relative", "echo x | tee .githooks/pre-commit", A),
    (".githooks/pre-push, absent", "echo x | tee {W}/.githooks/pre-push", A),
    (".githooks/*, glob", "echo x | tee {W}/.githooks/*", A),
    ("scripts/check-x.py, own worktree", "echo x | tee {W}/scripts/check-x.py", A),
    ("OwnWorktree + .githooks/pre-commit, -a", "echo x | tee -a {W}/u.txt {W}/.githooks/pre-commit", A),
];

#[test]
fn own_worktree_config_lift_is_allow() {
    check("own-cfg-lift", OWN_WORKTREE_CONFIG_LIFT);
}

#[rustfmt::skip]
const PLACEMENT: &[Pin] = &[
    ("temp root itself /tmp", "echo x | tee /tmp", D),
    ("temp root itself /private/tmp", "echo x | tee /private/tmp", D),
    ("other worktree", "echo x | tee {O}/u.txt", D),
    ("project main tree", "echo x | tee {P}/u.txt", D),
    ("Scratch + System", "echo x | tee {T}/sub/f.txt /etc/hosts", D),
    ("Scratch + System, -a", "echo x | tee -a {T}/sub/f.txt /etc/hosts", D),
    ("Scratch + Project", "echo x | tee {T}/sub/f.txt {P}/u.txt", D),
    ("Scratch + Other (HOME dir)", "echo x | tee {T}/sub/f.txt {H}/docs/f.txt", D),
    // Characterization: the legacy append arm is Allow off the safe roots
    // too; S6 only retires, so these must not move either way here.
    ("other worktree, -a", "echo x | tee -a {O}/u.txt", A),
    ("project main tree, -a", "echo x | tee -a {P}/u.txt", A),
    ("Other (plain HOME dir), -a", "echo x | tee -a {H}/docs/f.txt", A),
];

#[test]
fn control_placement() {
    check("placement", PLACEMENT);
}

#[rustfmt::skip]
const DOTDOT: &[Pin] = &[
    ("`..` escaping temp to System", "echo x | tee {T}/sub/../../../etc/hosts", D),
    ("`..` staying inside temp", "echo x | tee {T}/sub/../sub/f.txt", D),
    ("`..` from own worktree to other worktree, absolute", "echo x | tee {W}/../wt-other/u.txt", D),
    ("`..` relative to other worktree", "echo x | tee ../wt-other/u.txt", D),
];

#[test]
fn control_dotdot() {
    check("dotdot", DOTDOT);
}

#[rustfmt::skip]
const NON_LITERAL: &[Pin] = &[
    ("unquoted var, unassigned", "echo x | tee $F", D),
    ("quoted var, unassigned", "echo x | tee \"$F\"", D),
    ("var assigned to a System literal", "F=/etc/hosts; echo y | tee \"$F\"", D),
    ("quoted env expansion", "echo x | tee \"$TMPDIR/sub/f.txt\"", D),
    ("command substitution mktemp", "echo x | tee \"$(mktemp)\"", D),
    ("command substitution echoing a Scratch path", "echo x | tee $(echo {T}/sub/f.txt)", D),
    ("middle-component glob in temp", "echo x | tee {T}/*/f.txt", AD),
];

#[test]
fn control_non_literal() {
    check("nonlit", NON_LITERAL);
}

#[rustfmt::skip]
const UNLISTED_OPTION: &[Pin] = &[
    ("unknown short option", "echo x | tee -x {T}/sub/f.txt", D),
    ("unknown long option with value", "echo x | tee --output-error=warn {T}/sub/f.txt", D),
    ("unknown long option, -a", "echo x | tee -a --output-error {T}/sub/f.txt", AD),
];

#[test]
fn control_unlisted_option() {
    check("unlisted", UNLISTED_OPTION);
}

// ---------------------------------------------------------------------------
// FAIL-OPEN REDs (found while writing S6-6; NOT a retirement): the legacy
// arm's absent-target Allow probes a final-component glob word as one
// literal path (`{P}/*.txt` does not exist as a file), answers
// `NothingToDestroy`, and returns Allow — but the shell expands the glob to
// the existing files before `tee` runs, and truncates them. Measured
// allow/allow at 5b2b3a6b8. Each must reach its expanded spelling's verdict:
// both are measured in the same fixture and compared (the property), and
// the expanded spelling is also held to `floor` (its verdict as measured /
// as the ruling requires) so that both drifting together cannot pass.
// ---------------------------------------------------------------------------

fn expect_same_as_expanded(tag: &str, template: &str, expanded: &str, floor: &str) {
    let fx = fixture(tag);
    let (got, rm, rs) = measure(&fx, template);
    let (exp, em, es) = measure(&fx, expanded);
    assert_eq!(
        got, exp,
        "{template:?} is the same argv as its glob-expanded spelling {expanded:?} and must \
         get its verdict\n  glob main: {rm}\n  glob sub:  {rs}\n  expanded main: {em}\n  \
         expanded sub:  {es}"
    );
    assert_eq!(
        exp, floor,
        "{expanded:?} (the expanded spelling) moved off {floor}\n  main: {em}\n  sub: {es}"
    );
}

#[test]
fn red_failopen_glob_project() {
    expect_same_as_expanded(
        "fo-proj",
        "echo x | tee {P}/*.txt",
        "echo x | tee {P}/t.txt {P}/u.txt",
        D,
    );
}

#[test]
fn red_failopen_glob_other_worktree() {
    expect_same_as_expanded(
        "fo-other",
        "echo x | tee {O}/*.txt",
        "echo x | tee {O}/t.txt {O}/u.txt",
        D,
    );
}

#[test]
fn red_failopen_glob_home_dir() {
    expect_same_as_expanded(
        "fo-home",
        "echo x | tee {H}/docs/*.txt",
        "echo x | tee {H}/docs/f.txt",
        D,
    );
}

/// Own-worktree command-running config: the expanded spelling is Allow under
/// user ruling 2026-11-20 (was deny/deny at 5b2b3a6b8), so the glob is too.
#[test]
fn glob_own_worktree_crate_hooks_same_as_expanded() {
    expect_same_as_expanded(
        "fo-hooks",
        "echo x | tee {W}/crates/x/hooks/*.json",
        "echo x | tee {W}/crates/x/hooks/hooks.json",
        A,
    );
}

/// As above for `scripts/check-*.py`.
#[test]
fn glob_own_worktree_check_script_same_as_expanded() {
    expect_same_as_expanded(
        "fo-check",
        "echo x | tee {W}/scripts/check-*.py",
        "echo x | tee {W}/scripts/check-x.py",
        A,
    );
}

// ---------------------------------------------------------------------------
// RE-ANCHORED (user ruling 2026-11-20, superseding the earlier same-day ruling
// on this file's open question 1 that made these appends Ask/Deny): an append
// (`tee -a` AND shell `>>`) onto `hooks/hooks.json`, `crates/*/hooks/…`,
// `scripts/check-*.py` in the OWN worktree is Allow in both modes — the
// 2026-10-03 linked-worktree lift wins. (They were allow/allow at 5b2b3a6b8,
// then ask/deny-pinned as fail-open REDs.)
// ---------------------------------------------------------------------------

fn expect_own_config_allow(tag: &str, template: &str) {
    let fx = fixture(tag);
    let (got, rm, rs) = measure(&fx, template);
    assert_eq!(
        got, A,
        "{template:?} writes own-worktree command-running config (not .claude/settings*), \
         which user ruling 2026-11-20 makes Allow in both modes\n  main reason: {rm}\n  \
         sub reason:  {rs}"
    );
}

#[test]
fn tee_append_own_hooks_json_is_allow() {
    expect_own_config_allow("fo-a-hooks", "echo x | tee -a {W}/hooks/hooks.json");
}

#[test]
fn tee_append_own_crate_hooks_is_allow() {
    expect_own_config_allow(
        "fo-a-crate",
        "echo x | tee -a {W}/crates/x/hooks/hooks.json",
    );
}

#[test]
fn tee_append_own_check_script_is_allow() {
    expect_own_config_allow("fo-a-check", "echo x | tee -a {W}/scripts/check-x.py");
}

#[test]
fn tee_append_own_mixed_with_check_script_is_allow() {
    expect_own_config_allow(
        "fo-a-mixed",
        "echo x | tee -a {W}/u.txt {W}/scripts/check-x.py",
    );
}

#[test]
fn redirect_append_own_hooks_json_is_allow() {
    expect_own_config_allow("fo-r-hooks", "echo x >> {W}/hooks/hooks.json");
}

#[test]
fn redirect_append_own_crate_hooks_is_allow() {
    expect_own_config_allow("fo-r-crate", "echo x >> {W}/crates/x/hooks/hooks.json");
}

#[test]
fn redirect_append_own_check_script_is_allow() {
    expect_own_config_allow("fo-r-check", "echo x >> {W}/scripts/check-x.py");
}
