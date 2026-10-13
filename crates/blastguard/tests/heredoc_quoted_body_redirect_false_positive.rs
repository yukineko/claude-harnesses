// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED regression: a `>` inside a QUOTED-delimiter here-document BODY is read
//! as a truncating redirect.
//!
//! Observed 2026-10-14 against deployed blastguard 0.2.108 (PreToolUse Bash
//! payload piped into the binary, `CLAUDE_CODE_ENTRYPOINT=cli`):
//!
//! ```text
//! python3 - <<'EOF'
//! s = 'x > `y`'
//! print(len(s))
//! EOF
//! ```
//!
//! is DENIED with "'> `y`' destroys the file's current contents and ``y``
//! contains a shell expansion, so blastguard cannot tell which file this
//! names". With a quoted delimiter (`<<'EOF'`) the shell performs NO parsing
//! or expansion of the body: the `>` is a character in the program text fed to
//! python's stdin, never a redirect. Real occurrences blocked the same day were
//! README prose in a heredoc body containing "> `strict_separation`",
//! "> 1800s`", ">=`" and "> ``".
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.
//!
//! The controls below pin what a fix must NOT give up:
//!   * a real truncating redirect outside any heredoc;
//!   * a redirect on the heredoc OPENER line (`cat <<'EOF' > /etc/hosts`),
//!     which IS parsed by the shell;
//!   * a quoted heredoc to python3 whose PROGRAM is destructive (the
//!     stdin-mirror inline-eval analysis, `inline_eval_mirror_closure.rs`
//!     `python3 <<'PY'\nimport shutil\nshutil.rmtree('/')\nPY`), so a fix cannot
//!     just drop interpreter heredoc bodies;
//!   * an UNQUOTED delimiter, where `$(...)`/backticks in the body ARE real
//!     shell expansion — current conservative behaviour is pinned, not relaxed.
//!
//! End-to-end against the built binary, same harness as
//! `heredoc_safe_reader_body.rs` (CLAUDECODE=1, ENTRYPOINT=cli so Ask is not
//! hardened, no session id so the repeat-downgrade cannot fire).

use std::io::Write;
use std::process::{Command, Stdio};

fn verdict(command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = Command::new(bin)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must always exit 0 (cmd: {command:?})"
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

/// A fresh, not-yet-existing file path under the OS temp dir, so the
/// `cat > <file>` cases have nothing to destroy at the redirect target itself.
fn fresh_tmp_file(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-heredoc-fp-{}-{}",
        std::process::id(),
        tag
    ));
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join("out.md");
    let _ = std::fs::remove_file(&f);
    f.to_string_lossy().into_owned()
}

/// The four real body shapes blocked on 2026-10-14, plus the minimal repro.
fn body_shapes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("minimal repro", "s = 'x > `y`'\nprint(len(s))"),
        (
            "README: > `strict_separation`",
            "s = '''when the value is > `strict_separation` the gate blocks'''\nprint(len(s))",
        ),
        (
            "README: > 1800s`",
            "s = '''idle for > 1800s` triggers the reap'''\nprint(len(s))",
        ),
        (
            "README: >=`",
            "s = '''threshold >=`min_score` passes'''\nprint(len(s))",
        ),
        (
            "README: > ``",
            "s = '''an empty span > `` is still text'''\nprint(len(s))",
        ),
    ]
}

/// Collects every case whose verdict is a deny carrying the redirect reason.
fn redirect_false_positives(cases: &[(String, String)]) -> Vec<String> {
    let mut bad = Vec::new();
    for (label, cmd) in cases {
        let (d, reason) = verdict(cmd);
        if d != "allow" {
            bad.push(format!(
                "[{label}] got {d} (reason: {reason}) for {:?}",
                cmd.replace('\n', "\\n")
            ));
        }
    }
    bad
}

