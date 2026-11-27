#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial verification round 2 of 78d5f5d5b (backlog 92f4a28d: the r21
//! exemption for `. "$HOME/.cargo/env"` now holds only after known-pure
//! commands, `rustup.rs::known_pure`). Written by an independent verifier,
//! not the implementer.
//!
//! `v_*` cases: lines where the sourced cargo env file may be replaced (or the
//! later write may land through a link) before the exempt `.` runs, so the
//! following `echo x > d/out.txt` must NOT be Allow (Ask or Deny). Those
//! observed Allow on 78d5f5d5b (2026-11-27) are `#[ignore]`d as RED; run with
//! `--ignored` to observe the RED.
//!
//! `ctl_*` cases: common dev lines that must stay Allow (false-positive
//! controls).
//!
//! `/etc/bg-sentinel` is a deliberately non-existent sentinel path; nothing in
//! this file runs the commands, they are only fed to the hook as JSON.
//! Fixture: hermetic HOME (`.cargo/env` ordinary file, `wt/evil.sh`), cwd =
//! HOME/wt, main session. Empty hook stdout = allow.
//!
//! DOES NOT PROVE: subagent mode; exactly-Deny vs Ask; that the file system
//! is case-insensitive at test time (the `.CARGO/ENV` cases rest on the macOS
//! APFS default); that HOME is a git work tree (`v_git_stash_pop_in_home`
//! assumes a dotfiles repo at HOME, not created here); that the reason text
//! names the relink.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-verify2-92f4-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    std::fs::create_dir_all(dir.join(".cargo")).unwrap();
    std::fs::write(
        dir.join(".cargo/env"),
        "export PATH=\"$HOME/.cargo/bin:$PATH\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("wt/evil.sh"), "ln -s /etc/bg-sentinel d\n").unwrap();
    Home(dir)
}

