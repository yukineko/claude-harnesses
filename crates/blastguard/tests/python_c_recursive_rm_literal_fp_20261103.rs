// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED reproduction of a false positive observed 2026-11-03 at blastguard
//! 0.2.245 (F-A).

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Fresh existing directory under the OS temp dir (used as cwd).
fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-bg-findings-{}-{tag}",
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

/// F-A: the exact command denied on 2026-11-03 ("rm with a wildcard can delete
/// many files at once"). It only reads a file and prints; 'recursive rm' is
/// string data inside the Python program.
#[test]
#[ignore = "backlog 1943843c: open defect, remove ignore when fixed"]
fn fa_python_c_reading_file_with_recursive_rm_literal_is_not_denied() {
    let dir = fresh_dir("fa");
    let cmd = r#"python3 -c "
import json
p='/private/tmp/claude-502/-Users-yuki-src-harness/064f2060-9716-4e65-9495-61517ceee238/tasks/abb31611c2fb3ce04.output'
cmds={}
for line in open(p):
    try: d=json.loads(line)
    except: continue
    m=d.get('message',{})
    if not isinstance(m,dict) or not isinstance(m.get('content'),list): continue
    for c in m['content']:
        if c.get('type')=='tool_use' and c.get('name')=='Bash': cmds[c['id']]=c['input']['command']
        if c.get('type')=='tool_result':
            t=json.dumps(c.get('content'))
            if 'recursive rm' in t: print(repr(cmds.get(c['tool_use_id']))[:1500]); print('RESULT',t[:400])
""#;
    let (d, r) = verdict(&dir, cmd);
    assert_ne!(d, "deny", "F-A: -> {d} (reason: {r})");
}