/// Marker every Ask produced under the user's 2026-10-14 rulings must carry in
/// `permissionDecisionReason` (ruling R0: the Ask recommends the negative
/// choice).
const RECOMMEND_NO: &str = "Recommended: do not run this.";

/// User ruling R2 (2026-10-14). Program text fed to an interpreter
/// (`python3 -` heredoc, `python3 -c`): a `>` that only LOOKS like a shell
/// redirect inside the program text, whose "target" cannot be determined, is
/// downgraded from Deny to exactly Ask carrying [`RECOMMEND_NO`]. NOT Allow
/// (the ruling explicitly rejects allowing it). Observed Deny on 0.2.108.
#[test]
fn quoted_heredoc_to_python_stdin_body_gt_is_not_a_redirect() {
    let mut cases: Vec<(String, String)> = body_shapes()
        .into_iter()
        .map(|(l, b)| {
            (
                format!("python3 - / {l}"),
                format!("python3 - <<'EOF'\n{b}\nEOF"),
            )
        })
        .collect();
    cases.push((
        "python3 -c / print(1 > 0)".to_string(),
        "python3 -c 'print(1 > 0)'".to_string(),
    ));
    let mut bad = Vec::new();
    for (label, cmd) in &cases {
        let (d, reason) = verdict(cmd);
        if d != "ask" || !reason.contains(RECOMMEND_NO) {
            bad.push(format!(
                "[{label}] got {d} (reason: {reason}) for {:?}",
                cmd.replace('\n', "\\n")
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "program-text `>` must be exactly Ask carrying {RECOMMEND_NO:?}:\n{}",
        bad.join("\n")
    );
}

/// User ruling R2, the other half: program text whose shell-level effect is a
/// DEFINITE destruction outside repo/tmp keeps today's blocking verdict (deny
/// or ask) — the downgrade above must not reach Allow for these.
#[test]
fn program_text_with_definite_destruction_is_not_allowed() {
    for cmd in [
        "python3 -c 'import os; os.system(\"echo hi > /etc/hosts\")'",
        "python3 - <<'EOF'\nimport os\nos.system(\"echo hi > /etc/hosts\")\nEOF",
        "python3 -c \"import shutil; shutil.rmtree('/')\"",
        "python3 -c \"import os; os.system('mkfs /dev/sda')\"",
    ] {
        let (d, reason) = verdict(cmd);
        assert_ne!(
            d,
            "allow",
            "cmd {:?} reason: {reason}",
            cmd.replace('\n', "\\n")
        );
    }
}

/// Quoted-delimiter heredoc fed to bare `cat` (closed safe reader). Must be
/// `allow`. (Observed: `cat <<'EOF'\nnote: 1 > 0\nEOF` is already allowed; the
/// backtick variants are pinned here so a fix cannot regress this reader.)
#[test]
fn quoted_heredoc_to_bare_cat_body_gt_is_not_a_redirect() {
    let mut cases: Vec<(String, String)> = body_shapes()
        .into_iter()
        .map(|(l, b)| (format!("cat / {l}"), format!("cat <<'EOF'\n{b}\nEOF")))
        .collect();
    cases.push((
        "cat / plain 1 > 0".to_string(),
        "cat <<'EOF'\nnote: 1 > 0\nEOF".to_string(),
    ));
    let bad = redirect_false_positives(&cases);
    assert!(
        bad.is_empty(),
        "quoted heredoc body `>` judged as a redirect:\n{}",
        bad.join("\n")
    );
}

/// Quoted-delimiter heredoc written by `cat > <fresh file under temp dir>`.
/// The ONLY redirect the shell parses is the opener's `> <file>`, which names a
/// not-yet-existing temp file. Must be `allow`.
#[test]
fn quoted_heredoc_cat_to_tmp_file_body_gt_is_not_a_redirect() {
    let cases: Vec<(String, String)> = body_shapes()
        .into_iter()
        .enumerate()
        .map(|(i, (l, b))| {
            let f = fresh_tmp_file(&format!("cat{i}"));
            (
                format!("cat > tmp / {l}"),
                format!("cat > {f} <<'EOF'\n{b}\nEOF"),
            )
        })
        .collect();
    let bad = redirect_false_positives(&cases);
    assert!(
        bad.is_empty(),
        "quoted heredoc body `>` judged as a redirect:\n{}",
        bad.join("\n")
    );
}

/// Same, with the operator order the README-writing calls actually used
/// (`cat <<'EOF' > file`).
#[test]
fn quoted_heredoc_cat_opener_redirect_to_tmp_file_body_gt_is_not_a_redirect() {
    let cases: Vec<(String, String)> = body_shapes()
        .into_iter()
        .enumerate()
        .map(|(i, (l, b))| {
            let f = fresh_tmp_file(&format!("catpost{i}"));
            (
                format!("cat <<'EOF' > tmp / {l}"),
                format!("cat <<'EOF' > {f}\n{b}\nEOF"),
            )
        })
        .collect();
    let bad = redirect_false_positives(&cases);
    assert!(
        bad.is_empty(),
        "quoted heredoc body `>` judged as a redirect:\n{}",
        bad.join("\n")
    );
}

// ---------------------------------------------------------------- controls

/// CONTROL: a real truncating redirect into a system file stays `deny`.
#[test]
fn control_plain_truncating_redirect_to_system_file_is_denied() {
    let (d, reason) = verdict("echo hi > /etc/hosts");
    assert_eq!(d, "deny", "reason: {reason}");
}

/// CONTROL: the opener line IS parsed by the shell, so a redirect there is
/// real even though the body is quoted data.
#[test]
fn control_redirect_on_heredoc_opener_line_is_denied() {
    for cmd in [
        "cat <<'EOF' > /etc/hosts\nx > `y`\nEOF",
        "cat > /etc/hosts <<'EOF'\nx > `y`\nEOF",
    ] {
        let (d, reason) = verdict(cmd);
        assert_eq!(
            d,
            "deny",
            "cmd {:?} reason: {reason}",
            cmd.replace('\n', "\\n")
        );
    }
}

/// CONTROL: a truncating redirect AFTER the heredoc closes is ordinary code.
#[test]
fn control_redirect_after_heredoc_close_is_denied() {
    let cmd = "python3 - <<'EOF'\ns = 'x > `y`'\nprint(len(s))\nEOF\necho hi > /etc/hosts";
    let (d, reason) = verdict(cmd);
    assert_eq!(d, "deny", "reason: {reason}");
}

/// CONTROL: a quoted heredoc whose python PROGRAM is destructive must still be
/// blocked by the stdin-mirror inline-eval analysis (mirrors
/// `inline_eval_mirror_closure.rs`), even when the body also carries a
/// harmless `> \`y\`` string. A fix must not simply drop interpreter bodies.
#[test]
fn control_quoted_heredoc_destructive_python_program_is_blocked() {
    for cmd in [
        "python3 - <<'EOF'\nimport shutil\nshutil.rmtree('/')\nEOF",
        "python3 - <<'EOF'\ns = 'x > `y`'\nimport shutil\nshutil.rmtree('/')\nEOF",
        "python3 - <<'EOF'\nimport os\nos.system('mkfs /dev/sda')\nEOF",
    ] {
        let (d, reason) = verdict(cmd);
        assert!(
            d == "deny" || d == "ask",
            "expected blocking for {:?}, got {d} ({reason})",
            cmd.replace('\n', "\\n")
        );
    }
}

/// CONTROL (current conservative behaviour, NOT a target): with an UNQUOTED
/// delimiter the shell DOES perform command substitution in the body, so
/// `` `y` `` really runs `y` before python sees the text. Whatever blastguard
/// decides today must stay blocking; this test exists so a fix keyed on
/// "heredoc body" without checking delimiter quoting turns red.
#[test]
fn control_unquoted_heredoc_body_command_substitution_stays_blocking() {
    let cmd = "python3 - <<EOF\ns = 'x > `y`'\nprint(len(s))\nEOF";
    let (d, reason) = verdict(cmd);
    assert!(
        d == "deny" || d == "ask",
        "unquoted heredoc body runs `y` via command substitution; got {d} ({reason})"
    );
}

/// CONTROL: text EXECUTED BY A SHELL is code, whatever the delimiter quoting.
/// The exemption a fix introduces must apply only to bodies that are NOT fed
/// to a shell — in particular a `>` inside a body fed to `bash` IS a real
/// redirect. All observed `deny` on 0.2.109.
#[test]
fn control_heredoc_body_executed_by_shell_stays_denied() {
    for cmd in [
        "bash <<'EOF'\nrm -rf ~\nEOF",
        "sh -s <<'EOF'\nrm -rf ~/src\nEOF",
        "bash <<'EOF'\necho hi > /etc/hosts\nEOF",
        "zsh <<EOF\nrm -rf ~\nEOF",
        "cat <<'EOF' | bash\nrm -rf ~\nEOF",
        "python3 - <<'EOF'\nimport os\nos.system('rm -rf ~')\nEOF",
        "echo 'rm -rf ~' | sh",
    ] {
        let (d, reason) = verdict(cmd);
        assert_eq!(
            d,
            "deny",
            "cmd {:?} reason: {reason}",
            cmd.replace('\n', "\\n")
        );
    }
}

/// CONTROL: a quoted heredoc captured by `$(cat <<'EOF' …)` and handed to
/// `bash -c` / `eval` is executed. Observed `ask` on 0.2.109; must stay
/// not-allow.
#[test]
fn control_heredoc_captured_into_bash_c_or_eval_is_not_allowed() {
    for cmd in [
        "bash -c \"$(cat <<'EOF'\nrm -rf ~\nEOF\n)\"",
        "eval \"$(cat <<'EOF'\nrm -rf ~\nEOF\n)\"",
    ] {
        let (d, reason) = verdict(cmd);
        assert_ne!(
            d,
            "allow",
            "cmd {:?} reason: {reason}",
            cmd.replace('\n', "\\n")
        );
    }
}

/// EXPECTED RED on 0.2.109 (fail-open found in the same probe): a here-string
/// of a LITERAL destructive command into a shell is the same capability as
/// `echo 'rm -rf ~' | sh` (which is denied) but is allowed with no output.
#[test]
fn here_string_literal_destructive_command_into_shell_is_denied() {
    let mut bad = Vec::new();
    for cmd in ["bash <<< 'rm -rf ~'", "sh <<< \"rm -rf ~\""] {
        let (d, reason) = verdict(cmd);
        if d != "deny" {
            bad.push(format!("{cmd:?} -> {d} ({reason})"));
        }
    }
    assert!(
        bad.is_empty(),
        "here-string into a shell not denied:\n{}",
        bad.join("\n")
    );
}

/// User ruling R1 (2026-10-14): a here-string into a shell whose operand
/// cannot be statically determined (it is built by a command substitution the
/// outer shell runs) is exactly Ask carrying [`RECOMMEND_NO`] — not Allow
/// (the operand is unknown code) and not Deny (nothing destructive was seen).
#[test]
fn here_string_into_shell_with_undeterminable_operand_asks_with_marker() {
    let mut bad = Vec::new();
    for cmd in [
        r#"bash <<<"$(bash <<<"echo hi")""#,
        r#"bash <<<"$(echo ")" ; echo hi)""#,
    ] {
        let (d, reason) = verdict(cmd);
        if d != "ask" || !reason.contains(RECOMMEND_NO) {
            bad.push(format!("{cmd:?} -> {d} ({reason})"));
        }
    }
    assert!(
        bad.is_empty(),
        "undeterminable here-string operand into a shell must be Ask + {RECOMMEND_NO:?}:\n{}",
        bad.join("\n")
    );
}
