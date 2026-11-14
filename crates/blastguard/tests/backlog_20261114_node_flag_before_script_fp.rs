#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-14 by the verifier on the build of
//! HEAD 99bb406b6 in condukt worktree run-20261114-111444-69657-f9861c4e, the
//! pipe-to-interpreter-with-value-flag fix task): a harmless stdin piped to
//! `node <option> <script>` is Ask/Deny although node reads the script FILE as
//! the program and never executes stdin as code. Observed (main / subagent):
//!   cat x | node --max-old-space-size=4096 script.js     ask / deny  <- FP
//!   cat x | node --inspect script.js                     ask / deny  <- FP
//!   cat x | node --experimental-strip-types s.ts         ask / deny  <- FP
//! On base b0b4ac6e3 these were Allow.
//!
//! Option facts (node docs, https://nodejs.org/api/cli.html, checked 2026-11-14):
//!   `--max-old-space-size=SIZE` — value glued with `=` in the same word.
//!   `--inspect[=[host:]port]` — optional value, only via `=`.
//!   `--watch`, `--test`, `--enable-source-maps`, `--trace-warnings` — no value.
//!   `--experimental-strip-types` (docs v22.6.0, "Enable experimental
//!   type-stripping for TypeScript files.") — no value.
//! So in every RED shape the word after the option is the script file.
//!
//! Asserted:
//!   - RED: `cat notes.txt | node <opt> script.js` and `echo hi | node <opt>
//!     script.js` for each option above are allow in main and subagent mode.
//!   - control: `node script.js`, `node -r m script.js`, `node --no-warnings
//!     script.js` (same harmless stdin) stay allow; a harmful program piped to
//!     `node --max-old-space-size=4096` / `node --inspect` with NO script stays
//!     deny (node then reads the program from stdin).
//!
//! DOES NOT PROVE: an unknown bare flag (`node --someunknownflag script.js`)
//! — whether it takes a separate value is unknown, so no assertion is made;
//! other interpreters; `--test` with glob/dir positionals; `-` as script.
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
        "bg-nodeflagfp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "node-flag-before-script-fp-20261114",
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

const NODE: &str = "echo 'require(\"fs\").chmodSync(\"/etc/hosts\", 0o777)' | ";
const HARMLESS: [&str; 2] = ["cat notes.txt | ", "echo hi | "];

fn mismatched(cmds: &[String], want: &str, tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if d != want {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

fn harmless_piped(tails: &[&str]) -> Vec<String> {
    let mut v = Vec::new();
    for pre in HARMLESS {
        for tail in tails {
            v.push(format!("{pre}{tail}"));
        }
    }
    v
}

#[test]
fn harmless_stdin_piped_to_node_option_then_script_is_allow() {
    let cmds = harmless_piped(&[
        "node --max-old-space-size=4096 script.js",
        "node --inspect script.js",
        "node --watch script.js",
        "node --test script.js",
        "node --enable-source-maps script.js",
        "node --experimental-strip-types s.ts",
        "node --trace-warnings script.js",
    ]);
    let bad = mismatched(&cmds, "allow", "red");
    assert!(
        bad.is_empty(),
        "expected Allow (node runs the script file, not stdin):\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_harmless_stdin_piped_to_node_script_stays_allow() {
    let cmds = harmless_piped(&[
        "node script.js",
        "node -r m script.js",
        "node --no-warnings script.js",
    ]);
    let bad = mismatched(&cmds, "allow", "ctl-allow");
    assert!(
        bad.is_empty(),
        "control shapes must stay Allow:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_harmful_program_piped_to_node_option_without_script_is_deny() {
    let cmds = vec![
        format!("{NODE}node --max-old-space-size=4096"),
        format!("{NODE}node --inspect"),
    ];
    let bad = mismatched(&cmds, "deny", "ctl-deny");
    assert!(
        bad.is_empty(),
        "control shapes must stay Deny (node reads the program from stdin):\n{}",
        bad.join("\n")
    );
}
