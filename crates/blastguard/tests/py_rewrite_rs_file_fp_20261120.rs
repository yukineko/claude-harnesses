#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Possible FP observed 2026-11-20 (subagent whose live hook may be older
//! than main) — independent test author, not the implementer (CLAUDE.md §2(a)).
//!
//! Observed shape: `cd <own linked worktree> && python3 - <<'EOF'` whose body
//! reads a `.rs` test file into a variable, `str.replace`s text in memory and
//! writes it back with `open(p, 'w').write(s)`, followed by
//! `cargo fmt -p blastguard -- --check <that .rs>`. Refusal reported:
//! "`the Python program` writes `a path the Python program does not spell as
//! a literal`, and another part of this command may run that file, so the
//! written text is judged as the shell script it is; the Python program holds
//! a dynamic-execution or process marker (`exec`, `eval`, `subprocess`,
//! `os.system`, …), so blastguard does not read what it writes".
//!
//! Rulings (memory heredoc-body-by-target-language): a python stdin program
//! is judged by the python reading; an fs-write sink is judged on its path by
//! the existing path rules; writing a `.rs` then `cargo test`/`cargo build`
//! is not execution of the body (Allow). `cargo fmt` only reformats; it does
//! not execute the file. A `exec`/`eval` WORD inside string data is not a
//! call. So every intended-Allow case below expects Allow.
//!
//! Fixture: git repo `proj` with a linked worktree
//! `.harness-worktrees/session-pyrs` = session project dir
//! (`CLAUDE_PROJECT_DIR`) = payload cwd; tracked file `crates/blastguard/
//! tests/x.rs`; hermetic env. Main mode and subagent mode. The hook only
//! JUDGES the text; nothing runs.
//!
//! Measured at main 8d6b3403d (blastguard 0.2.790), 2026-11-20:
//!   * the reported write-then-run / dynamic-marker reason does NOT
//!     reproduce; (e) (new untracked `.rs`, `eval` word, cargo fmt) is Allow;
//!   * a..d (overwrite of the TRACKED `.rs`) are Deny in main AND subagent
//!     mode with the whose-tree refusal ("... inside a git work tree, but not
//!     one of this session's trees ..."), while the shell-redirect twin after
//!     the same `cd` is Allow. Same family as the ignored REDs b1..c of
//!     py_stdin_heredoc_verb_word_fp_20261120.rs; pinned as ignored REDs;
//!   * FAIL-OPEN (system harm): `cd <wt> && python3 - <<'EOF'` with
//!     `subprocess.run(["rm", "-rf", "/"])` is Allow in both modes (with and
//!     without the cargo fmt suffix), while the same body without the `cd`
//!     head is Deny. Pinned as ignored REDs.
//!
//! DOES NOT PROVE: which rule produces a decision (only decisions asserted);
//! behaviour of the live (older) hook binary; the exact body the subagent
//! used (only the reported shape is reproduced).

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(home: &Path, dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", home);
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Fx {
    base: PathBuf,
    home: PathBuf,
    wt: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

const REL: &str = "crates/blastguard/tests/x.rs";
const FMT: &str = "cargo fmt -p blastguard -- --check crates/blastguard/tests/x.rs";

fn fixture(tag: &str) -> Fx {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let root = neutral_base::neutral_base().join(format!(
        "py-rs-20261120-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let base = std::fs::canonicalize(&root).unwrap();
    let home = base.join("home");
    let repo = base.join("proj");
    std::fs::create_dir_all(home.join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    std::fs::create_dir_all(repo.join("crates/blastguard/tests")).unwrap();
    git(&home, &repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(REL), "// a\n#[test]\nfn a() {}\n").unwrap();
    git(&home, &repo, &["add", REL]);
    git(&home, &repo, &["commit", "-q", "-m", "init"]);
    std::fs::create_dir_all(base.join(".harness-worktrees")).unwrap();
    let wt = base.join(".harness-worktrees/session-pyrs");
    git(
        &home,
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "session-pyrs",
            wt.to_str().unwrap(),
        ],
    );
    assert!(wt.join(".git").is_file(), "fixture: linked worktree");
    assert!(wt.join(REL).is_file(), "fixture: tracked file in worktree");
    Fx { base, home, wt }
}

/// Run the hook once; `(decision, reason)`. A non-zero exit panics; only then
/// is an empty stdout read as Allow; non-JSON stdout panics.
fn run_as(fx: &Fx, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-rewrite-rs-file-fp-20261120",
        "cwd": fx.wt,
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
        .env("CLAUDE_PROJECT_DIR", &fx.wt)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .current_dir(&fx.wt)
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
        println!("[sub={subagent}] {command:?} => allow");
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("hook stdout for {command:?} is not JSON ({e}): {stdout:?}"));
    let h = &v["hookSpecificOutput"];
    let d = h["permissionDecision"].as_str().unwrap_or("?").to_string();
    let r = h["permissionDecisionReason"]
        .as_str()
        .unwrap_or("")
        .to_string();
    println!("[sub={subagent}] {command:?} => {d} ({r})");
    (d, r)
}

#[track_caller]
fn allow_both(fx: &Fx, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, command, subagent);
        assert_eq!(
            d, "allow",
            "subagent={subagent}: {command:?} expected Allow, got {d} ({r})"
        );
    }
}

#[track_caller]
fn deny_both(fx: &Fx, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, command, subagent);
        assert_eq!(
            d, "deny",
            "subagent={subagent}: {command:?} expected Deny, got {d} ({r})"
        );
    }
}

