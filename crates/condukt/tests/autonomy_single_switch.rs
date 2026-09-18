//! Black-box tests for the proposed SHARED autonomy switch (backlog 9976b845).
//!
//! Today condukt/ctxrot/autoflow each own an independent autonomy signal (see
//! `crates/condukt/tests/autonomy_invariant.rs` for condukt's existing
//! config+env contract). This file drives the NEW contract:
//!
//!   - a durable switch file at `<base>/<project-key>.json`
//!     (`base` = `$HARNESS_AUTONOMY_DIR` or `$HOME/.harness/autonomy`,
//!     `project-key` = `harness_core::projkey::project_key` of the repo's MAIN
//!     worktree root, resolved via `harness_core::projkey::main_worktree_root`),
//!     written via `condukt state autonomy-set on|off`;
//!   - resolution order env(`HARNESS_AUTONOMOUS`) > env(`CONDUKT_AUTONOMOUS`)
//!     > switch file > config.toml `autonomous` > default off;
//!   - fail-closed (CLAUDE.md §3): a switch file that exists but cannot be
//!     parsed is `Undetermined`, which resolves to OFF and is named as such
//!     (`undetermined-switch-file`) — NOT indistinguishable from a plain "off"
//!     (`default`/`config`), and NOT silently "autonomous" either;
//!   - cross-worktree visibility (CLAUDE.md §8): a switch set from the MAIN
//!     repo tree must be visible from a linked worktree of the same repo.
//!
//! REVISED CONTRACT (per the second Claude session's ruling, after I flagged
//! that a naive `"source"`-on-stdout addition would redden the FROZEN oracle
//! in `crates/condukt/tests/autonomy_invariant.rs:112` et al.):
//!
//!   - `condukt state autonomy-check` — stdout stays BYTE-IDENTICAL to today:
//!     exactly `{"autonomous":true}` / `{"autonomous":false}`, exit 0/1. No
//!     new field. This file never asserts a `source` field on this command.
//!   - `condukt state autonomy-check --explain` — NEW flag. Prints
//!     `{"autonomous":<bool>,"source":"<layer>"}`, same exit-code contract.
//!     `source` is asserted ONLY here.
//!   - `condukt state autonomy-path` — prints `{"path":"<absolute
//!     path>","source":"<layer>"}`, exit 0.
//!   - `condukt state autonomy-set on|off` — unchanged from the original
//!     brief: writes the switch file atomically, exit 0.
//!   - A4 (corrupt switch file) and A5 (absent switch file) must stay
//!     distinguishable WITHOUT a stdout field: the load-bearing distinction is
//!     STDERR. A4 (corrupt) must print a visible warning naming the unreadable
//!     file; A5 (absent) must stay silent on stderr — nothing is wrong, the
//!     switch was simply never set. Collapsing "broken" and "never set" onto
//!     the same silent stdout would be exactly the CLAUDE.md §1 fail-open this
//!     ticket exists to avoid.
//!   - A10 (new): the frozen contract is not widened. Plain `autonomy-check`
//!     in the autonomous case must print EXACTLY `{"autonomous":true}` and
//!     nothing else — no `source`, no extra whitespace. This is the regression
//!     guard against a future "helpful" field addition reddening
//!     `autonomy_invariant.rs:112`.
//!
//! None of this exists yet as of this writing. `condukt state
//! autonomy-set`/`autonomy-path`/`--explain` are not registered, and plain
//! `autonomy-check` carries no switch-file awareness. Every test below is
//! expected to fail against today's binary; that failure is the point. Each
//! failure is labeled below (in the accompanying report) as either
//! "arg-parse / subcommand missing" (weak RED) or "behavioural assertion"
//! (real RED) — do not conflate the two.
//!
//! I did not implement anything here. I only wrote tests and ran them.

use std::path::{Path, PathBuf};
use std::process::Command;

fn condukt_bin() -> &'static str {
    env!("CARGO_BIN_EXE_condukt")
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// A fully isolated sandbox: (tempdir guard, HOME dir, autonomy-dir, repo cwd).
/// `home` and `autonomy_dir` are both fresh per-test dirs so a test can never
/// read or write the developer's real `~/.condukt` / `~/.harness`. `cwd` is an
/// initialized git repo (so `project_key`/`main_worktree_root`'s repo-root walk
/// lands somewhere real) with one commit, matching how condukt is actually
/// invoked.
struct Sandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    autonomy_dir: PathBuf,
    cwd: PathBuf,
}

