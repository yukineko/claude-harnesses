#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 648a6641 (false positive) — prefix / target / operator variants.
//!
//! Companion to `backlog_20261117_rs_heredoc_doc_comment_backtick_fp.rs`, whose
//! header table (measured on 0.2.675) records that the FP also fired with
//! `true && true && ` / `true; true; ` as PRE, with a `t/x.txt` target, with
//! `cat <<'EOF'` to stdout, and with `&` / `|` / `&&` before the backtick span.
//! That RED asserts only the `mkdir -p d/t && cd d && cat > t/mod.rs` + `;`
//! shape. These tests check whether a fix covers the defect (the
//! quoted-delimiter body is data) or only that one shape.
//!
//! A-cases (FP property): quoted delimiter, nothing in the same command runs
//! the written file / body → allow in main AND subagent (user ruling: heredoc
//! body by target language — a .rs/.txt/.md target not run in the same
//! command is not shell; a stdout `cat` runs nothing).
//!
//! B-cases (no fail-open guards): the body or the written file IS run in the
//! same command, the target is `.sh`, or the delimiter is unquoted → not
//! allow. The harmful `bash <<'EOF'` + `rm -rf ~` case must be deny.
//! `.rs` + `cargo run` (spec A1 of bg-heredoc-script-failopen-596ef57d) is
//! INFORMATIONAL only (printed, not asserted) because 596ef57d is not
//! implemented yet.
//!
//! DOES NOT PROVE: which code path re-lexes the body; behaviour of other
//! landing-path namers than mkdir / cd / true; anything about the deployed
//! binary (these run the test-built binary).
//!
//! Written by an independent test author, not the implementer; `src/`
//! untouched.

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
        "bg-648v-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "v648-20261121",
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

const BT_LINE: &str = "/// Change the tree identity (a tracked edit); `restore` puts it back.";
const FN_LINE: &str = "pub fn restore() {}";
const PRE: &str = "mkdir -p d/t && cd d && ";

/// Runs `cmd` in both modes, prints both verdicts, returns them.
fn both(tag: &str, cmd: &str) -> ((String, String), (String, String)) {
    let home = unique_home(tag);
    let m = run_as(cmd, &home, false);
    let s = run_as(cmd, &home, true);
    println!("[{tag}] command={cmd:?}");
    println!("[{tag}] main={} reason={:?}", m.0, m.1);
    println!("[{tag}] sub ={} reason={:?}", s.0, s.1);
    (m, s)
}

fn assert_allow_both(tag: &str, cmd: &str) {
    let ((m, mr), (s, sr)) = both(tag, cmd);
    assert_eq!(
        (m.as_str(), s.as_str()),
        ("allow", "allow"),
        "[{tag}] (main, subagent): quoted-delimiter body is data; main reason={mr}; subagent reason={sr}"
    );
}

fn assert_not_allow_both(tag: &str, cmd: &str) {
    let ((m, mr), (s, sr)) = both(tag, cmd);
    assert_ne!(m, "allow", "[{tag}] main must not allow; reason={mr}");
    assert_ne!(s, "allow", "[{tag}] subagent must not allow; reason={sr}");
}

// ---------------------------------------------------------------- A: FP property

#[test]
fn a1_pre_true_and_true_rs_target_is_allowed() {
    let cmd = format!("true && true && cat > t/mod.rs <<'EOF'\n{BT_LINE}\n{FN_LINE}\nEOF");
    assert_allow_both("a1", &cmd);
}

#[test]
fn a2_pre_true_semicolon_true_rs_target_is_allowed() {
    let cmd = format!("true; true; cat > t/mod.rs <<'EOF'\n{BT_LINE}\n{FN_LINE}\nEOF");
    assert_allow_both("a2", &cmd);
}

#[test]
fn a3_pre_mkdir_cd_txt_target_is_allowed() {
    let cmd = format!("{PRE}cat > t/x.txt <<'EOF'\n{BT_LINE}\n{FN_LINE}\nEOF");
    assert_allow_both("a3", &cmd);
}

#[test]
fn a4_pre_mkdir_cd_cat_to_stdout_is_allowed() {
    let cmd = format!("{PRE}cat <<'EOF'\n{BT_LINE}\n{FN_LINE}\nEOF");
    assert_allow_both("a4", &cmd);
}

