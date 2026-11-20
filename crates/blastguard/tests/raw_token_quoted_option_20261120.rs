#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Raw-token option tests in legacy arms (2026-11-20).
//!
//! Background: the legacy `dd` arm tested raw whitespace-split tokens with
//! `starts_with("of=")`, so a quoted `'of=/dev/sda'` was Allow. An implementer
//! listed other legacy arms that test raw tokens with `starts_with("<opt>=")`
//! or equality:
//!
//! * `src/detect/fileops.rs` — `sort --output=` (`sort_output_file`),
//!   `cp`/`mv --target-directory=`, `sed --in-place` / `--in-place=`
//!   (`analyze_sed`)
//! * `src/detect/high_blast.rs` — `git push --force` / `--force-with-lease` /
//!   `--force-with-lease=` (`high_blast_git_force_push`)
//! * `src/detect/interpreter.rs` — `--in-place=` for perl/ruby/sed/gsed
//!   (`interpreter_edits_in_place`), `--eval=` / `--print=`
//!   (`is_inline_eval_flag_for`)
//! * `src/detect.rs` — `flock --command=`
//! * `src/detect/sinks.rs` — `base64 --decode` / `--decode=` / `-d`
//!   (`is_fetch_or_decode`)
//!
//! Written by an independent test author (not the implementer; CLAUDE.md
//! §2(a)).
//!
//! For each arm the UNQUOTED dangerous spelling is the control (its verdict
//! is pinned as measured at main 8d6b3403d, 2026-11-20). The same command
//! with the option word single-quoted, double-quoted and backslash-escaped is
//! the same command to the shell (quote removal), so its verdict must be the
//! same class or stricter in BOTH modes (main and subagent), with
//! `allow < ask < deny`. The variant tests compare live against the
//! unquoted spelling measured in the same run (not against the pin).
//!
//! * `#[ignore = "RED fail-open: .."]`: variants MEASURED less strict than
//!   the unquoted spelling at 8d6b3403d; the measured verdicts are in the
//!   ignore reason.
//! * Not ignored: controls, and variants that already match (or are
//!   stricter). At 8d6b3403d EVERY quoted / escaped variant measured was
//!   less strict, so every variant test is ignored.
//! * `sort -o/--output` and `gsed -i/--in-place` onto a System file are
//!   Allow even UNQUOTED at 8d6b3403d (while `sed -i` onto `/etc/hosts` is
//!   deny/deny), so those spellings are held to deny/deny directly (system
//!   harm) instead of a live comparison.
//! * Short-flag siblings of the same class (`'-i'`, `'-t'`, `'-f'`, `'-e'`,
//!   `'-c'`, `'-d'`) are included: they fail open the same way.
//!
//! Fixture: `<base>/home` (the hook's `HOME`), `<base>/src/proj` (git repo
//! main tree, `CLAUDE_PROJECT_DIR`), `<base>/src/.harness-worktrees/wt-own`
//! (linked worktree, the SESSION CWD), `<base>/tmpdir` (`TMPDIR`). The hook
//! runs with `env_clear()` plus `HOME`, `PATH=/usr/bin:/bin`, `TMPDIR`,
//! `CLAUDECODE=1`, `CLAUDE_CODE_ENTRYPOINT=cli`, `CLAUDE_PROJECT_DIR`,
//! `BLASTGUARD_APPROVALS_DIR`; git variables scrubbed. Subagent mode =
//! `agent_id` in the payload. Nothing is executed; the hook only judges text.
//!
//! DOES NOT PROVE: which arm produced a verdict (only the decision is
//! asserted, so a variant may be caught by a different layer than the
//! unquoted spelling); quoting of only part of the option word (e.g.
//! `--out'put'=`), `$'..'` ANSI-C quoting, or quoting introduced through
//! variables; the reason text; verdicts with a session id or cwd in the main
//! tree.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
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
    fn repo(&self) -> PathBuf {
        self.base.join("src").join("proj")
    }
    fn wt(&self) -> PathBuf {
        self.base
            .join("src")
            .join(".harness-worktrees")
            .join("wt-own")
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

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let base = neutral_base::neutral_base().join(format!("rawtok-{tag}-{n}"));
    let _ = std::fs::remove_dir_all(&base);
    for d in [
        base.join("home"),
        base.join("src").join("proj"),
        base.join("src").join(".harness-worktrees"),
        base.join("store"),
        base.join("tmpdir"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    let fx = Fx { base };
    let repo = fx.repo();
    std::fs::write(repo.join("t.txt"), "a\n").unwrap();
    fx.git(&repo, &["init", "-q", "-b", "main"]);
    fx.git(&repo, &["add", "-A"]);
    fx.git(&repo, &["commit", "-q", "-m", "init"]);
    let wt = fx.wt();
    fx.git(
        &repo,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "own"],
    );
    std::fs::write(wt.join("x"), "a\n").unwrap();
    fx
}

/// Run the hook once with the session cwd in `wt-own`; `(decision, reason)`.
/// A non-zero exit panics; only then is an empty stdout read as Allow;
/// non-JSON stdout panics.
fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let cwd = fx.wt();
    let mut p = serde_json::json!({
        "session_id": "raw-token-quoted-option-20261120",
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
        .env("TMPDIR", fx.base.join("tmpdir"))
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

/// `allow < ask < deny`; anything else panics (an unknown decision is not
/// silently ranked).
fn rank(d: &str) -> u8 {
    match d {
        "allow" => 0,
        "ask" => 1,
        "deny" => 2,
        other => panic!("unknown permissionDecision {other:?}"),
    }
}

/// `"main/subagent"` plus both reasons.
fn measure(fx: &Fx, command: &str) -> (String, String, String, String) {
    let (m, rm) = run_as(fx, command, false);
    let (s, rs) = run_as(fx, command, true);
    (m, s, rm, rs)
}

/// Control: the unquoted spelling keeps its measured verdict.
fn pin(tag: &str, command: &str, expected: &str) {
    let fx = fixture(tag);
    let (m, s, rm, rs) = measure(&fx, command);
    let got = format!("{m}/{s}");
    println!("MEASURE control {command:?} => {got}\n    main: {rm}\n    sub:  {rs}");
    assert_eq!(
        got, expected,
        "control {command:?} moved\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

/// RED body for a shape whose unquoted spelling is itself fail-open: the
/// command must reach `expected` ("main/subagent").
fn expect(tag: &str, command: &str, expected: &str) {
    let fx = fixture(tag);
    let (m, s, rm, rs) = measure(&fx, command);
    let got = format!("{m}/{s}");
    assert_eq!(
        got, expected,
        "{command:?} must be {expected}\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

/// Variant: same verdict class as `unquoted` or stricter, in both modes.
fn same_or_stricter(tag: &str, unquoted: &str, variant: &str) {
    let fx = fixture(tag);
    let (um, us, _, _) = measure(&fx, unquoted);
    let (vm, vs, rm, rs) = measure(&fx, variant);
    println!("MEASURE {unquoted:?} => {um}/{us}; {variant:?} => {vm}/{vs}");
    assert!(
        rank(&um) > 0,
        "unquoted control {unquoted:?} is Allow in main mode; the comparison is vacuous"
    );
    assert!(
        rank(&vm) >= rank(&um) && rank(&vs) >= rank(&us),
        "{variant:?} is LESS strict than {unquoted:?}: {vm}/{vs} vs {um}/{us} (main/subagent); \
         quote removal makes them the same command\n  main reason: {rm}\n  sub reason:  {rs}"
    );
}

// ---------------------------------------------------------------------------
// sort --output= (fileops.rs sort_output_file)
// ---------------------------------------------------------------------------

// The UNQUOTED spellings are already Allow at 8d6b3403d, so a live
// comparison would be vacuous; every spelling is held to the system-harm
// verdict instead (memory "system-harm-always-deny": overwriting `/etc/passwd`
// is Deny in every shape), with `sed -i -e d /etc/hosts` (deny/deny) as the
// in-repo precedent for a System rewrite target.

#[test]
fn reference_sed_short_in_place_system_target_is_deny() {
    pin("sed-ref", "sed -i -e d /etc/hosts", "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm): measured allow/allow at 8d6b3403d"]
fn sort_output_eq_unquoted_system_target() {
    expect("sort-c", "sort --output=/etc/passwd x", "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm): measured allow/allow at 8d6b3403d"]
fn sort_output_separate_unquoted_system_target() {
    expect("sort-sep", "sort --output /etc/passwd x", "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm): measured allow/allow at 8d6b3403d"]
fn sort_short_o_unquoted_system_target() {
    expect("sort-o", "sort -o /etc/passwd x", "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm): measured allow/allow at 8d6b3403d"]
fn sort_output_single_quoted() {
    expect("sort-sq", "sort '--output=/etc/passwd' x", "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm): measured allow/allow at 8d6b3403d"]
fn sort_output_double_quoted() {
    expect("sort-dq", "sort \"--output=/etc/passwd\" x", "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm): measured allow/allow at 8d6b3403d"]
fn sort_output_escaped() {
    expect("sort-esc", "sort \\--output=/etc/passwd x", "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm): measured allow/allow at 8d6b3403d"]
fn sort_short_o_single_quoted() {
    expect("sort-o-sq", "sort '-o' /etc/passwd x", "deny/deny");
}

// ---------------------------------------------------------------------------
// cp / mv --target-directory= (fileops.rs)
// ---------------------------------------------------------------------------

const CP: &str = "cp --target-directory=/etc x";
const MV: &str = "mv --target-directory=/etc x";

#[test]
fn control_cp_target_directory() {
    pin("cp-c", CP, "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn cp_target_directory_single_quoted() {
    same_or_stricter("cp-sq", CP, "cp '--target-directory=/etc' x");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn cp_target_directory_double_quoted() {
    same_or_stricter("cp-dq", CP, "cp \"--target-directory=/etc\" x");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn cp_target_directory_escaped() {
    same_or_stricter("cp-esc", CP, "cp \\--target-directory=/etc x");
}

#[test]
fn control_mv_target_directory() {
    pin("mv-c", MV, "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn mv_target_directory_single_quoted() {
    same_or_stricter("mv-sq", MV, "mv '--target-directory=/etc' x");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn mv_target_directory_double_quoted() {
    same_or_stricter("mv-dq", MV, "mv \"--target-directory=/etc\" x");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn mv_target_directory_escaped() {
    same_or_stricter("mv-esc", MV, "mv \\--target-directory=/etc x");
}

// ---------------------------------------------------------------------------
// sed --in-place= / --in-place (fileops.rs analyze_sed, interpreter.rs)
// ---------------------------------------------------------------------------

const SED_EQ: &str = "sed --in-place= -e d /etc/hosts";
const SED_BARE: &str = "sed --in-place -e d /etc/hosts";
const GSED_EQ: &str = "gsed --in-place= -e d /etc/hosts";

#[test]
fn control_sed_in_place_eq() {
    pin("sed-c", SED_EQ, "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn sed_in_place_eq_single_quoted() {
    same_or_stricter("sed-sq", SED_EQ, "sed '--in-place=' -e d /etc/hosts");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn sed_in_place_eq_double_quoted() {
    same_or_stricter("sed-dq", SED_EQ, "sed \"--in-place=\" -e d /etc/hosts");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn sed_in_place_eq_escaped() {
    same_or_stricter("sed-esc", SED_EQ, "sed \\--in-place= -e d /etc/hosts");
}

#[test]
fn control_sed_in_place_bare() {
    pin("sedb-c", SED_BARE, "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn sed_in_place_bare_single_quoted() {
    same_or_stricter("sedb-sq", SED_BARE, "sed '--in-place' -e d /etc/hosts");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn sed_in_place_bare_double_quoted() {
    same_or_stricter("sedb-dq", SED_BARE, "sed \"--in-place\" -e d /etc/hosts");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn sed_in_place_bare_escaped() {
    same_or_stricter("sedb-esc", SED_BARE, "sed \\--in-place -e d /etc/hosts");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn sed_short_in_place_single_quoted() {
    same_or_stricter(
        "sedi-sq",
        "sed -i -e d /etc/hosts",
        "sed '-i' -e d /etc/hosts",
    );
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn perl_short_in_place_single_quoted() {
    same_or_stricter(
        "perli-sq",
        "perl -i -pe 1 /etc/hosts",
        "perl '-i' -pe 1 /etc/hosts",
    );
}

// `gsed` (GNU sed): the UNQUOTED spellings are already Allow at 8d6b3403d
// while `sed` with the same arguments is deny/deny; held to deny/deny (system
// harm) like the sort section.

#[test]
#[ignore = "RED fail-open (system harm): measured allow/allow at 8d6b3403d"]
fn gsed_in_place_eq_unquoted_system_target() {
    expect("gsed-c", GSED_EQ, "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm): measured allow/allow at 8d6b3403d"]
fn gsed_short_in_place_unquoted_system_target() {
    expect("gsed-i", "gsed -i -e d /etc/hosts", "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm): measured allow/allow at 8d6b3403d"]
fn gsed_in_place_eq_single_quoted() {
    expect("gsed-sq", "gsed '--in-place=' -e d /etc/hosts", "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm): measured allow/allow at 8d6b3403d"]
fn gsed_in_place_eq_double_quoted() {
    expect(
        "gsed-dq",
        "gsed \"--in-place=\" -e d /etc/hosts",
        "deny/deny",
    );
}

#[test]
#[ignore = "RED fail-open (system harm): measured allow/allow at 8d6b3403d"]
fn gsed_in_place_eq_escaped() {
    expect(
        "gsed-esc",
        "gsed \\--in-place= -e d /etc/hosts",
        "deny/deny",
    );
}

// ---------------------------------------------------------------------------
// git push --force / --force-with-lease(=) (high_blast.rs)
// ---------------------------------------------------------------------------

const PUSH_F: &str = "git push --force origin main";
const PUSH_L: &str = "git push --force-with-lease origin main";
const PUSH_LE: &str = "git push --force-with-lease=main origin main";

#[test]
fn control_git_push_force() {
    pin("pushf-c", PUSH_F, "ask/deny");
}

#[test]
#[ignore = "RED fail-open: measured allow/allow vs unquoted ask/deny at 8d6b3403d"]
fn git_push_force_single_quoted() {
    same_or_stricter("pushf-sq", PUSH_F, "git push '--force' origin main");
}

#[test]
#[ignore = "RED fail-open: measured allow/allow vs unquoted ask/deny at 8d6b3403d"]
fn git_push_force_double_quoted() {
    same_or_stricter("pushf-dq", PUSH_F, "git push \"--force\" origin main");
}

#[test]
#[ignore = "RED fail-open: measured allow/allow vs unquoted ask/deny at 8d6b3403d"]
fn git_push_force_escaped() {
    same_or_stricter("pushf-esc", PUSH_F, "git push \\--force origin main");
}

#[test]
fn control_git_push_force_with_lease() {
    pin("pushl-c", PUSH_L, "ask/deny");
}

#[test]
#[ignore = "RED fail-open: measured allow/allow vs unquoted ask/deny at 8d6b3403d"]
fn git_push_force_with_lease_single_quoted() {
    same_or_stricter(
        "pushl-sq",
        PUSH_L,
        "git push '--force-with-lease' origin main",
    );
}

#[test]
#[ignore = "RED fail-open: measured allow/allow vs unquoted ask/deny at 8d6b3403d"]
fn git_push_force_with_lease_double_quoted() {
    same_or_stricter(
        "pushl-dq",
        PUSH_L,
        "git push \"--force-with-lease\" origin main",
    );
}

#[test]
#[ignore = "RED fail-open: measured allow/allow vs unquoted ask/deny at 8d6b3403d"]
fn git_push_force_with_lease_escaped() {
    same_or_stricter(
        "pushl-esc",
        PUSH_L,
        "git push \\--force-with-lease origin main",
    );
}

#[test]
fn control_git_push_force_with_lease_eq() {
    pin("pushle-c", PUSH_LE, "ask/deny");
}

#[test]
#[ignore = "RED fail-open: measured allow/allow vs unquoted ask/deny at 8d6b3403d"]
fn git_push_force_with_lease_eq_single_quoted() {
    same_or_stricter(
        "pushle-sq",
        PUSH_LE,
        "git push '--force-with-lease=main' origin main",
    );
}

#[test]
#[ignore = "RED fail-open: measured allow/allow vs unquoted ask/deny at 8d6b3403d"]
fn git_push_force_with_lease_eq_double_quoted() {
    same_or_stricter(
        "pushle-dq",
        PUSH_LE,
        "git push \"--force-with-lease=main\" origin main",
    );
}

#[test]
#[ignore = "RED fail-open: measured allow/allow vs unquoted ask/deny at 8d6b3403d"]
fn git_push_force_with_lease_eq_escaped() {
    same_or_stricter(
        "pushle-esc",
        PUSH_LE,
        "git push \\--force-with-lease=main origin main",
    );
}

// ---------------------------------------------------------------------------
// --eval= / --print= (interpreter.rs is_inline_eval_flag_for)
// ---------------------------------------------------------------------------

const NODE_EVAL: &str = "node --eval='require(\"fs\").rmSync(\"/usr\",{recursive:true})'";
const NODE_PRINT: &str = "node --print='require(\"fs\").rmSync(\"/usr\",{recursive:true})'";

#[test]
fn control_node_eval() {
    pin("neval-c", NODE_EVAL, "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn node_eval_single_quoted() {
    same_or_stricter(
        "neval-sq",
        NODE_EVAL,
        "node '--eval=require(\"fs\").rmSync(\"/usr\",{recursive:true})'",
    );
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn node_eval_double_quoted() {
    same_or_stricter(
        "neval-dq",
        NODE_EVAL,
        "node \"--eval=require('fs').rmSync('/usr',{recursive:true})\"",
    );
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn node_eval_escaped() {
    same_or_stricter(
        "neval-esc",
        NODE_EVAL,
        "node \\--eval='require(\"fs\").rmSync(\"/usr\",{recursive:true})'",
    );
}

#[test]
fn control_node_print() {
    pin("nprint-c", NODE_PRINT, "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn node_print_single_quoted() {
    same_or_stricter(
        "nprint-sq",
        NODE_PRINT,
        "node '--print=require(\"fs\").rmSync(\"/usr\",{recursive:true})'",
    );
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn node_print_double_quoted() {
    same_or_stricter(
        "nprint-dq",
        NODE_PRINT,
        "node \"--print=require('fs').rmSync('/usr',{recursive:true})\"",
    );
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn node_print_escaped() {
    same_or_stricter(
        "nprint-esc",
        NODE_PRINT,
        "node \\--print='require(\"fs\").rmSync(\"/usr\",{recursive:true})'",
    );
}

// ---------------------------------------------------------------------------
// flock --command= (detect.rs)
// ---------------------------------------------------------------------------

const FLOCK: &str = "flock --command='rm -rf /' /tmp/l";

#[test]
fn control_flock_command() {
    pin("flock-c", FLOCK, "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn flock_command_single_quoted() {
    same_or_stricter("flock-sq", FLOCK, "flock '--command=rm -rf /' /tmp/l");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn flock_command_double_quoted() {
    same_or_stricter("flock-dq", FLOCK, "flock \"--command=rm -rf /\" /tmp/l");
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn flock_command_escaped() {
    same_or_stricter("flock-esc", FLOCK, "flock \\--command='rm -rf /' /tmp/l");
}

// ---------------------------------------------------------------------------
// base64 --decode / -d feeding a shell (sinks.rs is_fetch_or_decode)
// ---------------------------------------------------------------------------

const B64_LONG: &str = "echo cm0gLXJmIC8K | base64 --decode | sh";
const B64_SHORT: &str = "echo cm0gLXJmIC8K | base64 -d | sh";

#[test]
fn control_base64_decode_long() {
    pin("b64l-c", B64_LONG, "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn base64_decode_long_single_quoted() {
    same_or_stricter(
        "b64l-sq",
        B64_LONG,
        "echo cm0gLXJmIC8K | base64 '--decode' | sh",
    );
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn base64_decode_long_double_quoted() {
    same_or_stricter(
        "b64l-dq",
        B64_LONG,
        "echo cm0gLXJmIC8K | base64 \"--decode\" | sh",
    );
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn base64_decode_long_escaped() {
    same_or_stricter(
        "b64l-esc",
        B64_LONG,
        "echo cm0gLXJmIC8K | base64 \\--decode | sh",
    );
}

#[test]
fn control_base64_decode_short() {
    pin("b64s-c", B64_SHORT, "deny/deny");
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn base64_decode_short_single_quoted() {
    same_or_stricter("b64s-sq", B64_SHORT, "echo cm0gLXJmIC8K | base64 '-d' | sh");
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn base64_decode_short_double_quoted() {
    same_or_stricter(
        "b64s-dq",
        B64_SHORT,
        "echo cm0gLXJmIC8K | base64 \"-d\" | sh",
    );
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn base64_decode_short_escaped() {
    same_or_stricter(
        "b64s-esc",
        B64_SHORT,
        "echo cm0gLXJmIC8K | base64 \\-d | sh",
    );
}

// ---------------------------------------------------------------------------
// Short-flag siblings of the same raw-token class (quoted `-t` / `-f` / `-e`
// / `-c`), and flock with the lock file first.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn cp_short_t_single_quoted() {
    same_or_stricter("cpt-sq", "cp -t /etc x", "cp '-t' /etc x");
}

#[test]
#[ignore = "RED fail-open: measured allow/allow vs unquoted ask/deny at 8d6b3403d"]
fn git_push_short_f_single_quoted() {
    same_or_stricter(
        "pushsf-sq",
        "git push -f origin main",
        "git push '-f' origin main",
    );
}

#[test]
#[ignore = "RED fail-open (system harm) measured ask/deny vs unquoted deny/deny at 8d6b3403d"]
fn node_short_e_single_quoted() {
    same_or_stricter(
        "nodee-sq",
        "node -e 'require(\"fs\").rmSync(\"/usr\",{recursive:true})'",
        "node '-e' 'require(\"fs\").rmSync(\"/usr\",{recursive:true})'",
    );
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn flock_short_c_single_quoted() {
    same_or_stricter(
        "flockc-sq",
        "flock -c 'rm -rf /' /tmp/l",
        "flock '-c' 'rm -rf /' /tmp/l",
    );
}

#[test]
#[ignore = "RED fail-open (system harm) measured allow/allow vs unquoted deny/deny at 8d6b3403d"]
fn flock_lockfile_first_command_single_quoted() {
    same_or_stricter(
        "flockl-sq",
        "flock /tmp/l --command='rm -rf /'",
        "flock /tmp/l '--command=rm -rf /'",
    );
}
