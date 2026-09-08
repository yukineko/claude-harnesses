//! End-to-end tests: build a throwaway git repo, stage changes, run the real
//! binary, assert on exit code and stderr. Requires `git` on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn unique_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("pca-test-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(
        status.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&status.stderr)
    );
}

/// Init a repo with one committed file so HEAD exists.
fn init_repo() -> PathBuf {
    let dir = unique_dir();
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "user.name", "Test"]);
    write(&dir, "README.md", "# repo\n");
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

fn write(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, content).unwrap();
}

/// Run the binary in `dir` with mode `precommit` (deterministic exit code 1).
/// Returns (exit_code, stderr).
fn run(dir: &Path) -> (i32, String) {
    // Tests exercise config-driven behavior, so the temp repo's config must be
    // honored: trust every root for these in-process runs (env-scoped to the
    // spawned binary; never touches the real trust list). The untrusted path is
    // covered separately by `untrusted_repo_config_is_ignored`.
    let bin = env!("CARGO_BIN_EXE_precommit-audit");
    let out = Command::new(bin)
        .arg("--mode")
        .arg("precommit")
        .arg("--root")
        .arg(dir)
        .current_dir(dir)
        .env("HARNESS_TRUST_ALL", "1")
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Run the binary in `dir` with mode `stop` and a Claude Code hook payload on
/// stdin carrying `hook_event_name`. Returns (exit_code, stderr). Used to assert
/// the SessionEnd contract: a blocking finding must NOT surface a non-zero exit
/// (SessionEnd can't block; a non-zero exit there is reported as a failed hook).
fn run_stop_event(dir: &Path, event: &str) -> (i32, String) {
    use std::io::Write;
    let bin = env!("CARGO_BIN_EXE_precommit-audit");
    let mut child = Command::new(bin)
        .arg("--mode")
        .arg("stop")
        .arg("--root")
        .arg(dir)
        .current_dir(dir)
        .env("HARNESS_TRUST_ALL", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("binary spawns");
    let payload = format!("{{\"hook_event_name\":\"{event}\"}}");
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Config that turns off external linters so tests stay hermetic & fast.
const NO_LINTERS: &str = "[checks]\nlinters = false\n";

#[test]
fn clean_tree_passes() {
    let dir = init_repo();
    write(&dir, ".precommit-audit.toml", NO_LINTERS);
    let (code, _err) = run(&dir);
    assert_eq!(code, 0, "no staged source changes => pass");
}

#[test]
fn source_without_test_blocks() {
    let dir = init_repo();
    write(&dir, ".precommit-audit.toml", NO_LINTERS);
    write(&dir, "app.py", "def add(a, b):\n    return a + b\n");
    let (code, err) = run(&dir);
    assert_eq!(code, 1, "source without test must block");
    assert!(err.contains("TEST MISSING"), "stderr: {err}");
}

#[test]
fn session_end_blocking_finding_is_advisory_not_a_failed_hook() {
    // The hook now runs on SessionEnd, which cannot block. A blocking finding
    // must therefore exit 0 (advisory) — a non-zero exit would be reported by
    // Claude Code as a failed hook. The finding is still surfaced on stderr.
    let dir = init_repo();
    write(&dir, ".precommit-audit.toml", NO_LINTERS);
    write(&dir, "app.py", "def add(a, b):\n    return a + b\n");
    let (code, err) = run_stop_event(&dir, "SessionEnd");
    assert_eq!(
        code, 0,
        "SessionEnd must never surface a blocking exit; stderr: {err}"
    );
    assert!(
        err.contains("advisory"),
        "advisory wording expected; stderr: {err}"
    );
    assert!(
        err.contains("TEST MISSING"),
        "finding still reported; stderr: {err}"
    );
}

#[test]
fn stop_event_blocking_finding_still_blocks() {
    // The Stop hook contract is unchanged: a blocking finding exits 2 to block.
    let dir = init_repo();
    write(&dir, ".precommit-audit.toml", NO_LINTERS);
    write(&dir, "app.py", "def add(a, b):\n    return a + b\n");
    let (code, err) = run_stop_event(&dir, "Stop");
    assert_eq!(code, 2, "Stop must still block with exit 2; stderr: {err}");
    assert!(err.contains("TEST MISSING"), "stderr: {err}");
}

#[test]
fn source_with_test_passes() {
    let dir = init_repo();
    write(&dir, ".precommit-audit.toml", NO_LINTERS);
    write(&dir, "app.py", "def add(a, b):\n    return a + b\n");
    write(
        &dir,
        "tests/test_app.py",
        "def test_add():\n    assert True\n",
    );
    let (code, err) = run(&dir);
    assert_eq!(code, 0, "source + test => pass; stderr: {err}");
}

#[test]
fn hardcoded_secret_blocks() {
    let dir = init_repo();
    write(&dir, ".precommit-audit.toml", NO_LINTERS);
    write(&dir, "conf.py", "password = \"hunter2supersecret\"\n");
    write(
        &dir,
        "tests/test_conf.py",
        "def test_x():\n    assert True\n",
    );
    let (code, err) = run(&dir);
    assert_eq!(code, 1);
    assert!(err.contains("SECRET"), "stderr: {err}");
}

#[test]
fn env_getter_secret_is_allowed() {
    let dir = init_repo();
    write(&dir, ".precommit-audit.toml", NO_LINTERS);
    write(&dir, "conf.py", "password = os.environ[\"DB_PASSWORD\"]\n");
    write(
        &dir,
        "tests/test_conf.py",
        "def test_x():\n    assert True\n",
    );
    let (code, err) = run(&dir);
    assert_eq!(
        code, 0,
        "env getter is not a hard-coded secret; stderr: {err}"
    );
}

#[test]
fn hardcoded_ip_blocks_but_testnet_ok() {
    let dir = init_repo();
    write(&dir, ".precommit-audit.toml", NO_LINTERS);
    // 10.x is a real private addr -> flagged; 192.0.2.x is RFC5737 -> benign.
    write(
        &dir,
        "net.py",
        "HOST = \"10.20.30.40\"\nDOC = \"192.0.2.5\"\n",
    );
    write(
        &dir,
        "tests/test_net.py",
        "def test_x():\n    assert True\n",
    );
    let (code, err) = run(&dir);
    assert_eq!(code, 1);
    assert!(err.contains("HARD-CODED IP"), "stderr: {err}");
    assert!(err.contains("10.20.30.40"), "stderr: {err}");
    assert!(
        !err.contains("192.0.2.5"),
        "test-net must be benign; stderr: {err}"
    );
}

#[test]
fn audit_ignore_suppresses_line() {
    let dir = init_repo();
    write(&dir, ".precommit-audit.toml", NO_LINTERS);
    write(
        &dir,
        "net.py",
        "HOST = \"10.20.30.40\"  # audit-ignore: lab fixture\n",
    );
    write(
        &dir,
        "tests/test_net.py",
        "def test_x():\n    assert True\n",
    );
    let (code, err) = run(&dir);
    assert_eq!(
        code, 0,
        "audit-ignore must suppress the IP hit; stderr: {err}"
    );
}

#[test]
fn swallowed_exception_blocks() {
    let dir = init_repo();
    write(&dir, ".precommit-audit.toml", NO_LINTERS);
    // Bare `except:` is the swallow pattern the check detects (a multi-line
    // `except Exception as e:` + `pass` is intentionally NOT flagged).
    write(&dir, "app.py", "try:\n    do()\nexcept:\n    pass\n");
    write(
        &dir,
        "tests/test_app.py",
        "def test_x():\n    assert True\n",
    );
    let (code, err) = run(&dir);
    assert_eq!(code, 1);
    assert!(
        err.contains("SWALLOWED") || err.contains("FALL-THROUGH"),
        "stderr: {err}"
    );
}

/// The security contract: an auto-discovered project `.precommit-audit.toml`
/// in an UNTRUSTED repo must be ignored — its custom rules / linter knobs carry
/// no weight, so a cloned hostile repo can't steer the audit. Same config as
/// `custom_rule_blocks_with_glob_scope`, but run without `HARNESS_TRUST_ALL`.
#[test]
fn untrusted_repo_config_is_ignored() {
    let dir = init_repo();
    let cfg = r#"
[checks]
linters = false
missing_test = false

[[rule]]
id = "no-todo-fixme"
pattern = 'TODO|FIXME'
include_globs = ["src/**"]
message = "Resolve TODO/FIXME before committing."
"#;
    write(&dir, ".precommit-audit.toml", cfg);
    // A scanned-but-not-source file (.conf): the custom rule would flag its TODO
    // when the config is honored, but no built-in default check applies to it —
    // so when the untrusted config is dropped the audit is clean (exit 0). This
    // isolates the custom rule from the built-in missing-test default.
    write(&dir, "src/a.conf", "key = value  # TODO later\n");

    // Untrusted run: NO HARNESS_TRUST_ALL, so the project config is dropped and
    // the custom rule never fires.
    let bin = env!("CARGO_BIN_EXE_precommit-audit");
    let out = Command::new(bin)
        .arg("--mode")
        .arg("precommit")
        .arg("--root")
        .arg(&dir)
        .current_dir(&dir)
        .output()
        .expect("binary runs");
    let code = out.status.code().unwrap_or(-1);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 0,
        "untrusted custom rule must NOT block; stderr: {err}"
    );
    assert!(
        !err.contains("NO-TODO-FIXME"),
        "custom rule from untrusted config must not fire; stderr: {err}"
    );
    assert!(
        err.contains("not trusted"),
        "should warn the config was ignored; stderr: {err}"
    );
}

#[test]
fn custom_rule_blocks_with_glob_scope() {
    let dir = init_repo();
    let cfg = r#"
[checks]
linters = false
missing_test = false

[[rule]]
id = "no-todo-fixme"
pattern = 'TODO|FIXME'
include_globs = ["src/**"]
message = "Resolve TODO/FIXME before committing."
"#;
    write(&dir, ".precommit-audit.toml", cfg);
    // In scope -> blocks.
    write(&dir, "src/a.py", "x = 1  # TODO later\n");
    let (code, err) = run(&dir);
    assert_eq!(code, 1, "in-scope custom rule must block; stderr: {err}");
    assert!(err.contains("NO-TODO-FIXME"), "stderr: {err}");
}

#[test]
fn custom_rule_glob_excludes_out_of_scope() {
    let dir = init_repo();
    let cfg = r#"
[checks]
linters = false
missing_test = false

[[rule]]
id = "no-todo-fixme"
pattern = 'TODO|FIXME'
include_globs = ["src/**"]
message = "Resolve TODO/FIXME before committing."
"#;
    write(&dir, ".precommit-audit.toml", cfg);
    // Out of scope -> ignored.
    write(&dir, "docs/notes.py", "x = 1  # TODO later\n");
    let (code, _err) = run(&dir);
    assert_eq!(code, 0, "out-of-scope file must not trip the rule");
}

#[test]
fn custom_rule_unless_allowlist() {
    let dir = init_repo();
    let cfg = r#"
[checks]
linters = false
missing_test = false

[[rule]]
id = "noncanonical-env"
pattern = 'load_dotenv'
unless = ['/etc/myapp/app\.env']
message = "canonical env only"
"#;
    write(&dir, ".precommit-audit.toml", cfg);
    write(&dir, "a.py", "load_dotenv(\"/etc/myapp/app.env\")\n");
    let (code, err) = run(&dir);
    assert_eq!(
        code, 0,
        "unless allowlist must exempt the line; stderr: {err}"
    );
}

/// The shared in-tree marker used to be the bypass. It is not one any more.
///
/// `<audit_dir>/.audit-skip` lived inside the working tree, carried no
/// attribution, and was consumed by whichever invocation ran next — so a bypass
/// one session armed was routinely spent on a DIFFERENT session's commit, or on
/// a human's terminal `git commit`. CLAUDE.md §5 forbids exactly that shape. The
/// replacement is `precommit-audit skip --reason "<why>"`, which is scoped to
/// the issuing session, requires a reason, and is recorded at both ends.
///
/// This asserts the removal, not the replacement: a file dropped in the shared
/// tree must no longer buy anyone a pass. The positive path (a session's own
/// skip does allow its own next audit) is covered separately.
#[test]
fn a_shared_in_tree_marker_no_longer_bypasses_the_audit() {
    let dir = init_repo();
    write(&dir, ".precommit-audit.toml", NO_LINTERS);
    write(&dir, "app.py", "def add(a, b):\n    return a + b\n"); // would block
    write(&dir, ".claude/.audit-skip", "emergency");
    let (code, err) = run(&dir);
    assert_ne!(
        code, 0,
        "a file in the SHARED working tree must not wave this audit through — that marker \
         names no session, so it applies to whichever invocation runs next (CLAUDE.md §5). \
         stderr: {err}"
    );
    assert!(
        dir.join(".claude/.audit-skip").exists(),
        "the audit must not even consume the stale marker: deleting it would silently spend \
         a file the operator may still believe is armed"
    );
}

/// ANTI-VACUITY CONTROL for the test above. It asserts a BLOCK, which a gate
/// that blocked unconditionally would satisfy for free. This is the same repo
/// and config with the offending file removed, and it must come out clean — so
/// the block above is attributable to the audit finding something, not to the
/// audit being broken.
#[test]
fn the_same_repo_without_the_offending_file_is_clean() {
    let dir = init_repo();
    write(&dir, ".precommit-audit.toml", NO_LINTERS);
    write(&dir, ".claude/.audit-skip", "emergency");
    let (code, err) = run(&dir);
    assert_eq!(
        code, 0,
        "nothing to find, so nothing to block; stderr: {err}"
    );
}

#[test]
fn line_ending_lf_in_sh_is_fine() {
    let dir = init_repo();
    write(
        &dir,
        ".precommit-audit.toml",
        "[checks]\nlinters = false\nmissing_test = false\n",
    );
    write(&dir, "run.sh", "#!/bin/bash\necho hi\n");
    let (code, err) = run(&dir);
    assert_eq!(code, 0, "LF .sh is correct; stderr: {err}");
}
