#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Black-box tests for `webqa gate` (Stop hook), written by an independent
//! test author (CLAUDE.md §2(a)).
//!
//! Contract under test:
//!   ALLOW = exit 0 and empty stdout.
//!   BLOCK = exit 0 and stdout JSON with "decision":"block" and a "reason".
//!   Every cannot-determine path (bad config, git failure, no spec for a
//!   touched web file, runner crash, inconsistent/absent results, flaky)
//!   must BLOCK. Only the bounded give-up after repeated blocks in the same
//!   session may allow, and then only with a loud systemMessage.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    state: PathBuf,
    stub: PathBuf,
    argv_file: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn unique_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = std::env::temp_dir().join(format!(
        "webqa-gate-{}-{}-{}-{}",
        tag,
        std::process::id(),
        n,
        nanos
    ));
    std::fs::create_dir_all(&p).expect("create temp dir");
    p
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&p, content).expect("write file");
}

fn playwright_json(expected: u32, unexpected: u32, flaky: u32, skipped: u32) -> String {
    format!(
        "{{\"config\":{{}},\"suites\":[],\"errors\":[],\"stats\":{{\"expected\":{expected},\"unexpected\":{unexpected},\"flaky\":{flaky},\"skipped\":{skipped},\"duration\":1.0}}}}"
    )
}

/// Base fixture: a fresh git repo at `<base>/repo` with an initial commit
/// containing `README.md`, `src/login/form.ts`, `src/cart/cart.ts`,
/// `e2e/login.spec.ts`, `e2e/cart.spec.ts`. No `webqa.toml` yet.
fn fixture(tag: &str) -> Fixture {
    let base = unique_dir(tag);
    let root = base.join("repo");
    let state = base.join("state");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    std::fs::create_dir_all(&state).expect("mkdir state");
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "t@example.com"]);
    git(&root, &["config", "user.name", "t"]);
    write(&root, "README.md", "readme\n");
    write(&root, "src/login/form.ts", "export const a = 1;\n");
    write(&root, "src/cart/cart.ts", "export const c = 1;\n");
    write(
        &root,
        "e2e/login.spec.ts",
        "import { test } from '@playwright/test';\ntest('login', async () => {});\n",
    );
    write(
        &root,
        "e2e/cart.spec.ts",
        "import { test } from '@playwright/test';\ntest('cart', async () => {});\n",
    );
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "--no-verify", "-m", "init"]);
    Fixture {
        stub: base.join("stub.sh"),
        argv_file: base.join("argv.txt"),
        base,
        root,
        state,
    }
}

impl Fixture {
    /// Write a stub runner that records its argv (one per line) to
    /// `argv_file`, prints `stdout_text`, and exits with `code`.
    fn stub(&self, stdout_text: &str, code: i32) {
        let script = format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\ncat <<'WEBQA_EOF'\n{}\nWEBQA_EOF\nexit {}\n",
            self.argv_file.display(),
            stdout_text,
            code
        );
        std::fs::write(&self.stub, script).expect("write stub");
        let st = Command::new("chmod")
            .arg("755")
            .arg(&self.stub)
            .status()
            .expect("chmod");
        assert!(st.success());
    }

    /// Write and commit a standard webqa.toml mapping login and cart.
    fn standard_config(&self) {
        self.config_with_command(&format!("[\"{}\"]", self.stub.display()));
    }

    fn config_with_command(&self, command_toml: &str) {
        let toml = format!(
            "command = {command_toml}\n\n[[map]]\nsources = [\"src/login/**\"]\nspecs = [\"e2e/login.spec.ts\"]\n\n[[map]]\nsources = [\"src/cart/**\"]\nspecs = [\"e2e/cart.spec.ts\"]\n"
        );
        self.commit_config(&toml);
    }

    fn commit_config(&self, toml: &str) {
        write(&self.root, "webqa.toml", toml);
        git(&self.root, &["add", "webqa.toml"]);
        git(&self.root, &["commit", "-q", "--no-verify", "-m", "cfg"]);
    }

    fn touch_login(&self) {
        write(&self.root, "src/login/form.ts", "export const a = 2;\n");
    }

    fn stub_invoked(&self) -> bool {
        self.argv_file.exists()
    }

    fn argv(&self) -> String {
        std::fs::read_to_string(&self.argv_file).unwrap_or_default()
    }

    fn gate(&self, session: &str, active: bool) -> Output {
        self.gate_in(&self.root, session, active)
    }

    fn gate_in(&self, cwd: &Path, session: &str, active: bool) -> Output {
        let input = format!(
            "{{\"session_id\":\"{}\",\"stop_hook_active\":{},\"cwd\":\"{}\",\"hook_event_name\":\"Stop\"}}",
            session,
            active,
            cwd.display()
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_webqa"))
            .arg("gate")
            .current_dir(cwd)
            .env("WEBQA_STATE_DIR", &self.state)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn webqa gate");
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().expect("stdin");
            stdin.write_all(input.as_bytes()).expect("write stdin");
        }
        child.wait_with_output().expect("wait webqa gate")
    }
}