#[test]
fn a5a_pre_mkdir_cd_ampersand_before_span_is_allowed() {
    let line = "/// Change the tree identity (a tracked edit) & `restore` puts it back.";
    let cmd = format!("{PRE}cat > t/mod.rs <<'EOF'\n{line}\n{FN_LINE}\nEOF");
    assert_allow_both("a5a", &cmd);
}

#[test]
fn a5b_pre_mkdir_cd_pipe_before_span_is_allowed() {
    let line = "/// Change the tree identity (a tracked edit) | `restore` puts it back.";
    let cmd = format!("{PRE}cat > t/mod.rs <<'EOF'\n{line}\n{FN_LINE}\nEOF");
    assert_allow_both("a5b", &cmd);
}

#[test]
fn a5c_pre_mkdir_cd_and_and_before_span_is_allowed() {
    let line = "/// Change the tree identity (a tracked edit) && `restore` puts it back.";
    let cmd = format!("{PRE}cat > t/mod.rs <<'EOF'\n{line}\n{FN_LINE}\nEOF");
    assert_allow_both("a5c", &cmd);
}

#[test]
fn a6_pre_mkdir_cd_md_target_is_allowed() {
    let cmd = format!("{PRE}cat > t/notes.md <<'EOF'\n{BT_LINE}\n{FN_LINE}\nEOF");
    assert_allow_both("a6", &cmd);
}

// ------------------------------------------------------- B: no-fail-open guards

#[test]
fn b1_written_rs_run_by_bash_after_eof_is_not_allowed() {
    let cmd = format!("{PRE}cat > t/mod.rs <<'EOF'\n{BT_LINE}\n{FN_LINE}\nEOF\nbash t/mod.rs");
    assert_not_allow_both("b1", &cmd);
}

#[test]
fn b2_written_rs_executed_directly_after_eof_is_not_allowed() {
    let cmd = format!("{PRE}cat > t/mod.rs <<'EOF'\n{BT_LINE}\n{FN_LINE}\nEOF\n./t/mod.rs");
    assert_not_allow_both("b2", &cmd);
}

#[test]
fn b3_written_rs_run_by_sh_on_heredoc_line_is_not_allowed() {
    let cmd = format!("{PRE}cat > t/mod.rs <<'EOF' && sh t/mod.rs\n{BT_LINE}\n{FN_LINE}\nEOF");
    assert_not_allow_both("b3", &cmd);
}

#[test]
fn b4_sh_target_body_is_shell_and_not_allowed() {
    let cmd = format!("{PRE}cat > t/run.sh <<'EOF'\n{BT_LINE}\n{FN_LINE}\nEOF");
    assert_not_allow_both("b4", &cmd);
}

#[test]
fn b5_unquoted_delimiter_span_is_not_allowed() {
    let cmd = format!("{PRE}cat > t/mod.rs <<EOF\n{BT_LINE}\n{FN_LINE}\nEOF");
    assert_not_allow_both("b5", &cmd);
}

/// INFORMATIONAL (not asserted): spec A1 of
/// specs/bg-heredoc-script-failopen-596ef57d.toml says `.rs` + `cargo run` is
/// not-Allow, but 596ef57d is not implemented yet.
#[test]
fn b6_informational_rs_then_cargo_run() {
    let cmd = format!("{PRE}cat > t/mod.rs <<'EOF'\n{BT_LINE}\n{FN_LINE}\nEOF\n&& cargo run");
    let cmd2 = format!("{PRE}cat > t/mod.rs <<'EOF' && cargo run\n{BT_LINE}\n{FN_LINE}\nEOF");
    let ((m, _), (s, _)) = both("b6-after-eof", &cmd);
    let ((m2, _), (s2, _)) = both("b6-on-heredoc-line", &cmd2);
    println!("[b6] INFORMATIONAL (spec A1 expects not-allow): after-eof main={m} sub={s}; on-line main={m2} sub={s2}");
}

#[test]
fn b7_harmful_body_fed_to_bash_is_denied() {
    let cmd = "bash <<'EOF'\nrm -rf ~\nEOF";
    let ((m, mr), (s, sr)) = both("b7", cmd);
    assert_eq!(m, "deny", "[b7] main must deny; reason={mr}");
    assert_eq!(s, "deny", "[b7] subagent must deny; reason={sr}");
}
