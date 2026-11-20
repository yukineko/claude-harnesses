#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-17 in a peer session; blocks.jsonl
//! ts 1794865460 subagent deny / ts 1794865551 main ask; relay bgr-cf29c893de79).
//!
//! Shape: `mkdir -p <dir> && cd <dir> && cat > <x>/mod.rs <<'EOF'` + Rust source
//! then `EOF`. The QUOTED delimiter means the shell never expands the body, and
//! nothing in the same command runs the `.rs` file (user ruling: heredoc body by
//! target language — a `.rs` target not run in the same command is not shell).
//! The body held the doc-comment line
//! `/// Change the tree identity (a tracked edit); `restore` puts it back.`
//! and blastguard answered (main ask / subagent deny), verbatim:
//!   the command word ``restore`` is an expansion whose value only exists at
//!   run time — blastguard cannot tell what program this runs
//!
//! Measured 2026-11-17 against the deployed binary 0.2.675 (scratch HOME, cwd
//! HOME/wt, PreToolUse JSON on stdin, env cleared), main / subagent:
//!   PRE=`mkdir -p d/t && cd d && `, body line L
//!   PRE + cat > t/mod.rs, L=`/// ...edit); `restore` puts it back.`  ask / deny
//!   no PRE (heredoc is the first command), same L                     allow / allow
//!   `cd d && ` only (one preceding command), L=`// a; `restore` b`    allow / allow
//!   PRE, L=`// a `restore` b` (no control operator before the span)   allow / allow
//!   PRE, L=`/// `restore` x`                                          allow / allow
//!   PRE, L with `;` / `&` / `|` / `&&` before the span                ask / deny
//!   `true && true && ` or `true; true; ` as PRE                       ask / deny
//!   target t/x.txt, or `cat <<'EOF'` to stdout                        ask / deny
//!   PRE, same L without backticks                                     allow / allow
//! So the trigger is: heredoc command is the 3rd+ statement AND the body line
//! has a shell control operator before the backtick span. The target extension
//! is not involved (the `.txt` and stdout shapes fire too); `.rs` is asserted
//! here because it is the observed shape.
//!
//! Differs from the pending items read before filing: d292ce6e / 53a97aa3
//! (backtick at column 0), d462444e (span crossing a line break; single-line
//! span reported Allow), 9d347af5 / 234cae6f (git commit lines), a826bdab
//! (JSON `"…"`-adjacent span, fires with no preceding command), 0bc46ca0
//! (later rm/mv/cp/git add after EOF). None of their tests carries a control
//! operator before a mid-line span with two preceding commands.
//!
//! Two readings: (a) defect — the quoted-delimiter body is data and some path
//! re-splits the body line at `;`/`&`/`|`, making `` `restore` `` a command word;
//! (b) intended — none found: an unquoted delimiter would expand it (control c2
//! keeps that non-Allow), but `<<'EOF'` never does.
//!
//! DOES NOT PROVE: which code path re-lexes the body; that the `.txt` / stdout /
//! `&` / `|` variants are fixed (only `.rs` + `;` is asserted); anything about
//! commands that later execute the written file; the exact Ask/Deny split of
//! the unquoted control.
//!
//! RED written by an independent test author, not the implementer; `src/`
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
        "bg-rsbt-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "rsbt-20261117",
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
const PLAIN_LINE: &str = "/// Change the tree identity (a tracked edit); restore puts it back.";

/// `mkdir -p d/t && cd d && cat > t/mod.rs <<{delim}` + doc line + a fn + `EOF`.
fn command(delim: &str, line: &str) -> String {
    format!("mkdir -p d/t && cd d && cat > t/mod.rs <<{delim}\n{line}\npub fn restore() {{}}\nEOF")
}

#[test]
fn quoted_rs_heredoc_doc_comment_backtick_is_data() {
    let home = unique_home("fp");
    let cmd = command("'EOF'", BT_LINE);
    let (m, mr) = run_as(&cmd, &home, false);
    let (s, sr) = run_as(&cmd, &home, true);
    assert_eq!(
        (m.as_str(), s.as_str()),
        ("allow", "allow"),
        "(main, subagent): quoted-delimiter .rs body is data; main reason={mr}; subagent reason={sr}"
    );
}

/// Control c1 (passes at HEAD): the same command with the backticks removed is
/// allowed in both modes — the FP is pinned to the backtick span.
#[test]
fn control_same_body_without_backticks_is_allowed() {
    let home = unique_home("plain");
    let cmd = command("'EOF'", PLAIN_LINE);
    let (m, mr) = run_as(&cmd, &home, false);
    let (s, sr) = run_as(&cmd, &home, true);
    assert_eq!(m, "allow", "main: {mr}");
    assert_eq!(s, "allow", "subagent: {sr}");
}

/// Control c2 (passes at HEAD): an UNQUOTED delimiter really expands the span,
/// so `restore` runs as a command and must not be allowed.
#[test]
fn control_unquoted_delimiter_span_is_not_allowed() {
    let home = unique_home("unq");
    let cmd = command("EOF", BT_LINE);
    let (m, mr) = run_as(&cmd, &home, false);
    let (s, sr) = run_as(&cmd, &home, true);
    assert_ne!(m, "allow", "unquoted heredoc expands the span; reason={mr}");
    assert_ne!(s, "allow", "unquoted heredoc expands the span; reason={sr}");
}