#[track_caller]
fn not_allow_both(fx: &Fx, command: &str) {
    for subagent in [false, true] {
        let (d, r) = run_as(fx, command, subagent);
        assert_ne!(
            d, "allow",
            "subagent={subagent}: {command:?} must not be Allow ({r})"
        );
    }
}

/// Observed body: read into `s`, replace `old` with `new`, write back via
/// `open(p, 'w').write(s)`.
fn open_body(path: &str, old: &str, new: &str) -> String {
    format!(
        "p = \"{path}\"\ns = open(p).read()\ns = s.replace(\"{old}\", \"{new}\")\n\
open(p, 'w').write(s)\n"
    )
}

/// `cd <wt> && python3 - <<'EOF'` + body + `EOF`, then `tail` on the next line.
fn cmd(fx: &Fx, body: &str, tail: Option<&str>) -> String {
    let mut c = format!("cd {} && python3 - <<'EOF'\n{body}EOF", fx.wt.display());
    if let Some(t) = tail {
        c.push('\n');
        c.push_str(t);
    }
    c
}

// test-weakening-justified: new RED for an observed FP (2026-11-20)
/// (a) observed shape: open().write() of the own-worktree `.rs`, then a
/// `cargo fmt --check` line.
#[test]
#[ignore = "RED FP: python3 - heredoc overwrite of a tracked .rs in the session's own linked worktree gets the whose-tree Deny; shell redirect twin is Allow"]
fn a_open_write_rs_then_cargo_fmt_is_allow() {
    let fx = fixture("a");
    allow_both(&fx, &cmd(&fx, &open_body(REL, "a", "b"), Some(FMT)));
}

// test-weakening-justified: new RED for an observed FP (2026-11-20)
/// (a') same, `cargo fmt` chained with `&&` on the heredoc line.
#[test]
#[ignore = "RED FP: python3 - heredoc overwrite of a tracked .rs in the session's own linked worktree gets the whose-tree Deny; shell redirect twin is Allow"]
fn a2_open_write_rs_and_and_cargo_fmt_is_allow() {
    let fx = fixture("a2");
    let c = format!(
        "cd {} && python3 - <<'EOF' && {FMT}\n{}EOF",
        fx.wt.display(),
        open_body(REL, "a", "b")
    );
    allow_both(&fx, &c);
}

// test-weakening-justified: new RED for an observed FP (2026-11-20)
/// (b) same body without the cargo fmt suffix.
#[test]
#[ignore = "RED FP: python3 - heredoc overwrite of a tracked .rs in the session's own linked worktree gets the whose-tree Deny; shell redirect twin is Allow"]
fn b_open_write_rs_without_cargo_fmt_is_allow() {
    let fx = fixture("b");
    allow_both(&fx, &cmd(&fx, &open_body(REL, "a", "b"), None));
}

// test-weakening-justified: new RED for an observed FP (2026-11-20)
/// (c1) (a) where the replaced string literals contain the word `eval`.
#[test]
#[ignore = "RED FP: python3 - heredoc overwrite of a tracked .rs in the session's own linked worktree gets the whose-tree Deny; shell redirect twin is Allow"]
fn c1_eval_word_in_replaced_literal_is_allow() {
    let fx = fixture("c1");
    allow_both(
        &fx,
        &cmd(
            &fx,
            &open_body(REL, "fn eval_old()", "fn eval_new()"),
            Some(FMT),
        ),
    );
}

