#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! S6-4 (user ruling 2026-11-20): retire the legacy `truncate` reading
//! (`src/detect.rs`, the `"truncate" => shape_deny_or_confined_ask(..)` arm)
//! wherever the new layer is Known Allow — `detect::known_allow::known_allow`
//! (S6-3): the AST ties the command to evaluated top-level commands whose
//! Table B B.1 `truncate` targets are all `Literal` and `judge::classify`
//! places every one in **Scratch** or **OwnWorktree**. The precedent is `rm`
//! (`src/detect/rm/new_layer.rs`).
//!
//! Written by an independent test author (not the S6-4 implementer;
//! CLAUDE.md §2(a)).
//!
//! * REDs (`#[ignore = "RED S6-4: .."]`): truncate shapes with Literal
//!   Scratch / OwnWorktree targets that were MEASURED non-Allow (Ask / Deny)
//!   at main 342da07b8 (2026-11-20) and must become Allow in both modes.
//!   Each one's measured verdict is in its ignore reason.
//! * Controls (not ignored): the ruling says S6 only retires — System,
//!   Protected, `$HOME` assets, Project / Other / OtherWorktree, the temp root
//!   itself, `..`, unquoted expansions, a middle-component glob, non-Literal
//!   targets, an unlisted option, and a mix with a non-Allow target keep the
//!   verdict MEASURED at the same base (the command-running-config siblings
//!   and the middle-component glob: measured at main 9f6cb2fea). A control is
//!   a characterization pin, not an endorsement.
//! * Target behaviour without an ignore (added 2026-11-20, after the
//!   implementer removed the ignores): a `0` path component next to `-s 0`
//!   (the tie defect), and a final-component glob under a literal Scratch dir
//!   (literal-dir glob ruling 2026-11-19) — both Allow.
//!
//! Fixture (hermetic, `support/neutral_base.rs` location rule):
//!   * `<base>/home`                             — the hook's `HOME` (has
//!     `.cache/x/f.txt`, `.ssh/id_rsa`, `.zshrc`, `docs/f.txt`)
//!   * `<base>/src/proj`                         — git repo main tree;
//!     `CLAUDE_PROJECT_DIR`
//!   * `<base>/src/.harness-worktrees/wt-own`    — linked worktree; the
//!     SESSION CWD (cell OwnWorktree)
//!   * `<base>/src/.harness-worktrees/wt-other`  — linked worktree NOT
//!     holding the session cwd (cell OtherWorktree)
//!   * `<base>/tmpdir`                           — the hook's `TMPDIR`
//!   * `<fixed temp root>/bg-s6-4-trunc-…`        — a dir strictly inside
//!     `/private/tmp` (macOS) / `/tmp` (Linux), with `sub/f.txt`
//!
//! The hook runs with `env_clear()` plus `HOME`, `PATH=/usr/bin:/bin`,
//! `TMPDIR`, `CLAUDECODE=1`, `CLAUDE_CODE_ENTRYPOINT=cli`,
//! `CLAUDE_PROJECT_DIR`, `BLASTGUARD_APPROVALS_DIR`; git variables scrubbed;
//! no `CLAUDE_CODE_SESSION_ID`. Subagent mode = `agent_id` in the payload.
//! Nothing is executed; the hook only judges the text.
//!
//! DOES NOT PROVE: that the retirement goes through `known_allow` (only the
//! decision is asserted); verdicts with the session cwd in the main tree, or
//! with a session id; symlinked targets; `$HOME/Library/Caches`; that a shape
//! not listed here keeps its verdict; the Ask/Deny REASON text of controls.
//!
//! FLAGGED for the caller (not decided here): `<own wt>/.claude/settings.json`
//! and `<own wt>/.git` are pinned as controls (Deny today) per the
//! instruction "Protected files stay as they are today", but
//! `judge::classify` LIFTS Protected strictly inside a linked worktree
//! checkout, so `known_allow` may answer true for them. If S6-4 moves them to
//! Allow, that is a ruling question (Table C lift vs "Protected keeps
//! today's"), not a test edit.

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
    let base = neutral_base::neutral_base().join(format!("s6-4-trunc-{tag}-{n}"));
    let tmp = fixed_tmp::canonical_fixed_temp_root()
        .join(format!("bg-s6-4-trunc-{tag}-{}-{n}", std::process::id()));
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
        "session_id": "s6-4-truncate-retire-20261120",
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

/// Placeholders: `{T}` dir strictly inside the fixed temp root, `{D}` the
/// hook's `TMPDIR`, `{H}` fixture `HOME`, `{W}` own worktree (session cwd),
/// `{O}` other worktree, `{P}` project main tree.
fn expand(fx: &Fx, t: &str) -> String {
    t.replace("{T}", fx.tmp.to_str().unwrap())
        .replace("{D}", fx.tmpdir().to_str().unwrap())
        .replace("{H}", fx.home().to_str().unwrap())
        .replace("{W}", fx.wt("wt-own").to_str().unwrap())
        .replace("{O}", fx.wt("wt-other").to_str().unwrap())
        .replace("{P}", fx.repo().to_str().unwrap())
}

