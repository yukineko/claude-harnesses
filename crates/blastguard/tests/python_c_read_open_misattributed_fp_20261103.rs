// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction of a false positive observed 2026-11-03 (blastguard
//! 0.2.218).
//!
//! FP4 — `python3 -c "…"` that opens an EXISTING file read-only
//! (`open('<decomposition.json>')`, default mode) and writes only to a
//! different, formatted scratch path (`open('<scratch>/task-%s.json'%t['id'],'w')`)
//! was denied with:
//!
//! ```text
//! `python3` is invoked with an inline-eval flag, and it writes
//! `/Users/yuki/.condukt/state/harness-e77d23c7/run-20261103-102800-88064.decomposition.json`
//! (`,'w')`) — it exists, and it is not inside a git work tree — nothing holds
//! a second copy of these bytes
//! ```
//!
//! The mode `'w'` belongs to the scratch `open(...)`; the decomposition file is
//! only read. blastguard attributed the `'w'` to the wrong `open()`.
//!
//! Expected verdict for the FP4 shape: the same decision and reason as the
//! identical command with the read-only `open(A)` removed (the write to a
//! scratch path alone). Measured 2026-11-03 at 0.2.218: that variant returns
//! `allow` with an empty reason (the write target is a new file in an
//! existing temp-dir scratch dir). The test asserts the equivalence AND that
//! the reason does not name the read-only file. At 0.2.218 the FP4 command
//! instead returns `ask` naming A as the `,'w')` target (RED).
//!
//! Control: `open(A,'w')` on the same existing non-git file really is a write
//! to an unbacked file and must stay refused.
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Fresh existing directory under the OS temp dir (used as cwd).
fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-py-c-read-open-{}-{tag}",
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

/// Create `<base>/state/a.decomposition.json` (existing, non-git, with
/// content) and an existing scratch dir `<base>/scratch`. Returns (A, scratch).
fn make_files(base: &Path) -> (PathBuf, PathBuf) {
    let state = base.join("state");
    let scratch = base.join("scratch");
    std::fs::create_dir_all(&state).expect("mkdir state");
    std::fs::create_dir_all(&scratch).expect("mkdir scratch");
    let a = state.join("a.decomposition.json");
    std::fs::write(&a, "{\"tasks\":[{\"id\":\"t1\"}]}").expect("write A");
    (a, scratch)
}

/// FP4: read-only `open(A)` + formatted-name write `open(B%x,'w')`.
/// Must be judged exactly like the same command without the read of A, and
/// the reason must not claim the command writes A.
#[test]
fn fp4_read_only_open_is_not_reported_as_the_write_target() {
    let base = fresh_dir("fp4");
    let (a, scratch) = make_files(&base);
    let a = a.to_string_lossy();
    let sc = scratch.to_string_lossy();

    let with_read = format!(
        "python3 -c \"\nimport json;d=json.load(open('{a}'))\nfor t in d['tasks']:\n  open('{sc}/task-%s.json'%t['id'],'w').write(json.dumps(t,ensure_ascii=False,indent=1))\nprint('ok')\""
    );
    // Same write, read of A removed.
    let without_read = format!(
        "python3 -c \"\nimport json;d={{'tasks':[{{'id':'t1'}}]}}\nfor t in d['tasks']:\n  open('{sc}/task-%s.json'%t['id'],'w').write(json.dumps(t,ensure_ascii=False,indent=1))\nprint('ok')\""
    );
    let (d, r) = verdict(&base, &with_read);
    let (ed, er) = verdict(&base, &without_read);
    assert!(
        !r.contains(a.as_ref()) && d == ed && r == er,
        "FP4: read-only open(A) misattributed as write target.\n  with read:    {d} (reason: {r})\n  without read: {ed} (reason: {er})\n  A = {a}\n  cmd: {}",
        esc(&with_read)
    );
}

/// Control: really writing the existing non-git file A via `open(A,'w')` must
/// stay restrictive (anything but allow) and name A.
#[test]
fn control_python_c_really_writing_an_existing_unbacked_file_is_still_refused() {
    let base = fresh_dir("ctl");
    let (a, _scratch) = make_files(&base);
    let a = a.to_string_lossy();
    let cmd = format!("python3 -c \"open('{a}','w').write('x')\"");
    let (d, r) = verdict(&base, &cmd);
    assert_ne!(d, "allow", "control: {:?} reason: {r}", esc(&cmd));
}
