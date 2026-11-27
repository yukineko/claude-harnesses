#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial verification of d07d986e8 (backlog 92f4a28d: r21 exemption for
//! `. "$HOME/.cargo/env"` dropped after same-line writes / exec redirects /
//! Python relinks). Written by an independent verifier, not the implementer.
//!
//! `v_*` cases: variants of the SAME holes that were observed Allow on
//! d07d986e8 (2026-11-27) although the sourced file or the later write's
//! landing is undetermined. They assert NOT Allow (Ask or Deny) and are
//! `#[ignore]`d as RED until fixed; run with `--ignored` to observe the RED.
//!
//! `ctl_*` cases: common dev lines that must stay Allow (false-positive
//! controls). They are not ignored.
//!
//! `/etc/bg-sentinel` is a deliberately non-existent sentinel path; nothing in
//! this file runs the commands, they are only fed to the hook as JSON.
//! Fixture: hermetic HOME (`.cargo/env` ordinary file, `wt/evil.sh`), cwd =
//! HOME/wt, main session. Empty hook stdout = allow.
//!
//! DOES NOT PROVE: subagent mode; exactly-Deny vs Ask; that each variant was
//! Allow before d07d986e8 as well (base not measured); that the reason text
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
        "bg-verify-92f4-{tag}-{}-{}",
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
        "session_id": "verify-92f4a28d",
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

fn assert_not_allow(tag: &str, c: &str) {
    let home = unique_home(tag);
    let (d, r) = run_main(c, &home);
    assert!(
        d == "ask" || d == "deny",
        "write after an undetermined relink / replaced cargo env must not be allow: {c:?} => {d} ({r})"
    );
}

fn assert_allow(tag: &str, c: &str) {
    let home = unique_home(tag);
    let c = c.replace("@WT@", &home.0.join("wt").display().to_string());
    let (d, r) = run_main(&c, &home);
    assert_eq!(
        d, "allow",
        "common dev line must stay allow: {c:?} => {d} ({r})"
    );
}

// ---- writer reached through a wrapper (head is not in WRITERS) ----

#[test]
fn v_env_ln_over_cargo_env() {
    assert_not_allow(
        "envln",
        "env ln -sf /dev/stdin ~/.cargo/env; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
fn v_command_ln_over_cargo_env() {
    assert_not_allow(
        "cmdln",
        "command ln -sf /dev/stdin ~/.cargo/env; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
fn v_nice_ln_over_cargo_env() {
    assert_not_allow(
        "niceln",
        "nice ln -sf /dev/stdin ~/.cargo/env; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
fn v_timeout_ln_over_cargo_env() {
    assert_not_allow(
        "toln",
        "timeout 5 ln -sf /dev/stdin ~/.cargo/env; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
fn v_busybox_ln_over_cargo_env() {
    assert_not_allow(
        "bbln",
        "busybox ln -sf /dev/stdin ~/.cargo/env; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
fn v_find_exec_ln_over_cargo_env() {
    assert_not_allow(
        "findln",
        "find ~/.cargo -name env -exec ln -sf /dev/stdin {} \\;; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
fn v_xargs_ln_over_cargo_env() {
    assert_not_allow(
        "xargsln",
        "xargs ln -sf /dev/stdin <<< ~/.cargo/env; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

// ---- writers missing from WRITERS ----

#[test]
fn v_tar_extract_into_home() {
    assert_not_allow(
        "tar",
        "tar xf evil.tar -C ~; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
fn v_unzip_into_home() {
    assert_not_allow(
        "unzip",
        "unzip -o evil.zip -d ~; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
fn v_curl_o_cargo_env() {
    assert_not_allow(
        "curl",
        "curl -so ~/.cargo/env http://127.0.0.1/x; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
fn v_patch_cargo_env() {
    assert_not_allow(
        "patch",
        "patch ~/.cargo/env evil.patch; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
fn v_ditto_cargo_env() {
    assert_not_allow(
        "ditto",
        "ditto evil.sh ~/.cargo/env; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

// ---- sourcing spelled differently / HOME rebound ----

#[test]
fn v_ln_then_source_spelling() {
    assert_not_allow(
        "source",
        "ln -sf /dev/stdin ~/.cargo/env; source \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
fn v_home_rebound_then_dot() {
    assert_not_allow(
        "homere",
        "HOME=/tmp/x; . \"$HOME/.cargo/env\"; echo x > d/out.txt",
    );
}

#[test]
fn v_command_builtin_dot_herestring() {
    assert_not_allow(
        "cmdbuiltin",
        "command builtin . /dev/stdin <<< 'ln -s /etc/bg-sentinel d'; echo x > d/out.txt",
    );
}

// ---- Python relink whose program is not an inline -c literal ----

#[test]
fn v_python_herestring_symlink() {
    assert_not_allow(
        "pyhs",
        "python3 <<< 'import os; os.symlink(\"/etc/bg-sentinel\",\"d\")'; echo x > d/out.txt",
    );
}

#[test]
fn v_python_heredoc_symlink() {
    assert_not_allow(
        "pyhd",
        "python3 - <<'EOF'\nimport os\nos.symlink(\"/etc/bg-sentinel\",\"d\")\nEOF\necho x > d/out.txt",
    );
}

// ---- false-positive controls (must stay Allow) ----

#[test]
fn ctl_dot_and_cargo_test() {
    assert_allow("c1", ". \"$HOME/.cargo/env\" && cargo test -p x --test y");
}

#[test]
fn ctl_dot_semicolon_cargo_fmt() {
    assert_allow("c2", ". \"$HOME/.cargo/env\"; cargo fmt");
}

#[test]
fn ctl_cd_abs_dot_cargo_clippy() {
    assert_allow("c3", "cd @WT@ && . \"$HOME/.cargo/env\" && cargo clippy");
}

#[test]
fn ctl_dot_cargo_test_piped_tail() {
    assert_allow(
        "c4",
        ". \"$HOME/.cargo/env\" && cargo test -p x --test y 2>&1 | tail -50",
    );
}

#[test]
fn ctl_mkdir_then_dot_cargo_test() {
    assert_allow("c5", "mkdir -p out && . \"$HOME/.cargo/env\" && cargo test");
}

#[test]
fn ctl_git_stash_then_dot_cargo_test() {
    assert_allow("c6", "git stash && . \"$HOME/.cargo/env\" && cargo test");
}

#[test]
fn ctl_bare_dot() {
    assert_allow("c7", ". \"$HOME/.cargo/env\"");
}
