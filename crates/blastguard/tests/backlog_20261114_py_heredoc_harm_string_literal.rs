#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (possible false positive, observed 2026-11-14 on a worker command):
//! `cd <wt> && python3 - <<'PY' ... PY` whose Python program only reads a spec
//! file, does `str.replace` with Python string literals (one containing the text
//! `chmod 777 /etc/hosts`) and writes the file back was Denied with: "`chmod`
//! re-permissions `/etc/hosts`, which is inside a system directory ... [system
//! harm: refused on every request, never handed back as a question]".
//!
//! Two readings: (1) real defect: the text is inside a Python string literal
//! that is only written to a file; nothing runs `chmod`, so system-harm Deny is
//! a false positive (a harmless-equivalent program gets a lesser verdict);
//! (2) intended: a stdin-program interpreter's body is scanned as shell-ish text
//! and any literal harm token is refused regardless of quoting. Tell apart: the
//! same program with the literal replaced by `echo hi` gets verdict V; if the
//! harm-string version is Deny while V is not, the Deny is caused by the string
//! content alone -> reading (1) (a gate FP is a bug per standing ruling).
//!
//! Asserted: the exact recovered command (cd path replaced by a temp dir) is not
//! Deny in main mode. Observed at main dde31f11b: Deny with the reason above.
//! NOT reproduced by minimised forms (same verdict as the `echo hi` equivalent,
//! for a single-quoted one-line literal and for a triple-quoted literal fed to
//! `s.replace`), so the trigger lies elsewhere in the full program (e.g. the
//! multi-line `"""` body shape, the `assert`, the `cd &&` prefix); this test pins
//! the exact command only.
//!
//! DOES NOT PROVE: that python-heredoc programs are safe in general (they can
//! exec anything), nor behaviour for other interpreters or quoting styles.
//! Controls (must stay Deny): subprocess.run([...chmod...]), os.system(...),
//! os.chmod('/etc/hosts', 0o777) inside the same heredoc shape.
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
        "bg-side1114-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261114",
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

fn py_heredoc(body: &str) -> String {
    format!("python3 - <<'PY'\n{body}\nPY\n")
}

#[test]
#[ignore = "RED: 2026-11-14 python heredoc harm string literal denied — remove the ignore when fixed"]
fn exact_recovered_command_is_not_denied_in_main() {
    let home = unique_home("exact");
    let wt = home.0.join("wt");
    let cmd = EXACT.replace("@WT@", wt.to_str().unwrap());
    let (d, r) = run_as(&cmd, &home, false);
    assert_ne!(
        d, "deny",
        "recovered command must not be denied; reason={r}"
    );
}

const EXACT: &str = r##"cd @WT@ && python3 - <<'PY'
p='docs/specs/blastguard.md'
s=open(p,encoding='utf-8').read()
old="""  Ask（subagent では Deny）で、中身が literal で harm なら main でも Deny。fd 0 を here-text で開く
  `exec <<< …` は追跡対象外で、従来どおり reader が stdin から program を読みうるとき Ask。
"""
new="""  Ask（subagent では Deny）で、中身が literal で harm なら main でも Deny。fd 0 を here-text で開く
  `exec <<< …` / `exec <<'EOF'` / `exec 0<&N` も同じ追跡で中身を判定し、literal で harm なら main でも Deny
  （backlog 2210e1db、blastguard 0.2.596）。それ以外は従来どおり reader が stdin から program を読みうるとき Ask。
"""
assert s.count(old)==1
s=s.replace(old,new)
anchor="""   - `<&N` や `/dev/fd/N` で fd を読む reader は、shell（`bash|sh|zsh`）に限らない。既存の stdin reader の集合（code interpreter を含む）のすべてを対象とする。
"""
assert s.count(anchor)==1
add=anchor+"""5. fd 0 への拡張（ユーザー裁定 2026-11-14、backlog 2210e1db。blastguard 0.2.596）:
   - 再現テスト: `crates/blastguard/tests/backlog_20261114_exec_stdin_herestring_literal_harm.rs`（`exec <<< 'chmod 777 /etc/hosts'; bash` は main・subagent とも Deny）。
"""
s=s.replace(anchor,add)
open(p,'w',encoding='utf-8').write(s)
PY
git diff --stat docs/
"##;

// ---- controls: real execution of the harm must stay Deny ----

fn control(body: &str, tag: &str) {
    let home = unique_home(tag);
    let (d, r) = run_as(&py_heredoc(body), &home, false);
    assert_eq!(
        d, "deny",
        "control must stay deny; body={body:?} reason={r}"
    );
}

#[test]
fn control_subprocess_run_chmod_is_deny() {
    control(
        "import subprocess\nsubprocess.run(['chmod','777','/etc/hosts'])",
        "c1",
    );
}

#[test]
fn control_os_system_chmod_is_deny() {
    control("import os\nos.system('chmod 777 /etc/hosts')", "c2");
}

#[test]
fn control_os_chmod_is_deny() {
    control("import os\nos.chmod('/etc/hosts', 0o777)", "c3");
}
