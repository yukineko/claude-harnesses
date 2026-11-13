//! RED (6cac93af round 4): a backslash-newline inside a double-quoted shell
//! word is removed by the shell, so `python3 -c "o\<NL>pen('/etc/hosts','a')"`
//! hands python `open('/etc/hosts','a')`. blastguard must not Allow it.
//!
//! In single quotes the backslash-newline is NOT removed (python sees a syntax
//! error); either decision is acceptable there, so it is printed, not asserted.

#[path = "support/bounded.rs"]
mod bounded;
use std::process::{Command, Stdio};

fn run_as(command: &str, home: &std::path::Path, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "backlog-line-continuation",
        "cwd": home.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

fn home() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("bg-linecont-{}", std::process::id()));
    std::fs::create_dir_all(d.join("wt")).unwrap();
    d
}

fn dq(body: &str) -> String {
    format!("python3 -c \"{body}\"")
}

fn sq(body: &str) -> String {
    format!("python3 -c '{body}'")
}

#[test]
#[ignore = "RED: backslash-newline inside a double-quoted python3 -c word splits the identifier (o\\<NL>pen / os.ch\\<NL>mod) and is Allow; fix pending (6cac93af round 4)"]
fn double_quoted_line_continuation_in_identifier_is_not_allow() {
    let h = home();
    let mut shapes: Vec<String> = Vec::new();
    for m in ["a", "w"] {
        shapes.push(dq(&format!("o\\\npen('/etc/hosts','{m}')")));
        shapes.push(dq(&format!("op\\\nen('/etc/hosts','{m}').write('x')")));
    }
    shapes.push(dq("import os; os.ch\\\nmod('/etc/hosts',0)"));
    shapes.push(dq("import os; o\\\ns.chmod('/etc/hosts',0)"));

    // single-quoted control: informational only.
    for m in ["a", "w"] {
        let c = sq(&format!("o\\\npen('/etc/hosts','{m}')"));
        println!(
            "SQ-INFO {} {} | {c:?}",
            run_as(&c, &h, false),
            run_as(&c, &h, true)
        );
    }
    let c = sq("import os; os.ch\\\nmod('/etc/hosts',0)");
    println!(
        "SQ-INFO {} {} | {c:?}",
        run_as(&c, &h, false),
        run_as(&c, &h, true)
    );

    let mut bad = Vec::new();
    for c in &shapes {
        let a = run_as(c, &h, false);
        let b = run_as(c, &h, true);
        println!("DQ main={a} subagent={b} | {c:?}");
        if a == "allow" {
            bad.push(format!("main allow: {c:?}"));
        }
        if b == "allow" {
            bad.push(format!("subagent allow: {c:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} Allow of system-path write/chmod via line continuation:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn control_without_continuation_is_not_allow() {
    let h = home();
    let mut progs: Vec<String> = Vec::new();
    for m in ["a", "w"] {
        progs.push(dq(&format!("open('/etc/hosts','{m}')")));
    }
    progs.push(dq("import os; os.chmod('/etc/hosts',0)"));
    let mut bad = Vec::new();
    for c in &progs {
        for sub in [false, true] {
            let d = run_as(c, &h, sub);
            println!("CONTROL subagent={sub} {d} | {c:?}");
            if d == "allow" {
                bad.push(format!("subagent={sub}: {c:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "control Allow: {bad:?}");
}
