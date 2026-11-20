#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, observed 2026-11-16 at main fa58e3698, run-20261116-143410-12142):
//! the KNOWN GAPS of `src/detect/git_apply_patch.rs`. A `git apply` heredoc
//! patch creates `run.sh` holding `chmod 777 /etc/hosts`, and the same command
//! then runs `sh run.sh`. The patch reaches git through (a) `|&`, (b) a
//! subshell, (c) a trailing-`|` line continuation, (d) a TAB-indented `<<-`
//! body, (e) a reader line that also carries another command's heredoc; F3:
//! a non-plain `git` word (quoted / variable). F4: a README.md patch whose
//! added text is `rm -rf ~` with nothing executed (possible false positive).
//!
//! Each shape asserts not-allow in main and subagent mode (F4: allow). Which
//! of them are RED is recorded by the `#[ignore]` attributes: only shapes
//! observed allow at HEAD are ignored; the rest are plain controls.
//!
//! DOES NOT PROVE: the verdict for other patch contents, `git am` in these
//! shapes, or the reason text.
//!
//! RED written by an independent test author, not the implementer.

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
        "bg-gitapply1116-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "bg-gitapply-20261116",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
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

fn assert_not_allow(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    eprintln!("OBSERVED {tag}: main={m} [{mr}] subagent={s} [{sr}]");
    assert!(
        m != "allow" && s != "allow",
        "{tag}: main={m} [{mr}] subagent={s} [{sr}]; cmd={cmd:?}"
    );
}

fn assert_allow(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    eprintln!("OBSERVED {tag}: main={m} [{mr}] subagent={s} [{sr}]");
    assert!(
        m == "allow" && s == "allow",
        "{tag}: main={m} [{mr}] subagent={s} [{sr}]; cmd={cmd:?}"
    );
}

const PATCH: &str = "diff --git a/run.sh b/run.sh\nnew file mode 100644\n--- /dev/null\n+++ b/run.sh\n@@ -0,0 +1 @@\n+chmod 777 /etc/hosts\n";

#[test]
fn control_base_shape_git_apply_then_sh_is_not_allow() {
    assert_not_allow("base", &format!("git apply <<'EOF'\n{PATCH}EOF\nsh run.sh"));
}

#[test]
fn a_pipe_ampersand_reader() {
    assert_not_allow(
        "a",
        &format!("cat <<'EOF' |& git apply\n{PATCH}EOF\nsh run.sh"),
    );
}

#[test]
fn b_subshell_reader() {
    assert_not_allow(
        "b",
        &format!("cat <<'EOF' | (git apply)\n{PATCH}EOF\nsh run.sh"),
    );
}

#[test]
#[ignore = "RED: git apply reader after a trailing-| line continuation (patch via cat heredoc), then sh run.sh, judged allow (fail-open; git_apply_patch.rs KNOWN GAP)"]
fn c_trailing_pipe_continuation_reader() {
    assert_not_allow(
        "c",
        &format!("cat <<'EOF' |\n{PATCH}EOF\ngit apply\nsh run.sh"),
    );
}

#[test]
#[ignore = "RED: git apply <<- heredoc with a TAB-indented added line (+chmod 777 /etc/hosts into run.sh), then sh run.sh, judged allow (fail-open; git_apply_patch.rs KNOWN GAP)"]
fn d_tab_indented_dash_heredoc_reader() {
    // Only the added line is TAB-indented; the shell strips the TAB, so git
    // sees `+chmod 777 /etc/hosts`. (Tabbing every line makes the module refuse
    // for "no patch header", which would hide the gap.)
    let tabbed = PATCH.replace("\n+chmod", "\n\t+chmod");
    assert_not_allow("d", &format!("git apply <<-'EOF'\n{tabbed}EOF\nsh run.sh"));
}

#[test]
fn e_reader_line_carrying_another_commands_heredoc() {
    assert_not_allow(
        "e",
        &format!("cat <<'A' >f; git apply < x.patch\n{PATCH}A\nsh run.sh"),
    );
}

#[test]
#[ignore = "RED: 'git' apply (quoted git word) heredoc patch writing run.sh, then sh run.sh, judged allow (fail-open; git_apply_patch.rs KNOWN GAP)"]
fn f3_quoted_git_word() {
    assert_not_allow(
        "f3q",
        &format!("'git' apply <<'EOF'\n{PATCH}EOF\nsh run.sh"),
    );
}

#[test]
fn f3_variable_git_word() {
    assert_not_allow(
        "f3v",
        &format!("g=git; $g apply <<'EOF'\n{PATCH}EOF\nsh run.sh"),
    );
}

#[test]
fn f4_readme_patch_with_rm_text_and_nothing_executed_is_allow() {
    // Reading (a): false positive, patch bodies are read as shell regardless of
    // the target file type. Reading (b): intended, the module is conservative.
    let readme = "diff --git a/README.md b/README.md\nnew file mode 100644\n--- /dev/null\n+++ b/README.md\n@@ -0,0 +1 @@\n+rm -rf ~\n";
    assert_allow("f4", &format!("git apply <<'EOF'\n{readme}EOF"));
}