// test-weakening-justified: new RED for an observed FP (2026-11-20)
/// (c2) (a) where the replaced string literals contain the word `exec`.
#[test]
#[ignore = "RED FP: python3 - heredoc overwrite of a tracked .rs in the session's own linked worktree gets the whose-tree Deny; shell redirect twin is Allow"]
fn c2_exec_word_in_replaced_literal_is_allow() {
    let fx = fixture("c2");
    allow_both(
        &fx,
        &cmd(
            &fx,
            &open_body(REL, "exec_old", "Command::new('exec')"),
            Some(FMT),
        ),
    );
}

// test-weakening-justified: new RED for an observed FP (2026-11-20)
/// (d) (a) using `Path(p).write_text(s)`.
#[test]
#[ignore = "RED FP: python3 - heredoc overwrite of a tracked .rs in the session's own linked worktree gets the whose-tree Deny; shell redirect twin is Allow"]
fn d_path_write_text_rs_then_cargo_fmt_is_allow() {
    let fx = fixture("d");
    let body = format!(
        "from pathlib import Path\np = \"{REL}\"\ns = open(p).read()\n\
s = s.replace(\"a\", \"b\")\nPath(p).write_text(s)\n"
    );
    allow_both(&fx, &cmd(&fx, &body, Some(FMT)));
}

/// (e) calibration: a NEW (untracked) `.rs` written with an `eval` word in
/// the literal, then cargo fmt — no overwrite of tracked content (measured
/// Allow at 8d6b3403d; the marker word plus cargo fmt alone does not produce
/// the observed write-then-run reason at main).
#[test]
fn e_new_rs_with_eval_word_then_cargo_fmt_is_allow() {
    let fx = fixture("e");
    let body =
        "p = \"crates/blastguard/tests/new.rs\"\ns = \"fn eval_x() {}\"\nopen(p, 'w').write(s)\n";
    allow_both(&fx, &cmd(&fx, body, Some(FMT)));
}

/// Control (fixture calibration): the same overwrite spelled as a shell
/// redirect after the same `cd`.
#[test]
fn control_shell_redirect_overwrite_in_own_worktree_is_allow() {
    let fx = fixture("ctl0");
    allow_both(&fx, &format!("cd {} && echo x > {REL}", fx.wt.display()));
}

/// Control: same body writing `run.sh`, then `bash run.sh` runs it.
#[test]
fn control_write_run_sh_then_bash_is_not_allow() {
    let fx = fixture("ctl1");
    not_allow_both(
        &fx,
        &cmd(&fx, &open_body("run.sh", "a", "b"), Some("bash run.sh")),
    );
}

const RM_ROOT: &str = "import subprocess\nsubprocess.run([\"rm\", \"-rf\", \"/\"])\n";

/// Control calibration: the destructive body WITHOUT the `cd` head is Deny.
#[test]
fn control_subprocess_rm_rf_root_without_cd_is_deny() {
    let fx = fixture("ctl2a");
    deny_both(&fx, &format!("python3 - <<'EOF'\n{RM_ROOT}EOF"));
}

// test-weakening-justified: new RED for an observed fail-open (2026-11-20)
/// Control: body really spawns a destructive process, in the (a) shape
/// (`cd <wt> && python3 - <<'EOF'` … `EOF` + cargo fmt).
#[test]
#[ignore = "RED fail-open (system harm): `cd <dir> && python3 - <<'EOF'` with subprocess.run([rm,-rf,/]) is Allow; the same body without the cd head is Deny"]
fn control_subprocess_rm_rf_root_is_deny() {
    let fx = fixture("ctl2");
    deny_both(&fx, &cmd(&fx, RM_ROOT, Some(FMT)));
}

// test-weakening-justified: new RED for an observed fail-open (2026-11-20)
/// Same fail-open, minimal: `cd <wt> && python3 - <<'EOF'` + body, no suffix.
#[test]
#[ignore = "RED fail-open (system harm): `cd <dir> && python3 - <<'EOF'` with subprocess.run([rm,-rf,/]) is Allow; the same body without the cd head is Deny"]
fn control_cd_then_subprocess_rm_rf_root_is_deny() {
    let fx = fixture("ctl2b");
    deny_both(&fx, &cmd(&fx, RM_ROOT, None));
}
