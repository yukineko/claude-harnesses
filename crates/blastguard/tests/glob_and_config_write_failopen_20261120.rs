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
//! 2. / 3. (RE-ANCHORED, user ruling 2026-11-20 — supersedes the earlier
//!    same-day ruling "own-worktree command-running config appends → Ask"
//!    these sections were written against): in the OWN worktree `{W}`, every
//!    write — overwrite AND append: `>`, `>>`, `tee`, `tee -a`, `truncate`,
//!    literal or final-component glob — onto `.githooks/*`,
//!    `hooks/hooks.json`, `crates/*/hooks/*`, `scripts/check-*.py` is ALLOW
//!    in both modes (the 2026-10-03 linked-worktree lift wins).
//!    `.claude/settings*.json` in the own worktree stays Ask (main) / Deny
//!    (subagent) for every writer. Outside the own worktree (project main
//!    tree `{P}`, other worktree `{O}`) command-running config keeps a
//!    non-Allow verdict.
//!
//! Hole (1) is unchanged by that ruling.
//!
//! * REDs: `#[ignore = "RED ...: <observed> at <rev>"]`, the measured
//!   verdict in the reason; each expects the stated verdict.
//! * Controls (not ignored): must not move. A control is a
//!   characterization pin, not an endorsement.
//!
//! Placeholders: `{T}` dir strictly inside the fixed temp root, `{D}` the
//! hook's `TMPDIR`, `{H}` fixture `HOME`, `{W}` own worktree (session cwd),
//! `{O}` other worktree, `{P}` project main tree. Nothing is executed; the
//! hook only judges the text.
//!
//! DOES NOT PROVE: which layer produces a verdict (only the decision is
//! asserted); verdicts with the session cwd in the main tree; globs that
//! match nothing or match several classes; middle-component globs; the
//! Ask/Deny REASON text; writers other than the five listed (`dd`, `cp`,
//! `sed -i`, Write/Edit tools); symlinked config files; that the exact
//! main-mode verdict for `.claude/settings*.json` is Ask rather than Deny
//! (only "main non-Allow, subagent Deny" is asserted).

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
    // The same command-running config in the own worktree, the project main
    // tree and the other worktree (outside-own controls need real files).
    for tree in [own.clone(), fx.repo(), fx.wt("wt-other")] {
        write(&tree.join("hooks").join("hooks.json"), "{}\n");
        write(
            &tree
                .join("crates")
                .join("x")
                .join("hooks")
                .join("hooks.json"),
            "{}\n",
        );
        write(&tree.join(".githooks").join("pre-commit"), "#!/bin/sh\n");
        write(&tree.join("scripts").join("check-x.py"), "pass\n");
    }
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

/// Body for (1): the glob must get the verdict of the literal spelling of
/// the file it matches — both measured in the same fixture, so the property
/// (not a frozen letter) is what is asserted. `floor` is the literal
/// spelling's verdict measured at 40e49d3db, asserted too so that both
/// drifting to Allow together cannot pass.
fn expect_same_as_literal(tag: &str, glob: &str, literal: &str, floor: &str) {
    let fx = fixture(tag);
    let (got, rm, rs) = measure(&fx, glob);
    let (lit, lm, ls) = measure(&fx, literal);
    assert_eq!(
        got, lit,
        "{glob:?}: a final-component glob under a literal dir must get the verdict of \
         the literal spelling {literal:?} of the file it matches\n  glob main: {rm}\n  \
         glob sub:  {rs}\n  literal main: {lm}\n  literal sub:  {ls}"
    );
    assert_eq!(
        lit, floor,
        "{literal:?} (the literal spelling) moved off {floor}\n  main: {lm}\n  sub: {ls}"
    );
}

