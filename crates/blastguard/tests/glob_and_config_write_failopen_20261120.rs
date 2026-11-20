#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fail-open REDs reported 2026-11-20 by the S6-6 implementer, re-measured
//! by an independent test author (not the implementer; CLAUDE.md §2(a)) at
//! main 40e49d3db. Harness and fixture copied from
//! `s6_5_dd_retire_20261120.rs` (session cwd = own worktree `{W}`).
//!
//! 1. A shell `>` redirect onto an UNQUOTED final-component glob under a
//!    literal dir is judged as if the glob were a nonexistent single path.
//!    Literal-dir glob ruling 2026-11-19: it is placed by the parent dir's
//!    class / what it would expand to, so it must get at least the verdict
//!    of the literal spelling of a matching file. A QUOTED `'*.txt'` names
//!    a literal file called `*.txt` and keeps literal semantics (control).
//! 2. The command-running-config predicate misses a final-component glob in
//!    a plugin `hooks/` dir (`truncate` / `tee` / `tee -a` / `>>`).
//! 3. A truncating `>` onto own-worktree command-running config is Allow
//!    while append / truncate / dd onto the same file are not. User ruling
//!    2026-11-20: command-running config in the own worktree keeps a
//!    non-Allow verdict, appends included; overwrite must not be looser
//!    than append.
//!
//! * REDs: `#[ignore = "RED fail-open: <verdict> at 40e49d3db"]`, the
//!   measured verdict in the reason; each expects the stated verdict.
//! * Controls (not ignored): measured at the same base, must not move.
//!   A control is a characterization pin, not an endorsement.
//!
//! Placeholders: `{T}` dir strictly inside the fixed temp root, `{D}` the
//! hook's `TMPDIR`, `{H}` fixture `HOME`, `{W}` own worktree (session cwd),
//! `{O}` other worktree, `{P}` project main tree. Nothing is executed; the
//! hook only judges the text.
//!
//! DOES NOT PROVE: which layer produces a verdict (only the decision is
//! asserted); verdicts with the session cwd in the main tree; globs that
//! match nothing or match several classes; middle-component globs; the
//! Ask/Deny REASON text.

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
    let base = neutral_base::neutral_base().join(format!("glob-cfg-{tag}-{n}"));
    let tmp = fixed_tmp::canonical_fixed_temp_root()
        .join(format!("bg-glob-cfg-{tag}-{}-{n}", std::process::id()));
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
        "session_id": "glob-cfg-failopen-20261120",
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