/// `"main/subagent"` for the template, plus the main-mode reason.
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
        "{template:?} (Literal Scratch / OwnWorktree truncate target) must be Allow in both \
         modes after S6-4\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

// ---------------------------------------------------------------------------
// REDs: Literal Scratch targets.
// ---------------------------------------------------------------------------

#[test]
fn red_scratch_tmp_subdir_existing_file() {
    expect_allow("r-tmp-existing", "truncate -s 0 {T}/sub/f.txt");
}

#[test]
fn red_scratch_tmp_subdir_absent_file() {
    expect_allow("r-tmp-absent", "truncate -s 0 {T}/sub/new.txt");
}

#[test]
fn red_scratch_tmp_dir_direct_child_file() {
    expect_allow("r-tmp-child", "truncate -s 0 {T}/f.txt");
}

#[test]
fn red_scratch_attached_size_value() {
    expect_allow("r-tmp-s0", "truncate -s0 {T}/sub/f.txt");
}

#[test]
fn red_scratch_long_size_equals() {
    expect_allow("r-tmp-size-eq", "truncate --size=0 {T}/sub/f.txt");
}

#[test]
fn red_scratch_long_size_separate() {
    expect_allow("r-tmp-size-sep", "truncate --size 0 {T}/sub/f.txt");
}

#[test]
fn red_scratch_single_quoted_literal() {
    expect_allow("r-tmp-sq", "truncate -s 0 '{T}/sub/f.txt'");
}

#[test]
fn red_scratch_usr_bin_spelling() {
    expect_allow("r-tmp-usrbin", "/usr/bin/truncate -s 0 {T}/sub/f.txt");
}

#[test]
fn red_scratch_two_targets() {
    expect_allow("r-tmp-two", "truncate -s 0 {T}/sub/f.txt {T}/sub/g.txt");
}

#[test]
fn red_scratch_hook_tmpdir() {
    expect_allow("r-tmpdir", "truncate -s 0 {D}/sub/f.txt");
}

#[test]
fn red_scratch_home_cache() {
    expect_allow("r-cache", "truncate -s 0 {H}/.cache/x/f.txt");
}

// ---------------------------------------------------------------------------
// REDs: Literal OwnWorktree targets (session cwd = wt-own).
// ---------------------------------------------------------------------------

#[test]
fn red_own_worktree_absolute() {
    expect_allow("r-own-abs", "truncate -s 0 {W}/u.txt");
}

#[test]
fn red_own_worktree_absolute_subdir() {
    expect_allow("r-own-sub", "truncate -s 0 {W}/sub/f.txt");
}

#[test]
fn red_own_worktree_relative() {
    expect_allow("r-own-rel", "truncate -s 0 u.txt");
}

#[test]
fn red_own_worktree_relative_subdir() {
    expect_allow("r-own-rel-sub", "truncate -s 0 sub/f.txt");
}

#[test]
fn red_own_worktree_reference_file_is_a_read() {
    // `-r RFILE` reads RFILE (Table B); the only target is in OwnWorktree.
    expect_allow("r-own-ref", "truncate -r {P}/t.txt {W}/u.txt");
}

#[test]
fn red_scratch_and_own_worktree_mixed() {
    expect_allow("r-mixed", "truncate -s 0 {T}/sub/f.txt {W}/u.txt");
}

/// Pins the tie defect deterministically: a `0` path component equal to the
/// `-s 0` size value must not break the AST tie (fixture paths are random,
/// so this shape is not otherwise guaranteed to occur).
#[test]
fn red_scratch_path_component_equal_to_size_value() {
    expect_allow("r-tmp-s0-dir", "truncate -s 0 {T}/0/f.txt");
}

#[test]
fn red_scratch_path_component_containing_size_value() {
    expect_allow("r-tmp-s0-name", "truncate -s 0 {T}/s0/f.txt");
}

/// User ruling 2026-11-19 (literal-dir glob, docs/specs/blastguard.md,
/// "Table B の全行で"): a glob only in the final component under a literal
/// dir is placed by the parent's class — here Scratch — so Allow.
#[test]
fn literal_dir_final_component_glob_in_scratch() {
    expect_allow("glob-final", "truncate -s 0 {T}/sub/*.txt");
}

// ---------------------------------------------------------------------------
// Controls: measured at 342da07b8, must not move.
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

const AD: &str = "ask/deny";
const D: &str = "deny/deny";