/// Body for the own-worktree lift (ruling 2026-11-20): Allow in both modes.
fn expect_allow(tag: &str, template: &str) {
    let fx = fixture(tag);
    let (got, rm, rs) = measure(&fx, template);
    assert_eq!(
        got, A,
        "{template:?} writes own-worktree .githooks / hooks.json / crates/*/hooks / \
         scripts/check-*.py, which user ruling 2026-11-20 makes Allow in both modes (the \
         2026-10-03 linked-worktree lift wins)\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

/// Body for `.claude/settings*.json` in the own worktree (ruling 2026-11-20:
/// stays Ask main / Deny subagent for every writer). Asserted as "main
/// non-Allow, subagent Deny" so a stricter main Deny does not fail.
fn expect_settings_blocked(tag: &str, template: &str) {
    let fx = fixture(tag);
    let c = expand(&fx, template);
    let (m, rm) = run_as(&fx, &c, false);
    let (s, rs) = run_as(&fx, &c, true);
    assert!(
        (m == "ask" || m == "deny") && s == "deny",
        "{template:?} writes own-worktree .claude/settings*.json and must stay Ask/Deny-class \
         (main non-Allow, subagent Deny); got {m}/{s}\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

/// Body for command-running config OUTSIDE the own worktree (`{P}`, `{O}`):
/// keeps a non-Allow verdict in both modes. Ask or Deny accepted.
fn expect_non_allow(tag: &str, template: &str) {
    let fx = fixture(tag);
    let c = expand(&fx, template);
    let (m, rm) = run_as(&fx, &c, false);
    let (s, rs) = run_as(&fx, &c, true);
    let ok = |d: &str| d == "ask" || d == "deny";
    assert!(
        ok(&m) && ok(&s),
        "{template:?} writes command-running config outside the own worktree and must be \
         non-Allow in both modes; got {m}/{s}\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

/// One `#[test]` per row, so a RED can be ignored without hiding the rest.
macro_rules! cases {
    ($body:ident; $($(#[$m:meta])* $name:ident: $t:literal;)*) => {
        $(
            $(#[$m])*
            #[test]
            fn $name() {
                $body(stringify!($name), $t);
            }
        )*
    };
}

// ---------------------------------------------------------------------------
// (1) Unquoted final-component glob behind a `>` redirect.
// ---------------------------------------------------------------------------

/// Literal `echo x > {H}/docs/f.txt` is deny/deny; the glob would expand to
/// that very file.
#[test]
fn red_redirect_glob_home_docs_matches_existing_file() {
    expect_same_as_literal(
        "r1-home-glob",
        "echo x > {H}/docs/*.txt",
        "echo x > {H}/docs/f.txt",
        D,
    );
}

// ---------------------------------------------------------------------------
// Own-worktree lift (user ruling 2026-11-20): every writer, literal and
// final-component glob, onto `.githooks/*`, `hooks/hooks.json`,
// `crates/*/hooks/*`, `scripts/check-*.py` in `{W}` is allow/allow. The
// former (2)/(3) REDs (expected non-Allow) are re-anchored here.
// ---------------------------------------------------------------------------

cases! { expect_allow;
    // hooks/hooks.json — literal
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_hooks_json_redirect_is_allow: "echo x > {W}/hooks/hooks.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_hooks_json_redirect_relative_is_allow: "echo x > hooks/hooks.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_hooks_json_append_is_allow: "echo x >> {W}/hooks/hooks.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_hooks_json_tee_is_allow: "tee {W}/hooks/hooks.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_hooks_json_tee_pipe_is_allow: "echo x | tee {W}/hooks/hooks.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_hooks_json_tee_append_is_allow: "tee -a {W}/hooks/hooks.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_hooks_json_truncate_is_allow: "truncate -s0 {W}/hooks/hooks.json";
    // hooks/hooks.json — final-component glob
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_hooks_json_glob_redirect_is_allow: "echo x > {W}/hooks/*.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_hooks_json_glob_append_is_allow: "echo x >> {W}/hooks/*.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_hooks_json_glob_tee_is_allow: "tee {W}/hooks/*.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_hooks_json_glob_tee_append_is_allow: "tee -a {W}/hooks/*.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_hooks_json_glob_truncate_is_allow: "truncate -s0 {W}/hooks/*.json";
    // crates/x/hooks/hooks.json — literal
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_crate_hooks_redirect_is_allow: "echo x > {W}/crates/x/hooks/hooks.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_crate_hooks_append_is_allow: "echo x >> {W}/crates/x/hooks/hooks.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_crate_hooks_tee_is_allow: "echo x | tee {W}/crates/x/hooks/hooks.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_crate_hooks_tee_append_is_allow: "echo x | tee -a {W}/crates/x/hooks/hooks.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_crate_hooks_truncate_is_allow: "truncate -s0 {W}/crates/x/hooks/hooks.json";
    // crates/x/hooks/hooks.json — final-component glob
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_crate_hooks_glob_redirect_is_allow: "echo x > {W}/crates/x/hooks/*.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_crate_hooks_glob_append_is_allow: "echo x >> {W}/crates/x/hooks/*.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_crate_hooks_glob_tee_is_allow: "echo x | tee {W}/crates/x/hooks/*.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_crate_hooks_glob_tee_append_is_allow: "echo x | tee -a {W}/crates/x/hooks/*.json";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_crate_hooks_glob_truncate_is_allow: "truncate -s0 {W}/crates/x/hooks/*.json";
    // .githooks/pre-commit — literal
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_githooks_redirect_is_allow: "echo x > {W}/.githooks/pre-commit";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_githooks_append_is_allow: "echo x >> {W}/.githooks/pre-commit";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_githooks_tee_is_allow: "echo x | tee {W}/.githooks/pre-commit";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_githooks_tee_append_is_allow: "echo x | tee -a {W}/.githooks/pre-commit";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_githooks_truncate_is_allow: "truncate -s0 {W}/.githooks/pre-commit";
    // .githooks/pre-commit — final-component glob
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_githooks_glob_redirect_is_allow: "echo x > {W}/.githooks/pre-*";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_githooks_glob_append_is_allow: "echo x >> {W}/.githooks/pre-*";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_githooks_glob_tee_is_allow: "echo x | tee {W}/.githooks/pre-*";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_githooks_glob_tee_append_is_allow: "echo x | tee -a {W}/.githooks/pre-*";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_githooks_glob_truncate_is_allow: "truncate -s0 {W}/.githooks/pre-*";
    // scripts/check-x.py — literal
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_check_script_redirect_is_allow: "echo x > {W}/scripts/check-x.py";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_check_script_append_is_allow: "echo x >> {W}/scripts/check-x.py";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_check_script_tee_is_allow: "echo x | tee {W}/scripts/check-x.py";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_check_script_tee_append_is_allow: "echo x | tee -a {W}/scripts/check-x.py";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_check_script_truncate_is_allow: "truncate -s0 {W}/scripts/check-x.py";
    // scripts/check-x.py — final-component glob (the only `*.py` there)
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_check_script_glob_redirect_is_allow: "echo x > {W}/scripts/*.py";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_check_script_glob_named_redirect_is_allow: "echo x > {W}/scripts/check-*.py";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_check_script_glob_append_is_allow: "echo x >> {W}/scripts/check-*.py";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_check_script_glob_tee_is_allow: "echo x | tee {W}/scripts/check-*.py";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: ask/deny at bb3ca4e39"]
    own_check_script_glob_tee_append_is_allow: "echo x | tee -a {W}/scripts/check-*.py";
    #[ignore = "RED ruling 2026-11-20 own-worktree config lift: deny/deny at bb3ca4e39"]
    own_check_script_glob_truncate_is_allow: "truncate -s0 {W}/scripts/check-*.py";
}

// ---------------------------------------------------------------------------
// `.claude/settings*.json` in the own worktree: stays Ask (main) / Deny
// (subagent) for every writer (ruling 2026-11-20).
// ---------------------------------------------------------------------------

cases! { expect_settings_blocked;
    own_settings_local_redirect_stays_blocked: "echo x > {W}/.claude/settings.local.json";
    own_settings_local_append_stays_blocked: "echo x >> {W}/.claude/settings.local.json";
    own_settings_local_tee_stays_blocked: "echo x | tee {W}/.claude/settings.local.json";
    own_settings_local_tee_append_stays_blocked: "echo x | tee -a {W}/.claude/settings.local.json";
    own_settings_local_truncate_stays_blocked: "truncate -s0 {W}/.claude/settings.local.json";
    own_settings_redirect_stays_blocked: "echo x > {W}/.claude/settings.json";
    own_settings_append_stays_blocked: "echo x >> {W}/.claude/settings.json";
    own_settings_glob_redirect_stays_blocked: "echo x > {W}/.claude/settings*.json";
    own_settings_glob_append_stays_blocked: "echo x >> {W}/.claude/settings*.json";
    own_settings_glob_tee_stays_blocked: "echo x | tee {W}/.claude/settings*.json";
    own_settings_glob_tee_append_stays_blocked: "echo x | tee -a {W}/.claude/settings*.json";
    own_settings_glob_truncate_stays_blocked: "truncate -s0 {W}/.claude/settings*.json";
}

// ---------------------------------------------------------------------------
// Outside the own worktree: command-running config keeps a non-Allow
// verdict (ruling 2026-11-20).
// ---------------------------------------------------------------------------

cases! { expect_non_allow;
    project_hooks_json_redirect_non_allow: "echo x > {P}/hooks/hooks.json";
    project_hooks_json_append_non_allow: "echo x >> {P}/hooks/hooks.json";
    project_hooks_json_tee_append_non_allow: "echo x | tee -a {P}/hooks/hooks.json";
    project_crate_hooks_append_non_allow: "echo x >> {P}/crates/x/hooks/hooks.json";
    project_githooks_redirect_non_allow: "echo x > {P}/.githooks/pre-commit";
    project_githooks_append_non_allow: "echo x >> {P}/.githooks/pre-commit";
    project_githooks_truncate_non_allow: "truncate -s0 {P}/.githooks/pre-commit";
    project_check_script_append_non_allow: "echo x >> {P}/scripts/check-x.py";
    project_check_script_glob_redirect_non_allow: "echo x > {P}/scripts/check-*.py";
    other_wt_hooks_json_redirect_non_allow: "echo x > {O}/hooks/hooks.json";
    other_wt_hooks_json_append_non_allow: "echo x >> {O}/hooks/hooks.json";
    other_wt_crate_hooks_tee_append_non_allow: "echo x | tee -a {O}/crates/x/hooks/hooks.json";
    other_wt_githooks_append_non_allow: "echo x >> {O}/.githooks/pre-commit";
    other_wt_githooks_truncate_non_allow: "truncate -s0 {O}/.githooks/pre-commit";
    other_wt_check_script_append_non_allow: "echo x >> {O}/scripts/check-x.py";
    other_wt_check_script_glob_append_non_allow: "echo x >> {O}/scripts/check-*.py";
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
