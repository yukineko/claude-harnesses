#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (possible false positive, observed 2026-11-14, subagent mode):
//! `cat > $S/cmds.txt <<'EOF'` (body lines are shell-like probe strings incl.
//! `echo hi >&$fd`) `EOF` followed in the same command by `python3 $S/probe.py ...`
//! was refused: "'> $fd' destroys the file's current contents and `$fd` contains
//! a shell expansion, so blastguard cannot tell which file this names ...".
//!
//! Two readings: (1) real defect: the .txt body is data (heredoc body is judged
//! by target language; .txt is not shell) and nothing in the command executes it
//! as shell, so the redirect text must not be judged; (2) intended: python3
//! runs an unseen script that might exec the .txt, so refusal is conservative.
//! Tell apart: `python3 /tmp/x/probe.py /tmp/x` alone gets verdict V. If the
//! combination is stricter than V, the .txt body alone is what raised it
//! (reading 1); if it equals V, reading 2 holds and there is no RED.
//!
//! Asserted: verdict(write .txt + python3 probe.py) is no stricter than
//! verdict(python3 probe.py alone), in subagent mode and main mode.
//!
//! DOES NOT PROVE: what probe.py does (blastguard cannot see it); other
//! interpreters; `$S` path forms (literal path used). Control: the same body
//! written to a .sh file and run with `bash` stays non-Allow.
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

fn rank(d: &str) -> u8 {
    match d {
        "allow" => 0,
        "ask" => 1,
        _ => 2,
    }
}

const BODY: &str = "echo hi >&$fd\nprobe 3<<< x\n";

fn combo(path: &str, runner: &str) -> String {
    format!("cat > {path} <<'EOF'\n{BODY}EOF\n{runner}\n")
}

fn check(tag: &str, subagent: bool) {
    let home = unique_home(tag);
    let d = format!("{}/wt", home.0.display());
    let (ad, ar) = run_as(&format!("python3 {d}/probe.py {d}"), &home, subagent);
    let (cd, cr) = run_as(
        &combo(
            &format!("{d}/cmds.txt"),
            &format!("python3 {d}/probe.py {d}"),
        ),
        &home,
        subagent,
    );
    assert!(
        rank(&cd) <= rank(&ad),
        "txt data heredoc made it stricter (subagent={subagent}): alone={ad} ({ar}) combo={cd} ({cr})"
    );
}

#[test]
#[ignore = "RED: 2026-11-14 .txt data heredoc + python3 denied on redirect text — remove the ignore when fixed"]
fn txt_data_heredoc_then_python_no_stricter_subagent() {
    check("s", true);
}

#[test]
#[ignore = "RED: 2026-11-14 .txt data heredoc + python3 denied on redirect text — remove the ignore when fixed"]
fn txt_data_heredoc_then_python_no_stricter_main() {
    check("m", false);
}

#[test]
fn control_sh_heredoc_then_bash_stays_non_allow() {
    let home = unique_home("c");
    let w = format!("{}/wt", home.0.display());
    let (d, r) = run_as(
        &combo(&format!("{w}/cmds.sh"), &format!("bash {w}/cmds.sh")),
        &home,
        true,
    );
    assert_ne!(d, "allow", "control must stay non-allow; reason={r}");
}
