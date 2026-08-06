//! End-to-end tests for the real built `donegate` binary. donegate is a clap
//! subcommand CLI; its `gate` subcommand is the Stop hook (reads a JSON
//! HookInput from stdin and must ALWAYS exit 0 toward Claude — it blocks via a
//! `decision` field, never via a non-zero exit, so it can't trap a turn).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A fresh isolated HOME + working dir so the gate never touches the real
/// `~/.donegate` and finds no project `donegate.toml` (→ no checks → allow stop).
fn isolated_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("donegate-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run a non-hook subcommand. Returns (exit_code, stdout).
fn run(args: &[&str]) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_donegate");
    let dir = isolated_dir("cli");
    let out = Command::new(bin)
        .args(args)
        .current_dir(&dir)
        .env("HOME", &dir)
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// Run `gate` feeding `payload` on stdin (Stop-hook mode). Returns (code, stdout).
fn run_gate(payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_donegate");
    let dir = isolated_dir("gate");
    let mut child = Command::new(bin)
        .arg("gate")
        .current_dir(&dir)
        .env("HOME", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn help_exits_zero_and_describes_the_gate() {
    let (code, stdout) = run(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("Completion-verification gate"),
        "expected the about string, got: {stdout}"
    );
}

#[test]
fn status_prints_resolved_config() {
    // `status` is read-only: prints the resolved config for the cwd.
    let (code, stdout) = run(&["status"]);
    assert_eq!(code, 0, "status must exit 0");
    assert!(stdout.contains("enabled:"), "got: {stdout}");
    assert!(stdout.contains("checks:"), "got: {stdout}");
}

#[test]
fn gate_with_valid_stop_payload_and_no_checks_allows_stop() {
    // Valid Stop hook input, but the isolated dir has no donegate.toml → no
    // checks → the gate allows the stop and exits 0 (the never-trap invariant).
    let payload = r#"{"hook_event_name":"Stop","session_id":"s1","cwd":"."}"#;
    let (code, _stdout) = run_gate(payload);
    assert_eq!(code, 0, "gate must always exit 0 toward Claude");
}

#[test]
fn gate_with_malformed_stdin_still_exits_zero() {
    // Fail-soft: malformed stdin must never break the turn.
    let (code, _) = run_gate("not json");
    assert_eq!(code, 0, "malformed stdin must still exit 0");
}

#[test]
fn gate_with_empty_stdin_still_exits_zero() {
    let (code, _) = run_gate("");
    assert_eq!(code, 0, "empty stdin must still exit 0");
}

/// Run `gate` feeding `payload` on stdin, with HOME and cwd both set to `dir`
/// (which the caller has already isolated / seeded with a config). Returns
/// (code, stdout).
fn run_gate_in(dir: &std::path::Path, payload: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_donegate");
    let mut child = Command::new(bin)
        .arg("gate")
        .current_dir(dir)
        .env("HOME", dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The Stop-hook contract's most important invariant (main.rs:115): a
/// *required* check that actually fails must still exit 0 toward Claude — the
/// block is carried by the `decision` field in stdout JSON, never by the
/// process exit code. This uses a home-level `~/.donegate/config.toml` (no
/// project trust needed) with one required check that always fails (`false`).
#[test]
fn gate_blocking_stop_still_exits_zero_via_decision_field() {
    let dir = isolated_dir("blocking-stop");
    let home_config = dir.join(".donegate").join("config.toml");
    std::fs::create_dir_all(home_config.parent().unwrap()).unwrap();
    std::fs::write(
        &home_config,
        r#"
[[check]]
name = "always-fails"
cmd = "false"
optional = false
"#,
    )
    .unwrap();

    let payload = r#"{"hook_event_name":"Stop","session_id":"blocking-stop-1","cwd":"."}"#;
    let (code, stdout) = run_gate_in(&dir, payload);

    assert_eq!(
        code, 0,
        "a required check failing must still exit 0 toward Claude; got code {code}, stdout: {stdout}"
    );
    assert!(
        stdout.contains(r#""decision":"block""#),
        "expected a decision:block JSON on stdout, got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// The untrusted-project REFUSAL (backlog 3135ebb9).
//
// Measured defect: donegate rendered "there IS a declared check set and I am
// refusing to run it because this project is untrusted" as `checks: 0`, i.e.
// exactly the same output as "no checks were declared" — and then allowed every
// stop. A refusal to judge was being emitted as a clean verdict (CLAUDE.md 3).
//
// Every test below spawns the REAL compiled binary (production wiring), not an
// internal helper — mirroring `crates/stuckguard/tests/integration.rs`'s
// `unreadable_config_diagnostic_reaches_the_real_binarys_stderr`.
// ---------------------------------------------------------------------------

/// Spawn the real binary with `HOME` and cwd both pinned to `dir`, and with
/// `HARNESS_TRUST_ALL` explicitly REMOVED so a dev machine that happens to
/// export it cannot make these tests pass for the wrong reason. Returns
/// (code, stdout, stderr).
fn run_donegate_untrusted(
    dir: &std::path::Path,
    args: &[&str],
    payload: &str,
) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_donegate");
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env_remove("HARNESS_TRUST_ALL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const ONE_PASSING_CHECK: &str = r#"
[[check]]
name = "ok"
cmd = "true"
"#;

const ONE_FAILING_CHECK: &str = r#"
[[check]]
name = "always-fails"
cmd = "false"
"#;

const STOP_PAYLOAD: &str = r#"{"hook_event_name":"Stop","session_id":"trust-it","cwd":"."}"#;

/// THE DEFECT. A project that HAS declared checks, in a root donegate is not
/// trusted to read, must NOT be allowed to stop: donegate is refusing to run a
/// declared check set, which is a refusal to judge, not a clean verdict.
///
/// The reason must name the config path and the `donegate trust` remedy, so the
/// operator can act on it rather than being told nothing happened.
#[test]
fn untrusted_project_config_blocks_the_stop_instead_of_allowing_it() {
    let dir = isolated_dir("untrusted-blocks");
    std::fs::write(dir.join("donegate.toml"), ONE_PASSING_CHECK).unwrap();

    let (code, stdout, stderr) = run_donegate_untrusted(&dir, &["gate"], STOP_PAYLOAD);

    assert_eq!(
        code, 0,
        "the Stop hook must still exit 0 toward Claude; stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stdout.contains(r#""decision":"block""#),
        "an untrusted project WITH a declared check set is a REFUSAL TO JUDGE and must not allow \
         the stop; got stdout: {stdout:?} stderr: {stderr:?}"
    );
    assert!(
        stdout.contains("donegate.toml"),
        "the refusal must name the config path it is ignoring; got stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("donegate trust"),
        "the refusal must name the remedy so the operator can act; got stdout: {stdout:?}"
    );
}

/// ANTI-VACUITY CONTROL 1. Without this, a change that simply blocks everything
/// would satisfy the test above while destroying the gate.
///
/// A TRUSTED project with a real, passing check must still evaluate normally and
/// must NOT be blocked by the refusal path.
#[test]
fn trusted_project_with_passing_checks_still_allows_the_stop() {
    let dir = isolated_dir("trusted-green");
    std::fs::write(dir.join("donegate.toml"), ONE_PASSING_CHECK).unwrap();
    // Trust this root the way `donegate trust` does — through the real CLI, so
    // the fixture exercises production wiring rather than a hand-written store.
    let (tcode, tout, terr) = run_donegate_untrusted(&dir, &["trust"], "");
    assert_eq!(tcode, 0, "`donegate trust` must succeed: {tout} {terr}");

    let (code, stdout, stderr) = run_donegate_untrusted(&dir, &["gate"], STOP_PAYLOAD);

    assert_eq!(code, 0, "stdout: {stdout} stderr: {stderr}");
    assert!(
        !stdout.contains(r#""decision":"block""#),
        "a trusted project whose required checks all pass must NOT be blocked; got stdout: \
         {stdout:?} stderr: {stderr:?}"
    );
}

/// ANTI-VACUITY CONTROL 2. A trusted project whose required check FAILS must
/// still block for the CHECK's reason, not the trust reason — the refusal path
/// must not swallow or relabel real check failures.
#[test]
fn trusted_project_with_failing_check_blocks_for_the_check_not_for_trust() {
    let dir = isolated_dir("trusted-red");
    std::fs::write(dir.join("donegate.toml"), ONE_FAILING_CHECK).unwrap();
    let (tcode, tout, terr) = run_donegate_untrusted(&dir, &["trust"], "");
    assert_eq!(tcode, 0, "`donegate trust` must succeed: {tout} {terr}");

    let (code, stdout, stderr) = run_donegate_untrusted(&dir, &["gate"], STOP_PAYLOAD);

    assert_eq!(code, 0, "stdout: {stdout} stderr: {stderr}");
    assert!(
        stdout.contains(r#""decision":"block""#),
        "a failing required check must block; got stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("always-fails"),
        "the block must name the failing check; got stdout: {stdout:?}"
    );
    assert!(
        !stdout.contains("donegate trust"),
        "a trusted project must never be told to run `donegate trust`; got stdout: {stdout:?}"
    );
}

/// ANTI-VACUITY CONTROL 3. A project that declared NOTHING (no
/// `donegate.toml`, no `~/.donegate/config.toml`) is a determined observation of
/// ABSENCE, not a refusal, and must keep allowing the stop — installing the hook
/// may never trap a project that never opted in.
#[test]
fn project_with_no_config_at_all_still_allows_the_stop() {
    let dir = isolated_dir("no-config");
    let (code, stdout, stderr) = run_donegate_untrusted(&dir, &["gate"], STOP_PAYLOAD);
    assert_eq!(code, 0, "stdout: {stdout} stderr: {stderr}");
    assert!(
        !stdout.contains(r#""decision":"block""#),
        "a project with no declared checks must not be blocked; got stdout: {stdout:?} stderr: \
         {stderr:?}"
    );
}

/// The operator-facing `status` view must not render the refusal as
/// "checks: 0 … the gate will allow every stop" — that sentence is a false
/// statement about what the gate is going to do.
#[test]
fn status_does_not_render_an_untrusted_refusal_as_no_checks() {
    let dir = isolated_dir("untrusted-status");
    std::fs::write(dir.join("donegate.toml"), ONE_PASSING_CHECK).unwrap();

    let (code, stdout, stderr) = run_donegate_untrusted(&dir, &["status"], "");
    assert_eq!(code, 0, "status must exit 0; stderr: {stderr}");
    assert!(
        !stdout.contains("the gate will allow every stop"),
        "status must not promise to allow every stop while donegate is refusing to run a declared \
         check set; got stdout:\n{stdout}"
    );
    assert!(
        stdout.to_lowercase().contains("block"),
        "status must tell the operator the gate is going to BLOCK; got stdout:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// The ordering hazard: worktree trust inheritance.
//
// Blocking on an untrusted declared check set is only shippable if there is a
// path by which the ~90 existing session worktrees are trusted. Trust is
// resolved by exact canonical path, so a linked worktree of an ALREADY-TRUSTED
// repository was untrusted even though its contents are that repository's.
// ---------------------------------------------------------------------------

fn git(cwd: &std::path::Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build `<base>/main` as a real repo with one commit, plus a linked worktree at
/// `<base>/wt`, each carrying `donegate.toml` with one passing check. Returns
/// (main_root, worktree_root), or `None` if git is unavailable.
fn repo_with_linked_worktree(base: &std::path::Path) -> Option<(PathBuf, PathBuf)> {
    if !git_available() {
        return None;
    }
    let main = base.join("main");
    std::fs::create_dir_all(&main).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@t.com"][..],
        &["config", "user.name", "t"][..],
    ] {
        assert!(git(&main, args), "git {args:?} must succeed");
    }
    std::fs::write(main.join("donegate.toml"), ONE_PASSING_CHECK).unwrap();
    assert!(git(&main, &["add", "-A"]));
    assert!(git(&main, &["commit", "-qm", "seed"]));

    let wt = base.join("wt");
    assert!(git(
        &main,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "HEAD"]
    ));
    assert!(
        wt.join("donegate.toml").exists(),
        "the worktree must carry the committed donegate.toml"
    );
    // Apparatus: this really is a LINKED worktree (a `.git` FILE, not a dir).
    assert!(
        wt.join(".git").is_file(),
        "apparatus: a linked worktree has a .git file"
    );
    Some((main, wt))
}

/// A linked worktree of a TRUSTED repository inherits that repository's trust:
/// its committed `donegate.toml` is honored, so a passing check set lets the
/// stop through instead of hitting the refusal.
#[test]
fn worktree_of_a_trusted_repo_inherits_trust() {
    let base = isolated_dir("wt-inherit");
    let Some((main, wt)) = repo_with_linked_worktree(&base) else {
        eprintln!("SKIPPED worktree_of_a_trusted_repo_inherits_trust: git unavailable");
        return;
    };

    // HOME must be a *stable* dir shared by both invocations (that is where
    // ~/.harness/trust.toml lives), and it must not be inside either checkout.
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let with_home = |dir: &std::path::Path, args: &[&str], payload: &str| {
        let bin = env!("CARGO_BIN_EXE_donegate");
        let mut child = Command::new(bin)
            .args(args)
            .current_dir(dir)
            .env("HOME", &home)
            .env_remove("HARNESS_TRUST_ALL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("binary spawns");
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(payload.as_bytes());
        }
        let out = child.wait_with_output().expect("binary runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    // CONTROL: before the main repo is trusted, the worktree must be refused —
    // otherwise a pass below could be for the wrong reason.
    let (_, before, _) = with_home(&wt, &["gate"], STOP_PAYLOAD);
    assert!(
        before.contains(r#""decision":"block""#),
        "apparatus: with NOTHING trusted, the worktree's declared checks must be refused; got: \
         {before:?}"
    );

    // Trust the MAIN checkout only.
    let (tc, to, te) = with_home(&main, &["trust"], "");
    assert_eq!(tc, 0, "`donegate trust` in the main checkout: {to} {te}");

    let (code, stdout, stderr) = with_home(&wt, &["gate"], STOP_PAYLOAD);
    assert_eq!(code, 0, "stdout: {stdout} stderr: {stderr}");
    assert!(
        !stdout.contains(r#""decision":"block""#),
        "a linked worktree of a TRUSTED repo must inherit that trust and run its checks; got \
         stdout: {stdout:?} stderr: {stderr:?}"
    );
}

/// ANTI-VACUITY CONTROL for the inheritance: a worktree of an UNTRUSTED repo
/// must still be refused. Inheritance must not degrade into "every worktree is
/// trusted".
#[test]
fn worktree_of_an_untrusted_repo_is_still_refused() {
    let base = isolated_dir("wt-no-inherit");
    let Some((_main, wt)) = repo_with_linked_worktree(&base) else {
        eprintln!("SKIPPED worktree_of_an_untrusted_repo_is_still_refused: git unavailable");
        return;
    };
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let bin = env!("CARGO_BIN_EXE_donegate");
    let mut child = Command::new(bin)
        .arg("gate")
        .current_dir(&wt)
        .env("HOME", &home)
        .env_remove("HARNESS_TRUST_ALL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(STOP_PAYLOAD.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(r#""decision":"block""#),
        "a worktree whose parent repo is NOT trusted must still be refused; got stdout: {stdout:?}"
    );
}