fn init_git_repo(dir: &Path) {
    let run = |args: &[&str]| {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test.invalid")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test.invalid")
            .output()
            .expect("run git");
        assert!(
            st.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&st.stderr)
        );
    };
    run(&["init", "-q"]);
    std::fs::write(dir.join("README.md"), "sandbox\n").unwrap();
    run(&["add", "README.md"]);
    run(&["commit", "-q", "-m", "init"]);
}

fn sandbox() -> Sandbox {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let autonomy_dir = tmp.path().join("autonomy_dir");
    let cwd = tmp.path().join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&autonomy_dir).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    init_git_repo(&cwd);
    Sandbox {
        _tmp: tmp,
        home,
        autonomy_dir,
        cwd,
    }
}

/// Run a `condukt` subcommand fully sandboxed: HOME + HARNESS_AUTONOMY_DIR
/// pinned to the sandbox, CONDUKT_AUTONOMOUS/HARNESS_AUTONOMOUS scrubbed
/// unless explicitly passed in `env`, cwd set to the sandboxed repo (or an
/// override).
fn run_in(sb: &Sandbox, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new(condukt_bin());
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", &sb.home)
        .env("HARNESS_AUTONOMY_DIR", &sb.autonomy_dir)
        .env_remove("CONDUKT_AUTONOMOUS")
        .env_remove("HARNESS_AUTONOMOUS");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run condukt");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    }
}

fn run(sb: &Sandbox, args: &[&str], env: &[(&str, &str)]) -> Run {
    let cwd = sb.cwd.clone();
    run_in(sb, &cwd, args, env)
}

fn autonomy_set(sb: &Sandbox, mode: &str) -> Run {
    run(sb, &["state", "autonomy-set", mode], &[])
}

fn autonomy_check(sb: &Sandbox, env: &[(&str, &str)]) -> Run {
    run(sb, &["state", "autonomy-check"], env)
}

fn autonomy_check_explain(sb: &Sandbox, env: &[(&str, &str)]) -> Run {
    run(sb, &["state", "autonomy-check", "--explain"], env)
}

fn json_value(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|e| panic!("not valid JSON: {e}; got={s:?}"))
}

fn source_field(json: &str) -> Option<String> {
    json_value(json)
        .get("source")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}

/// Resolve the switch-file path via `condukt state autonomy-path`, which
/// prints `{"path":"<absolute path>","source":"<layer>"}`.
fn autonomy_path(sb: &Sandbox) -> PathBuf {
    let r = run(sb, &["state", "autonomy-path"], &[]);
    assert_eq!(
        r.code, 0,
        "`condukt state autonomy-path` must exit 0; stdout={:?} stderr={:?}",
        r.stdout, r.stderr
    );
    let v = json_value(&r.stdout);
    let path = v
        .get("path")
        .and_then(|p| p.as_str())
        .unwrap_or_else(|| panic!("autonomy-path JSON missing \"path\": {:?}", r.stdout));
    PathBuf::from(path)
}

// ---------------------------------------------------------------------------
// A1 — switch-set(on) is observed: plain contract unchanged, --explain names it
// ---------------------------------------------------------------------------
#[test]
fn a1_switch_file_on_makes_autonomy_check_pass_with_switch_file_source() {
    let sb = sandbox();
    let set = autonomy_set(&sb, "on");
    assert_eq!(
        set.code, 0,
        "A1: `autonomy-set on` must exit 0; stdout={:?} stderr={:?}",
        set.stdout, set.stderr
    );

    let plain = autonomy_check(&sb, &[]);
    assert_eq!(
        plain.code, 0,
        "A1: with the switch file ON and no env/config override, plain \
         `autonomy-check` must exit 0 (autonomous); stdout={:?} stderr={:?}",
        plain.stdout, plain.stderr
    );
    assert_eq!(
        plain.stdout, r#"{"autonomous":true}"#,
        "A1: plain `autonomy-check` stdout must stay byte-identical to the \
         frozen contract (no `source` field here); got {:?}",
        plain.stdout
    );

    let explain = autonomy_check_explain(&sb, &[]);
    assert_eq!(
        explain.code, 0,
        "A1: `autonomy-check --explain` must also exit 0; stdout={:?} \
         stderr={:?}",
        explain.stdout, explain.stderr
    );
    assert_eq!(
        source_field(&explain.stdout).as_deref(),
        Some("switch-file"),
        "A1: `--explain` JSON `source` must be \"switch-file\" when the \
         durable switch decided the outcome; got stdout={:?}",
        explain.stdout
    );
}