/// Records trust for `<root>/webqa.toml` (run with cwd = project root).
fn trust(root: &Path, state: &Path) {
    let out = Command::new(env!("CARGO_BIN_EXE_webqa"))
        .arg("trust")
        .current_dir(root)
        .env("WEBQA_STATE_DIR", state)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("spawn webqa trust");
    assert!(
        out.status.success(),
        "webqa trust failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn describe(out: &Output) -> String {
    format!(
        "status={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn assert_allow(out: &Output) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit 0\n{}",
        describe(out)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "expected ALLOW (empty stdout)\n{}",
        describe(out)
    );
}

/// Asserts BLOCK and returns the lowercased stdout for reason checks.
fn assert_block(out: &Output) -> String {
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit 0\n{}",
        describe(out)
    );
    let so = String::from_utf8_lossy(&out.stdout).into_owned();
    let c = compact(&so);
    assert!(
        c.contains("\"decision\":\"block\""),
        "expected BLOCK (\"decision\":\"block\")\n{}",
        describe(out)
    );
    assert!(
        c.contains("\"reason\":\""),
        "expected a \"reason\" string\n{}",
        describe(out)
    );
    so.to_lowercase()
}

// ------------------------------------------------------------ configuration

#[test]
fn no_config_allows_and_says_not_configured() {
    let f = fixture("noconf");
    f.touch_login();
    let out = f.gate("s-noconf", false);
    assert_allow(&out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not configured"),
        "expected stderr to mention `not configured`\n{}",
        describe(&out)
    );
}

#[test]
fn unparseable_config_blocks() {
    let f = fixture("badconf");
    f.commit_config("command = [ this is not toml\n[[map\n");
    f.touch_login();
    let out = f.gate("s-badconf", false);
    assert_block(&out);
}

#[test]
fn root_not_a_git_repo_blocks() {
    let f = fixture("nogit");
    f.stub(&playwright_json(2, 0, 0, 0), 0);
    let plain = f.base.join("plain");
    std::fs::create_dir_all(plain.join("src/login")).unwrap();
    std::fs::create_dir_all(plain.join("e2e")).unwrap();
    std::fs::write(plain.join("src/login/form.ts"), "x\n").unwrap();
    std::fs::write(plain.join("e2e/login.spec.ts"), "x\n").unwrap();
    std::fs::write(
        plain.join("webqa.toml"),
        format!(
            "command = [\"{}\"]\n\n[[map]]\nsources = [\"src/login/**\"]\nspecs = [\"e2e/login.spec.ts\"]\n",
            f.stub.display()
        ),
    )
    .unwrap();
    trust(&plain, &f.state);
    let out = f.gate_in(&plain, "s-nogit", false);
    assert_block(&out);
}

// ------------------------------------------------------------ selection

#[test]
fn no_touched_web_files_allows_without_running_stub() {
    let f = fixture("noweb");
    f.stub(&playwright_json(2, 0, 0, 0), 0);
    f.standard_config();
    trust(&f.root, &f.state);
    write(&f.root, "README.md", "changed\n");
    let out = f.gate("s-noweb", false);
    assert_allow(&out);
    assert!(
        !f.stub_invoked(),
        "stub must not run when no web file is touched; argv={:?}",
        f.argv()
    );
}