/// RED body for (1): the glob must get exactly `exp` (the verdict of the
/// literal spelling of the matching file).
fn expect_exact(tag: &str, template: &str, exp: &str) {
    let fx = fixture(tag);
    let (got, rm, rs) = measure(&fx, template);
    assert_eq!(
        got, exp,
        "{template:?}: a final-component glob under a literal dir must get the verdict of \
         the literal spelling of a matching file\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

/// RED body for (2)/(3): command-running config in the own worktree keeps a
/// non-Allow verdict in BOTH modes (user ruling 2026-11-20). Ask or Deny is
/// accepted in each mode; only Allow fails.
fn expect_non_allow(tag: &str, template: &str) {
    let fx = fixture(tag);
    let c = expand(&fx, template);
    let (m, rm) = run_as(&fx, &c, false);
    let (s, rs) = run_as(&fx, &c, true);
    let ok = |d: &str| d == "ask" || d == "deny";
    assert!(
        ok(&m) && ok(&s),
        "{template:?} writes own-worktree command-running config and must be non-Allow in \
         both modes; got {m}/{s}\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

// ---------------------------------------------------------------------------
// (1) Unquoted final-component glob behind a `>` redirect.
// ---------------------------------------------------------------------------

/// Literal `echo x > {H}/docs/f.txt` is deny/deny (control below); the glob
/// would expand to that very file.
#[test]
fn red_redirect_glob_home_docs_matches_existing_file() {
    expect_exact("r1-home-glob", "echo x > {H}/docs/*.txt", D);
}

// ---------------------------------------------------------------------------
// (2) Final-component glob onto own-worktree command-running config.
// Literal `truncate -s0 {W}/hooks/hooks.json` is ask/deny and literal
// `tee {W}/hooks/hooks.json` is deny/deny (controls below).
// ---------------------------------------------------------------------------

#[test]
fn red_truncate_glob_hooks_json() {
    expect_non_allow("r2-trunc", "truncate -s0 {W}/hooks/*.json");
}

#[test]
fn red_tee_glob_hooks_json() {
    expect_non_allow("r2-tee", "tee {W}/hooks/*.json");
}

#[test]
fn red_tee_append_glob_hooks_json() {
    expect_non_allow("r2-tee-a", "tee -a {W}/hooks/*.json");
}

#[test]
fn red_append_redirect_glob_hooks_json() {
    expect_non_allow("r2-append", "echo x >> {W}/hooks/*.json");
}

#[test]
fn red_truncating_redirect_glob_hooks_json() {
    expect_non_allow("r2-redir", "echo x > {W}/hooks/*.json");
}

#[test]
fn red_truncating_redirect_glob_crate_hooks_json() {
    expect_non_allow("r2-crate", "echo x > {W}/crates/x/hooks/*.json");
}

/// The only `*.py` in `{W}/scripts` is `check-x.py` (command-running config).
#[test]
fn red_truncating_redirect_glob_scripts_check() {
    expect_non_allow("r2-scripts", "echo x > {W}/scripts/*.py");
}

// ---------------------------------------------------------------------------
// (3) Literal `>` / `>>` / `tee -a` onto own-worktree command-running config.
// The same files are deny/deny under `dd of=` (s6_5 control_protected) and
// `truncate` is ask/deny (control below); overwrite / append must not be
// looser.
// ---------------------------------------------------------------------------

#[test]
fn red_redirect_hooks_json() {
    expect_non_allow("r3-hooks", "echo x > {W}/hooks/hooks.json");
}

#[test]
fn red_redirect_hooks_json_relative() {
    expect_non_allow("r3-hooks-rel", "echo x > hooks/hooks.json");
}

#[test]
fn red_redirect_crate_hooks_json() {
    expect_non_allow("r3-crate", "echo x > {W}/crates/x/hooks/hooks.json");
}

#[test]
fn red_redirect_scripts_check() {
    expect_non_allow("r3-scripts", "echo x > {W}/scripts/check-x.py");
}

#[test]
fn red_redirect_githooks_pre_commit() {
    expect_non_allow("r3-githooks", "echo x > {W}/.githooks/pre-commit");
}

#[test]
fn red_redirect_settings_local_json() {
    expect_non_allow("r3-settings", "echo x > {W}/.claude/settings.local.json");
}

#[test]
fn red_append_redirect_hooks_json() {
    expect_non_allow("r3-app-hooks", "echo x >> {W}/hooks/hooks.json");
}

#[test]
fn red_tee_append_hooks_json() {
    expect_non_allow("r3-tee-a", "tee -a {W}/hooks/hooks.json");
}

#[test]
fn red_append_redirect_crate_hooks_json() {
    expect_non_allow("r3-app-crate", "echo x >> {W}/crates/x/hooks/hooks.json");
}

#[test]
fn red_append_redirect_scripts_check() {
    expect_non_allow("r3-app-scripts", "echo x >> {W}/scripts/check-x.py");
}

#[test]
fn red_append_redirect_githooks_pre_commit() {
    expect_non_allow("r3-app-githooks", "echo x >> {W}/.githooks/pre-commit");
}

// ---------------------------------------------------------------------------
// Controls: measured at 40e49d3db, must not move.
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
        "[{table}] {} of {} control pins moved:\n{}",
        bad.len(),
        pins.len(),
        bad.join("\n")
    );
}

const A: &str = "allow/allow";
const D: &str = "deny/deny";

#[rustfmt::skip]
const LITERAL_AND_QUOTED: &[Pin] = &[
    ("literal existing plain HOME file", "echo x > {H}/docs/f.txt", D),
    ("literal absent plain HOME file", "echo x > {H}/docs/new.txt", A),
    // A quoted / escaped `*` names a literal (absent) file called `*.txt`:
    // literal semantics, same verdict as the absent literal above.
    ("single-quoted final component names a literal *.txt", "echo x > {H}/docs/'*.txt'", A),
    ("whole path single-quoted names a literal *.txt", "echo x > '{H}/docs/*.txt'", A),
    ("backslash-escaped * names a literal *.txt", "echo x > {H}/docs/\\*.txt", A),
];

#[test]
fn control_literal_and_quoted() {
    check("lit-quoted", LITERAL_AND_QUOTED);
}

#[rustfmt::skip]
const SCRATCH_AND_OWN: &[Pin] = &[
    ("Scratch final-component glob", "echo x > {T}/sub/*.txt", A),
    ("Scratch literal", "echo x > {T}/sub/f.txt", A),
    ("own-worktree ordinary file", "echo x > {W}/u.txt", A),
    ("own-worktree ordinary final-component glob", "echo x > {W}/sub/*.txt", A),
];

#[test]
fn control_scratch_and_own_worktree() {
    check("scratch-own", SCRATCH_AND_OWN);
}

#[rustfmt::skip]
const LITERAL_CONFIG: &[Pin] = &[
    ("truncate literal hooks.json", "truncate -s0 {W}/hooks/hooks.json", "ask/deny"),
    ("tee literal hooks.json", "tee {W}/hooks/hooks.json", D),
];

#[test]
fn control_literal_config() {
    check("lit-config", LITERAL_CONFIG);
}