// ---------------------------------------------------------------------------
// A2 — switch-set(off) is observed by autonomy-check
// ---------------------------------------------------------------------------
#[test]
fn a2_switch_file_off_makes_autonomy_check_fail() {
    let sb = sandbox();
    let set = autonomy_set(&sb, "off");
    assert_eq!(
        set.code, 0,
        "A2: `autonomy-set off` must exit 0; stdout={:?} stderr={:?}",
        set.stdout, set.stderr
    );

    let check = autonomy_check(&sb, &[]);
    assert_eq!(
        check.code, 1,
        "A2: switch file OFF must make `autonomy-check` exit 1 (not \
         autonomous); stdout={:?} stderr={:?}",
        check.stdout, check.stderr
    );
    assert_eq!(
        check.stdout, r#"{"autonomous":false}"#,
        "A2: plain `autonomy-check` stdout must stay byte-identical to the \
         frozen contract; got {:?}",
        check.stdout
    );
}

// ---------------------------------------------------------------------------
// A3 — env still wins over the switch file (precedence pin)
// ---------------------------------------------------------------------------
#[test]
fn a3_env_override_beats_switch_file() {
    let sb = sandbox();
    let set = autonomy_set(&sb, "on");
    assert_eq!(set.code, 0, "A3 setup: autonomy-set on must succeed");

    let check = autonomy_check(&sb, &[("CONDUKT_AUTONOMOUS", "0")]);
    assert_eq!(
        check.code, 1,
        "A3: CONDUKT_AUTONOMOUS=0 must beat a switch file that is ON \
         (env outranks the durable switch) — this pins the precedence order \
         so the new layer cannot steal the existing override; \
         stdout={:?} stderr={:?}",
        check.stdout, check.stderr
    );

    let explain = autonomy_check_explain(&sb, &[("CONDUKT_AUTONOMOUS", "0")]);
    assert_eq!(
        source_field(&explain.stdout).as_deref(),
        Some("env"),
        "A3: `--explain` JSON `source` must be \"env\" when the environment \
         decided the outcome; got stdout={:?}",
        explain.stdout
    );
}

// ---------------------------------------------------------------------------
// A4 — FAIL-CLOSED: a corrupt switch file is Undetermined -> OFF, and SAYS so
// (via a visible STDERR warning, since stdout must stay byte-identical)
// ---------------------------------------------------------------------------
#[test]
fn a4_corrupt_switch_file_is_undetermined_and_resolves_off() {
    let sb = sandbox();
    let path = autonomy_path(&sb);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, b"not json at all {{{").expect("write garbage switch file");

    let plain = autonomy_check(&sb, &[]);
    assert_eq!(
        plain.code, 1,
        "A4 (CLAUDE.md §3 fail-closed): a switch file that exists but is \
         unparseable must NOT silently report autonomous; must exit 1; \
         stdout={:?} stderr={:?}",
        plain.stdout, plain.stderr
    );
    assert_eq!(
        plain.stdout, r#"{"autonomous":false}"#,
        "A4: stdout must stay byte-identical to the frozen \"off\" contract \
         (the distinction from A5 lives in stderr, not stdout); got {:?}",
        plain.stdout
    );
    assert!(
        !plain.stderr.is_empty(),
        "A4 (CLAUDE.md §1 — silence that reads as no-problem IS the \
         fail-open): a corrupt switch file must print a VISIBLE stderr \
         warning. A silent exit-1 here is indistinguishable from A5 (switch \
         file never set), which is exactly the collapse this ticket exists \
         to prevent. Got empty stderr."
    );
    assert!(
        plain.stderr.contains(&*path.to_string_lossy()),
        "A4: the stderr warning must name the unreadable switch file ({}); \
         got stderr={:?}",
        path.display(),
        plain.stderr
    );

    let explain = autonomy_check_explain(&sb, &[]);
    assert_eq!(
        source_field(&explain.stdout).as_deref(),
        Some("undetermined-switch-file"),
        "A4: `--explain` JSON `source` must be \"undetermined-switch-file\", \
         distinct from a plain \"off\" (\"default\"/\"config\"); got \
         stdout={:?}",
        explain.stdout
    );
}