#[test]
fn touched_source_with_empty_spec_list_blocks_undetermined() {
    let f = fixture("emptyspecs");
    f.stub(&playwright_json(2, 0, 0, 0), 0);
    f.commit_config(&format!(
        "command = [\"{}\"]\n\n[[map]]\nsources = [\"src/login/**\"]\nspecs = []\n",
        f.stub.display()
    ));
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-emptyspecs", false);
    let so = assert_block(&out);
    assert!(so.contains("undetermined"), "{}", describe(&out));
}

#[test]
fn touched_source_whose_mapped_spec_is_missing_blocks_undetermined() {
    let f = fixture("missingspec");
    f.stub(&playwright_json(2, 0, 0, 0), 0);
    f.commit_config(&format!(
        "command = [\"{}\"]\n\n[[map]]\nsources = [\"src/login/**\"]\nspecs = [\"e2e/does-not-exist.spec.ts\"]\n",
        f.stub.display()
    ));
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-missingspec", false);
    let so = assert_block(&out);
    assert!(so.contains("undetermined"), "{}", describe(&out));
}

#[test]
fn only_mapped_specs_are_passed_to_runner() {
    let f = fixture("selection");
    f.stub(&playwright_json(2, 0, 0, 0), 0);
    f.standard_config();
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-selection", false);
    assert!(f.stub_invoked(), "stub not invoked\n{}", describe(&out));
    let argv = f.argv();
    assert!(
        argv.contains("e2e/login.spec.ts"),
        "argv missing login spec: {argv:?}\n{}",
        describe(&out)
    );
    assert!(
        !argv.contains("e2e/cart.spec.ts"),
        "argv must not contain unrelated cart spec: {argv:?}"
    );
}

#[test]
fn untracked_new_file_under_mapped_source_counts_as_touched() {
    let f = fixture("untracked");
    f.stub(&playwright_json(2, 0, 0, 0), 0);
    f.standard_config();
    trust(&f.root, &f.state);
    write(&f.root, "src/login/brand-new.ts", "export const n = 1;\n");
    let out = f.gate("s-untracked", false);
    assert!(
        f.stub_invoked(),
        "untracked file under mapped source must trigger the runner\n{}",
        describe(&out)
    );
    assert!(
        f.argv().contains("e2e/login.spec.ts"),
        "argv={:?}",
        f.argv()
    );
}

// ------------------------------------------------------------ runner verdicts

#[test]
fn passing_run_allows() {
    let f = fixture("pass");
    f.stub(&suites_json(&["e2e/login.spec.ts"], 2), 0);
    f.standard_config();
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-pass", false);
    assert!(f.stub_invoked(), "stub not invoked\n{}", describe(&out));
    assert_allow(&out);
}

#[test]
fn failing_run_blocks_with_failed_reason() {
    let f = fixture("fail");
    f.stub(&playwright_json(1, 1, 0, 0), 1);
    f.standard_config();
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-fail", false);
    let so = assert_block(&out);
    assert!(so.contains("failed"), "{}", describe(&out));
}

#[test]
fn exit_zero_with_unexpected_blocks_as_inconsistent() {
    let f = fixture("inc0");
    f.stub(&playwright_json(1, 1, 0, 0), 0);
    f.standard_config();
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-inc0", false);
    assert_block(&out);
}

#[test]
fn exit_one_with_zero_unexpected_blocks() {
    let f = fixture("inc1");
    f.stub(&playwright_json(2, 0, 0, 0), 1);
    f.standard_config();
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-inc1", false);
    assert_block(&out);
}

#[test]
fn flaky_run_blocks_with_flaky_reason() {
    let f = fixture("flaky");
    f.stub(&playwright_json(1, 0, 1, 0), 0);
    f.standard_config();
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-flaky", false);
    let so = assert_block(&out);
    assert!(so.contains("flaky"), "{}", describe(&out));
}

#[test]
fn nothing_ran_blocks() {
    let f = fixture("zero");
    f.stub(&playwright_json(0, 0, 0, 0), 0);
    f.standard_config();
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-zero", false);
    assert_block(&out);
}

#[test]
fn non_json_stdout_blocks() {
    let f = fixture("nonjson");
    f.stub("all good, trust me", 0);
    f.standard_config();
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-nonjson", false);
    assert_block(&out);
}