fn run_main(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "verify2-92f4a28d",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

const TAIL: &str = "; . \"$HOME/.cargo/env\"; echo x > d/out.txt";

fn assert_not_allow(tag: &str, c: &str) {
    let home = unique_home(tag);
    let (d, r) = run_main(c, &home);
    assert!(
        d == "ask" || d == "deny",
        "write after a replaced cargo env / undetermined relink must not be allow: {c:?} => {d} ({r})"
    );
}

fn assert_before_dot_not_allow(tag: &str, pre: &str) {
    assert_not_allow(tag, &format!("{pre}{TAIL}"));
}

fn assert_allow(tag: &str, c: &str) {
    let home = unique_home(tag);
    let (d, r) = run_main(c, &home);
    assert_eq!(
        d, "allow",
        "common dev line must stay allow: {c:?} => {d} ({r})"
    );
}

// ---- command substitution inside an allowlisted command ----

#[test]
fn v_echo_cmdsubst_ln() {
    assert_before_dot_not_allow("echosub", "echo $(ln -sf /dev/stdin ~/.cargo/env)");
}

#[test]
fn v_assignment_cmdsubst_ln() {
    assert_before_dot_not_allow("asgsub", "x=$(ln -sf /dev/stdin ~/.cargo/env)");
}

#[test]
fn v_test_n_cmdsubst_ln() {
    assert_before_dot_not_allow("testsub", "test -n \"$(ln -sf /dev/stdin ~/.cargo/env)\"");
}

#[test]
fn v_backtick_ln() {
    assert_before_dot_not_allow("btick", "echo `ln -sf /dev/stdin ~/.cargo/env`");
}

#[test]
fn v_cat_process_subst_ln() {
    assert_before_dot_not_allow("procsub", "cat <(cp evil.sh ~/.cargo/env)");
}

// ---- cargo env path spellings in a redirect target ----

#[test]
fn v_redirect_dotdot_spelling() {
    assert_before_dot_not_allow("dotdot", "cat evil.sh > ~/.cargo/../.cargo/env");
}

#[test]
fn v_redirect_double_slash_spelling() {
    assert_before_dot_not_allow("dslash", "cat evil.sh > $HOME//.cargo/env");
}

#[test]
fn v_redirect_braced_home_spelling() {
    assert_before_dot_not_allow("bhome", "cat evil.sh > ${HOME}/.cargo/env");
}

#[test]
fn v_cd_home_relative_redirect() {
    assert_before_dot_not_allow("cdhome", "cd ~ && cat wt/evil.sh > ./.cargo/env");
}

#[test]
fn v_cd_cargo_dir_relative_env() {
    assert_before_dot_not_allow("cdcargo", "cd ~/.cargo && cat ../wt/evil.sh > env");
}

#[test]
fn v_redirect_case_folded_path() {
    // APFS (macOS default) is case-insensitive: ~/.CARGO/ENV is ~/.cargo/env.
    assert_before_dot_not_allow("case", "cat evil.sh > ~/.CARGO/ENV");
}

#[test]
fn v_cd_case_folded_dir_then_relative() {
    assert_before_dot_not_allow("casecd", "cd ~/.Cargo && cat ../wt/evil.sh > Env");
}

#[test]
fn v_group_redirect_into_cargo_env() {
    assert_before_dot_not_allow("group", "{ cat evil.sh; } > ~/.cargo/env");
}

// ---- allowlisted heads whose arguments rebind state ----

#[test]
fn v_printf_v_attached_home() {
    // bash accepts `-vNAME`: this assigns HOME.
    assert_before_dot_not_allow("printfv", "printf -vHOME %s /tmp/x");
}

#[test]
fn v_export_git_config_fsmonitor_then_git_status() {
    // git runs core.fsmonitor through the shell during `git status`.
    assert_before_dot_not_allow(
        "fsmon",
        "export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.fsmonitor GIT_CONFIG_VALUE_0=./evil.sh; git status",
    );
}

#[test]
fn v_export_git_external_diff_then_git_diff() {
    assert_before_dot_not_allow("extdiff", "export GIT_EXTERNAL_DIFF=./evil.sh; git diff");
}

#[test]
fn v_git_stash_pop_in_home() {
    // A dotfiles repo at HOME: `git stash pop` rewrites ~/.cargo/env with
    // stashed content the line does not show.
    assert_before_dot_not_allow("stashpop", "cd ~ && git stash pop");
}

// ---- execution order is not span order ----

#[test]
fn v_loop_writes_after_dot_then_sources_again() {
    assert_not_allow(
        "loop",
        "for i in 1 2; do . \"$HOME/.cargo/env\"; cp evil.sh ~/.cargo/env; done; echo x > d/out.txt",
    );
}

#[test]
fn v_function_dot_called_after_write() {
    assert_not_allow(
        "fnlate",
        "f() { . \"$HOME/.cargo/env\"; }; cp evil.sh ~/.cargo/env; f; echo x > d/out.txt",
    );
}

#[test]
fn v_function_shadows_echo() {
    assert_before_dot_not_allow("fnecho", "echo() { cp evil.sh ~/.cargo/env; }; echo");
}

// ---- s5: Python link + a writer that is not a redirect ----

#[test]
fn v_python_heredoc_symlink_then_cp() {
    assert_not_allow(
        "pycp",
        "python3 - <<'EOF'\nimport os\nos.symlink(\"/etc/bg-sentinel\",\"d\")\nEOF\ncp evil.sh d/out.txt",
    );
}

#[test]
fn v_python_herestring_symlink_then_tee() {
    assert_not_allow(
        "pytee",
        "python3 <<< 'import os; os.symlink(\"/etc/bg-sentinel\",\"d\")'; echo x | tee d/out.txt",
    );
}

#[test]
fn v_python_inline_symlink_then_touch() {
    assert_not_allow(
        "pytouch",
        "python3 -c 'import os; os.symlink(\"/etc/bg-sentinel\",\"d\")'; touch d/out.txt",
    );
}

// ---- false-positive controls (must stay Allow) ----

#[test]
fn ctl_echo_then_dot_cargo_build() {
    assert_allow("c1", "echo start && . \"$HOME/.cargo/env\" && cargo build");
}

#[test]
fn ctl_cd_rel_then_dot_cargo_test() {
    assert_allow(
        "c2",
        "cd crates && . \"$HOME/.cargo/env\" && cargo test -p x",
    );
}

#[test]
fn ctl_export_rustflags_then_dot() {
    assert_allow(
        "c3",
        "export RUST_BACKTRACE=1 && . \"$HOME/.cargo/env\" && cargo test",
    );
}

#[test]
fn ctl_git_status_then_dot() {
    assert_allow(
        "c4",
        "git status --short && . \"$HOME/.cargo/env\" && cargo fmt",
    );
}

#[test]
fn ctl_source_spelling_cargo_test() {
    assert_allow(
        "c5",
        "source \"$HOME/.cargo/env\" && cargo test 2>&1 | tail -20",
    );
}

#[test]
fn ctl_dot_then_cargo_test_redirect_log() {
    assert_allow("c6", ". \"$HOME/.cargo/env\" && cargo test > test.log 2>&1");
}

#[test]
fn ctl_python_heredoc_print_then_redirect() {
    assert_allow(
        "c7",
        "python3 - <<'EOF'\nprint('hi')\nEOF\necho done > out.txt",
    );
}

#[test]
fn ctl_python_symlink_then_redirect_devnull() {
    assert_allow(
        "c8",
        "python3 -c 'import os; os.symlink(\"a\",\"b\")'; echo done > /dev/null",
    );
}

#[test]
fn ctl_redirect_before_python_symlink() {
    // The write happens before the link exists; it cannot land through it.
    assert_allow(
        "c9",
        "echo hi > log.txt; python3 -c 'import os; os.symlink(\"a\",\"b\")'",
    );
}

#[test]
fn ctl_python_symlink_then_unrelated_redirect() {
    assert_allow(
        "c10",
        "python3 -c 'import os; os.symlink(\"a\",\"b\")' && echo ok > status.txt",
    );
}