// ---------------------------------------------------------------------------
// A5 — an ABSENT switch file is a determinate "never set" -> Off, NOT
// Undetermined, and stays SILENT on stderr (the distinguishing signal vs A4)
// ---------------------------------------------------------------------------
#[test]
fn a5_absent_switch_file_is_default_not_undetermined() {
    let sb = sandbox();
    // Sanity: prove the switch file genuinely does not exist yet.
    let path = autonomy_path(&sb);
    assert!(
        !path.exists(),
        "A5 precondition failed: switch file {} already exists in a fresh \
         sandbox",
        path.display()
    );

    let plain = autonomy_check(&sb, &[]);
    assert_eq!(
        plain.code, 1,
        "A5: with no switch file, no env, and no config, autonomy-check must \
         exit 1 (off by default); stdout={:?} stderr={:?}",
        plain.stdout, plain.stderr
    );
    assert_eq!(
        plain.stdout, r#"{"autonomous":false}"#,
        "A5: stdout must stay byte-identical to the frozen \"off\" contract; \
         got {:?}",
        plain.stdout
    );
    assert!(
        plain.stderr.is_empty(),
        "A5: an ABSENT switch file is not a problem — nothing was ever set — \
         so stderr must be EMPTY. A warning here would make \"never set\" \
         indistinguishable from A4's \"broken\", which defeats the whole \
         point of separating them. Got stderr={:?}",
        plain.stderr
    );

    let explain = autonomy_check_explain(&sb, &[]);
    let source = source_field(&explain.stdout);
    assert!(
        matches!(source.as_deref(), Some("default") | Some("config")),
        "A5: an ABSENT switch file must resolve via \"default\" (or \
         \"config\" if a config.toml value exists) — NOT \
         \"undetermined-switch-file\" (that label is reserved for a file \
         that exists but cannot be read/parsed). Got source={:?} stdout={:?}",
        source,
        explain.stdout
    );
    assert_ne!(
        source.as_deref(),
        Some("undetermined-switch-file"),
        "A5: absent must be distinguishable from corrupt (see A4); both must \
         not collapse onto \"undetermined-switch-file\""
    );
}

// ---------------------------------------------------------------------------
// A9 — cross-worktree visibility (CLAUDE.md §8)
// ---------------------------------------------------------------------------
#[test]
fn a9_switch_set_in_main_tree_is_visible_from_a_linked_worktree() {
    let sb = sandbox();

    // Create a linked worktree of the sandbox repo on a new branch.
    let worktree_dir = sb._tmp.path().join("linked-worktree");
    let add = Command::new("git")
        .args([
            "worktree",
            "add",
            "-q",
            "-b",
            "feature-branch",
            worktree_dir.to_str().unwrap(),
        ])
        .current_dir(&sb.cwd)
        .output()
        .expect("run git worktree add");
    assert!(
        add.status.success(),
        "A9 setup: `git worktree add` failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    // Set the switch ON with cwd = the MAIN repo root.
    let set = run_in(&sb, &sb.cwd, &["state", "autonomy-set", "on"], &[]);
    assert_eq!(
        set.code, 0,
        "A9 setup: autonomy-set on (from main tree) must succeed; \
         stdout={:?} stderr={:?}",
        set.stdout, set.stderr
    );

    // Check with cwd = the LINKED WORKTREE.
    let check = run_in(&sb, &worktree_dir, &["state", "autonomy-check"], &[]);
    assert_eq!(
        check.code, 0,
        "A9 (CLAUDE.md §8): a switch set from the main worktree must be \
         visible from a linked worktree of the SAME repo — a switch a worker \
         in a worktree cannot see is not a switch. Got exit {} stdout={:?} \
         stderr={:?}",
        check.code, check.stdout, check.stderr
    );
}

// ---------------------------------------------------------------------------
// A10 — the frozen contract is NOT widened by this feature
// ---------------------------------------------------------------------------
#[test]
fn a10_plain_autonomy_check_stdout_is_not_widened() {
    let sb = sandbox();
    let set = autonomy_set(&sb, "on");
    assert_eq!(set.code, 0, "A10 setup: autonomy-set on must succeed");

    let plain = autonomy_check(&sb, &[]);
    assert_eq!(
        plain.code, 0,
        "A10: autonomous case must still exit 0; stdout={:?} stderr={:?}",
        plain.stdout, plain.stderr
    );
    assert_eq!(
        plain.stdout, r#"{"autonomous":true}"#,
        "A10: plain `condukt state autonomy-check` (no --explain) must print \
         EXACTLY `{{\"autonomous\":true}}` and nothing more — no `source` \
         field, no extra whitespace. `crates/condukt/tests/autonomy_invariant.rs:112` \
         pins this SAME byte sequence as a frozen oracle \
         (`assert_eq!(out, r#\"{{\"autonomous\":true}}\"#, \"exact JSON \
         contract\")`); a future implementer \"helpfully\" adding `source` \
         to the plain command would silently redden that frozen test. Got \
         {:?}",
        plain.stdout
    );
}