#[test]
fn missing_command_binary_blocks() {
    let f = fixture("nocmd");
    let missing = f.base.join("no-such-runner-binary");
    assert!(!missing.exists());
    f.config_with_command(&format!("[\"{}\"]", missing.display()));
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-nocmd", false);
    assert_block(&out);
}

// ------------------------------------------------------------ bounded give-up

#[test]
fn bounded_give_up_after_three_blocks_is_loud_and_per_session() {
    let f = fixture("giveup");
    f.stub(&playwright_json(1, 1, 0, 0), 1);
    f.standard_config();
    trust(&f.root, &f.state);
    f.touch_login();

    let o1 = f.gate("s-giveup", false);
    assert_block(&o1);
    let o2 = f.gate("s-giveup", true);
    assert_block(&o2);
    let o3 = f.gate("s-giveup", true);
    assert_block(&o3);

    let o4 = f.gate("s-giveup", true);
    assert_eq!(o4.status.code(), Some(0), "{}", describe(&o4));
    let c4 = compact(&String::from_utf8_lossy(&o4.stdout));
    assert!(
        !c4.contains("\"decision\":\"block\""),
        "4th call should give up (no block)\n{}",
        describe(&o4)
    );
    assert!(
        c4.contains("\"systemMessage\""),
        "give-up must carry a systemMessage\n{}",
        describe(&o4)
    );
    assert!(
        String::from_utf8_lossy(&o4.stdout).contains("NOTHING WAS VERIFIED"),
        "systemMessage must say NOTHING WAS VERIFIED\n{}",
        describe(&o4)
    );

    let other = f.gate("s-giveup-other", true);
    assert_block(&other);
}

// ------------------------------------------------------------ verifier-observed fail-opens

fn suites_json(files: &[&str], expected: u32) -> String {
    let suites: Vec<String> = files
        .iter()
        .map(|f| {
            format!(
                "{{\"title\":\"{f}\",\"file\":\"{f}\",\"specs\":[{{\"title\":\"t\",\"ok\":true,\"tests\":[{{\"status\":\"expected\"}}]}}],\"suites\":[]}}"
            )
        })
        .collect();
    format!(
        "{{\"config\":{{}},\"suites\":[{}],\"errors\":[],\"stats\":{{\"expected\":{expected},\"unexpected\":0,\"flaky\":0,\"skipped\":0,\"duration\":1.0}}}}",
        suites.join(",")
    )
}

fn two_spec_fixture(tag: &str, reported: &[&str]) -> Fixture {
    let f = fixture(tag);
    write(&f.root, "e2e/a.spec.ts", "test('a', async () => {});\n");
    write(&f.root, "e2e/b.spec.ts", "test('b', async () => {});\n");
    git(&f.root, &["add", "-A"]);
    git(&f.root, &["commit", "-q", "--no-verify", "-m", "specs"]);
    f.stub(&suites_json(reported, reported.len() as u32), 0);
    f.commit_config(&format!(
        "command = [\"{}\"]\n\n[[map]]\nsources = [\"src/login/**\"]\nspecs = [\"e2e/a.spec.ts\", \"e2e/b.spec.ts\"]\n",
        f.stub.display()
    ));
    trust(&f.root, &f.state);
    f.touch_login();
    f
}

#[test]
fn g1_selected_spec_missing_from_results_blocks() {
    let f = two_spec_fixture("g1-miss", &["a.spec.ts"]);
    let out = f.gate("s-g1-miss", false);
    assert!(f.stub_invoked(), "stub not invoked\n{}", describe(&out));
    let so = assert_block(&out);
    assert!(
        so.contains("b.spec.ts") || so.contains("not run") || so.contains("missing"),
        "reason must name the unreported spec\n{}",
        describe(&out)
    );
}

#[test]
fn g1_control_all_selected_specs_reported_allows() {
    let f = two_spec_fixture("g1-ok", &["a.spec.ts", "b.spec.ts"]);
    let out = f.gate("s-g1-ok", false);
    assert!(f.stub_invoked(), "stub not invoked\n{}", describe(&out));
    assert_allow(&out);
}

#[test]
fn g1_control_results_with_testdir_prefixed_paths_allow() {
    let f = two_spec_fixture("g1-pfx", &["e2e/a.spec.ts", "e2e/b.spec.ts"]);
    let out = f.gate("s-g1-pfx", false);
    assert_allow(&out);
}