#[rustfmt::skip]
const SYSTEM_AND_HOME: &[Pin] = &[
    ("System /etc", "truncate -s 0 /etc/hosts", D),
    ("System /usr/lib", "truncate -s 0 /usr/lib/libc.so", D),
    ("HOME asset ssh key", "truncate -s 0 {H}/.ssh/id_rsa", D),
    ("HOME asset shell rc", "truncate -s 0 {H}/.zshrc", D),
    ("Other (plain HOME dir)", "truncate -s 0 {H}/docs/f.txt", D),
    ("Scratch + System mix", "truncate -s 0 {T}/sub/f.txt /etc/hosts", D),
];

#[test]
fn control_system_and_home() {
    check("sys-home", SYSTEM_AND_HOME);
}

#[rustfmt::skip]
const PROTECTED: &[Pin] = &[
    ("Protected .git/config, main tree", "truncate -s 0 {P}/.git/config", D),
    ("Protected .git/HEAD, main tree", "truncate -s 0 {P}/.git/HEAD", AD),
    ("Protected .claude/settings.json, main tree", "truncate -s 0 {P}/.claude/settings.json", D),
    // FLAGGED (module doc): classify lifts Protected inside a linked
    // worktree checkout; pinned as today's per the caller's instruction.
    ("Protected .git file of own worktree", "truncate -s 0 {W}/.git", D),
    // User ruling 2026-11-20 「Deny のまま（推奨）」: command-running config
    // inside the own worktree keeps its verdict (measured at main 9f6cb2fea).
    ("Protected .claude/settings.json, own worktree", "truncate -s 0 {W}/.claude/settings.json", D),
    ("command-running config .claude/settings.local.json, own worktree", "truncate -s 0 {W}/.claude/settings.local.json", D),
    // FLAGGED: hooks.json is a confined Ask (not Deny) at main 9f6cb2fea;
    // pinned as measured. Unless it is Protected / Workflow in Table C, the
    // new layer places it OwnWorktree and S6-4 would move it to Allow.
    ("command-running config hooks/hooks.json, own worktree", "truncate -s 0 {W}/hooks/hooks.json", AD),
    ("command-running config crates/x/hooks/hooks.json, own worktree", "truncate -s 0 {W}/crates/x/hooks/hooks.json", AD),
    ("command-running config .githooks/pre-commit, own worktree", "truncate -s 0 {W}/.githooks/pre-commit", D),
];

#[test]
fn control_protected() {
    check("protected", PROTECTED);
}

#[rustfmt::skip]
const PLACEMENT: &[Pin] = &[
    ("temp root itself", "truncate -s 0 /tmp", D),
    ("other worktree", "truncate -s 0 {O}/u.txt", D),
    ("project main tree (Project, not Scratch/OwnWorktree)", "truncate -s 0 {P}/u.txt", AD),
];

#[test]
fn control_placement() {
    check("placement", PLACEMENT);
}

#[rustfmt::skip]
const DOTDOT: &[Pin] = &[
    ("`..` escaping temp to System", "truncate -s 0 {T}/sub/../../../etc/hosts", D),
    ("`..` staying inside temp", "truncate -s 0 {T}/sub/../sub/f.txt", AD),
    ("`..` from own worktree to other worktree, absolute", "truncate -s 0 {W}/../wt-other/u.txt", D),
    ("`..` relative to other worktree", "truncate -s 0 ../wt-other/u.txt", D),
];

#[test]
fn control_dotdot() {
    check("dotdot", DOTDOT);
}

#[rustfmt::skip]
const NON_LITERAL: &[Pin] = &[
    ("unquoted var, assigned to a temp literal", "F={T}/sub/f.txt; truncate -s 0 $F", AD),
    ("unquoted var, unassigned", "truncate -s 0 $F", D),
    ("quoted env expansion", "truncate -s 0 \"$TMPDIR/sub/f.txt\"", D),
    ("unquoted env expansion", "truncate -s 0 $TMPDIR/sub/f.txt", D),
    // The literal-dir glob ruling (2026-11-19) keeps a glob in a NON-final
    // component Unresolved; the final-component glob moved to
    // `literal_dir_final_component_glob_in_scratch`.
    ("middle-component glob in temp", "truncate -s 0 {T}/*/f.txt", D),
];

#[test]
fn control_non_literal() {
    check("nonlit", NON_LITERAL);
}

#[rustfmt::skip]
const UNLISTED_OPTION: &[Pin] = &[
    // `-c` is not in the Table B truncate option list (`-s`, `-r` only), so
    // the row is Undetermined and the legacy verdict stays.
    ("unlisted option -c, own worktree", "truncate -c -s 0 {W}/u.txt", AD),
];

#[test]
fn control_unlisted_option() {
    check("unlisted", UNLISTED_OPTION);
}
