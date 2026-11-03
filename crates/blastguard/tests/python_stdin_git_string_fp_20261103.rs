// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction of a heredoc false positive observed 2026-11-03
//! (run-20261103-102800-88064) in a subagent session.
//!
//! FP3 — `python3 - <<'EOF' … EOF` whose Python program merely contains the
//! string `.git` as DATA (it reads a Rust test file, inserts a Rust snippet
//! containing `!dir.join(".git").exists()`, and writes the Rust file back to a
//! non-`.git` path) was refused with:
//!
//! ```text
//! `python3 - <<'EOF'` takes its program from stdin, and that program names
//! `.git`, a protected gate/config path — changing it changes which hooks and
//! gates run, and blastguard will not do that implicitly
//! ```
//!
//! The program never opens anything under `.git`; the only write target is
//! `crates/x/tests/t.rs`.
//!
//! Expected verdict: whatever blastguard returns for the same program with
//! `.git` replaced by the neutral string `.gix`. At minimum the reason must
//! not be the protected-path reason.
//!
//! Controls pin what a fix must NOT give up: python stdin programs that
//! genuinely write to / remove `.git` (literal path, `os.path.join` built
//! path, append mode, `shutil.rmtree`) are still not allowed.
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FP3_REASON: &str = "a protected gate/config path";

/// Fresh existing directory under the OS temp dir (used as cwd).
fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-py-stdin-git-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn verdict(cwd: &Path, command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.current_dir(cwd)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 (cmd: {command:?}); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let decision = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}, got: {stdout}"))
        .to_string();
    let reason = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (decision, reason)
}

fn esc(s: &str) -> String {
    s.replace('\n', "\\n")
}

/// Wrap a Python program as `python3 - <<'EOF' … EOF`.
fn py_stdin(program: &str) -> String {
    format!("python3 - <<'EOF'\n{program}\nEOF")
}

/// FP3: a python stdin program that only mentions `.git` inside a string
/// literal (Rust source being spliced into a non-`.git` file) must be judged
/// like the same program with `.git` replaced by `.gix`.
#[test]
#[ignore = "backlog 61558e27: accepted limit (cancelled by user ruling 2026-11-03; the FP fails toward Ask)"]
fn fp3_python_stdin_program_naming_git_as_data_is_not_a_git_write() {
    let base = fresh_dir("fp3");
    let programs = [
        (
            "live shape: replace + open(p,'w').write",
            "p='crates/x/tests/t.rs'\n\
s=open(p).read()\n\
snippet='    assert!(!dir.join(\".git\").exists());\\n'\n\
s=s.replace('// INSERT\\n', snippet + '// INSERT\\n')\n\
open(p,'w').write(s)"
                .to_string(),
        ),
        (
            "with-block write",
            "p='crates/x/tests/t.rs'\n\
with open(p) as f:\n    s=f.read()\n\
s=s.replace('fn a() {}', 'fn a() { let _ = dir.join(\".git\").exists(); }')\n\
with open(p,'w') as f:\n    f.write(s)"
                .to_string(),
        ),
    ];
    let mut bad = Vec::new();
    for (label, program) in &programs {
        assert!(program.contains(".git"), "[{label}] shape must name .git");
        let cmd = py_stdin(program);
        let equivalent = py_stdin(&program.replace(".git", ".gix"));
        let (d, r) = verdict(&base, &cmd);
        let (ed, er) = verdict(&base, &equivalent);
        if r.contains(FP3_REASON) || d != ed || r != er {
            bad.push(format!(
                "[{label}] got {d} (reason: {r}) vs `.gix` equivalent {ed} (reason: {er}) for {:?}",
                esc(&cmd)
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "FP3: python stdin program naming `.git` only as data treated as a .git write:\n{}",
        bad.join("\n")
    );
}

/// Controls: python stdin programs that genuinely modify `.git` must not be
/// allowed.
#[test]
fn control_python_stdin_program_really_writing_git_is_not_allowed() {
    let base = fresh_dir("ctl");
    let programs = [
        "open('.git/hooks/pre-commit','w').write('x')",
        "import os\np=os.path.join('.git','hooks','x')\nopen(p,'w').write('x')",
        "import shutil; shutil.rmtree('.git')",
    ];
    for program in programs {
        let cmd = py_stdin(program);
        let (d, r) = verdict(&base, &cmd);
        assert_ne!(d, "allow", "control: {:?} reason: {r}", esc(&cmd));
    }
}

/// Control, kept as its own test because it is RED at the commit that added
/// it (measured 2026-11-03): appending `core.hooksPath` to `.git/config`
/// repoints every hook exactly like `git config core.hooksPath /tmp` (which
/// blastguard denies), yet the stdin program is allowed. `.git/config` is not
/// in `PROTECTED_GLOBS`; at the same commit `echo x >> .git/config` and
/// `python3 -c "open('.git/config','a').write('x')"` are also allowed, so
/// the gap is not specific to the stdin path. Pinned here so a fix for FP3 can
/// not be judged complete while this stays allowed. Do not weaken it to match
/// current behavior.
#[test]
fn control_python_stdin_program_appending_git_config_is_not_allowed() {
    let base = fresh_dir("ctl-config");
    let cmd = py_stdin("open('.git/config','a').write('[core]\\n\\thooksPath = /tmp\\n')");
    let (d, r) = verdict(&base, &cmd);
    assert_ne!(d, "allow", "control: {:?} reason: {r}", esc(&cmd));
}