#[test]
fn g2_gate_from_subdirectory_uses_toplevel_config() {
    let f = fixture("g2-sub");
    f.stub(&playwright_json(2, 0, 0, 0), 0);
    f.standard_config();
    trust(&f.root, &f.state);
    f.touch_login();
    let sub = f.root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let out = f.gate_in(&sub, "s-g2-sub", false);
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("not configured"),
        "config at git top level must be found from a subdir\n{}",
        describe(&out)
    );
    assert!(f.stub_invoked(), "stub not invoked\n{}", describe(&out));
}

#[test]
fn g3_timeout_secs_above_hook_budget_blocks() {
    let f = fixture("g3-timeout");
    f.stub(&playwright_json(2, 0, 0, 0), 0);
    f.commit_config(&format!(
        "command = [\"{}\"]\ntimeout_secs = 1000000\n\n[[map]]\nsources = [\"src/login/**\"]\nspecs = [\"e2e/login.spec.ts\"]\n",
        f.stub.display()
    ));
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-g3-timeout", false);
    assert_block(&out);
}

#[test]
fn g4_untrusted_config_blocks_and_does_not_run_command() {
    let f = fixture("g4-untrusted");
    f.stub(&playwright_json(2, 0, 0, 0), 0);
    f.standard_config();
    f.touch_login();
    let out = f.gate("s-g4-untrusted", false);
    let so = assert_block(&out);
    assert!(so.contains("untrusted"), "{}", describe(&out));
    assert!(
        !f.stub_invoked(),
        "untrusted command must NOT execute; argv={:?}",
        f.argv()
    );
}

#[test]
fn g4_trusted_config_runs_command() {
    let f = fixture("g4-trusted");
    f.stub(&suites_json(&["e2e/login.spec.ts"], 2), 0);
    f.standard_config();
    trust(&f.root, &f.state);
    f.touch_login();
    let out = f.gate("s-g4-trusted", false);
    assert!(f.stub_invoked(), "stub not invoked\n{}", describe(&out));
    assert_allow(&out);
}

#[test]
fn g4_config_changed_after_trust_blocks_untrusted() {
    let f = fixture("g4-changed");
    f.stub(&playwright_json(2, 0, 0, 0), 0);
    f.standard_config();
    trust(&f.root, &f.state);
    // Change the config (append a harmless comment) after trust was recorded.
    let p = f.root.join("webqa.toml");
    let mut s = std::fs::read_to_string(&p).unwrap();
    s.push_str("\n# changed after trust\n");
    std::fs::write(&p, s).unwrap();
    f.touch_login();
    let out = f.gate("s-g4-changed", false);
    let so = assert_block(&out);
    assert!(so.contains("untrusted"), "{}", describe(&out));
    assert!(!f.stub_invoked(), "changed config must not execute");
}

#[test]
fn non_git_cwd_without_config_allows_not_configured() {
    let f = fixture("nogit-noconf");
    let plain = f.base.join("plain-noconf");
    std::fs::create_dir_all(&plain).unwrap();
    let out = f.gate_in(&plain, "s-nogit-noconf", false);
    assert_allow(&out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not configured"),
        "expected stderr to mention `not configured`\n{}",
        describe(&out)
    );
}

// ------------------------------------------------------------ round-2 verifier-observed fail-opens

#[test]
fn r3_untrusted_config_cannot_set_its_own_give_up_budget() {
    let f = fixture("r3-untrusted-budget");
    f.stub(&playwright_json(2, 0, 0, 0), 0);
    f.commit_config(&format!(
        "command = [\"{}\"]\nmax_attempts = 1\n\n[[map]]\nsources = [\"src/login/**\"]\nspecs = [\"e2e/login.spec.ts\"]\n",
        f.stub.display()
    ));
    // deliberately NOT trusted
    f.touch_login();
    let o1 = f.gate("s-r3", false);
    assert_block(&o1);
    let o2 = f.gate("s-r3", true);
    assert_block(&o2);
    let o3 = f.gate("s-r3", true);
    assert_block(&o3);
    assert!(
        !f.stub_invoked(),
        "untrusted command must NOT execute; argv={:?}",
        f.argv()
    );
}
