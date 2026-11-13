#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 47b20647 (2026-11-13): Python removes a backslash + LF ("BN")
//! inside a NON-raw string literal (single-, double- or triple-quoted, `b''`,
//! `f''`), so `'/etc/ho\<LF>sts'` is `/etc/hosts` at runtime. A raw literal
//! (`r'…'`) keeps both the backslash and the LF.
//!
//! Asserted:
//! - every system-harm shape below is **Deny** (not merely "not Allow") in the
//!   main session AND in a subagent (user ruling: system harm always Deny);
//! - each shape's plain-path twin is Deny too (the control that shows the
//!   program itself is one blastguard denies when the path is spelled plainly);
//! - the same for programs fed to `python` on stdin (`python3 -`, `/dev/stdin`,
//!   quoted and UNQUOTED here-documents — the shell turns `\\` into `\` in the
//!   latter) and for `\N{…}` named escapes: a printable-ASCII name is decoded
//!   (`\N{SOLIDUS}`, `\N{LATIN SMALL LETTER E}` even inside a directory name,
//!   capital letters, digits, case-insensitive), a non-ASCII one is an unknown
//!   character (`/etc/\N{SNOWMAN}` is a new file in /etc);
//! - a NON-ASCII `\N{…}` inside a directory NAME (`/\N{SNOWMAN}tc/hosts`) is
//!   Ask in the main session and Deny in a subagent (never Allow);
//! - harmless programs keep the decision main had before the fix (observed at
//!   main 5bca9481 for C1-C3; for C4-C9 at a1fcab51 = main 5f208b7b plus the
//!   first 47b20647 commit, src of the second fix stashed; both modes): Allow;
//! - the raw-string shape `r'/etc/ho\<LF>sts'` (NOT /etc/hosts) keeps its
//!   decision at main 5bca9481: Allow, both modes.
//!
//! DOES NOT PROVE: escape forms other than BN and `\N{…}` (`\x2f`, octal, `\u`)
//! are judged on every path; shapes not listed here are safe; the Python FILE
//! judge (`python3 x.py` of a written file) reads `\N{…}` correctly.

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
        "bg-pbn-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "python-literal-bn-deny-20261113",
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

/// (label, command with BN inside a non-raw literal, plain-path twin).
const HARM: &[(&str, &str, &str)] = &[
    (
        "P1-single-quoted-shell-arg",
        "python3 -c 'open(\"/etc/ho\\\nsts\",\"w\")'",
        "python3 -c 'open(\"/etc/hosts\",\"w\")'",
    ),
    (
        "P2-double-quoted-shell-arg",
        "python3 -c \"open('/etc/ho\\\\\nsts','w')\"",
        "python3 -c \"open('/etc/hosts','w')\"",
    ),
    (
        "P3-assigned-variable",
        "python3 -c \"x='/etc/ho\\\\\nsts'; open(x,'w')\"",
        "python3 -c \"x='/etc/hosts'; open(x,'w')\"",
    ),
    (
        "T1-triple-single-quoted",
        "python3 -c \"open('''/etc/ho\\\\\nsts''','w')\"",
        "python3 -c \"open('''/etc/hosts''','w')\"",
    ),
    (
        "T2-triple-double-quoted",
        "python3 -c 'open(\"\"\"/etc/ho\\\nsts\"\"\",\"w\")'",
        "python3 -c 'open(\"\"\"/etc/hosts\"\"\",\"w\")'",
    ),
    (
        "B1-bytes-literal",
        "python3 -c \"open(b'/etc/ho\\\\\nsts','w')\"",
        "python3 -c \"open(b'/etc/hosts','w')\"",
    ),
    (
        "F1-fstring-without-placeholders",
        "python3 -c \"open(f'/etc/ho\\\\\nsts','w')\"",
        "python3 -c \"open(f'/etc/hosts','w')\"",
    ),
    (
        "S1-bn-inside-etc-prefix",
        "python3 -c \"open('/e\\\\\ntc/hosts','w')\"",
        "python3 -c \"open('/etc/hosts','w')\"",
    ),
    (
        "S2-two-bns",
        "python3 -c \"open('/\\\\\netc/ho\\\\\nsts','w')\"",
        "python3 -c \"open('/etc/hosts','w')\"",
    ),
    (
        "O1-os-chmod",
        "python3 -c \"import os; os.chmod('/etc/ho\\\\\nsts', 0o777)\"",
        "python3 -c \"import os; os.chmod('/etc/hosts', 0o777)\"",
    ),
    (
        "O2-os-remove",
        "python3 -c \"import os; os.remove('/etc/ho\\\\\nsts')\"",
        "python3 -c \"import os; os.remove('/etc/hosts')\"",
    ),
    (
        "H1-quoted-heredoc-stdin",
        "python3 - <<'PY'\nopen('/etc/ho\\\nsts','w')\nPY",
        "python3 - <<'PY'\nopen('/etc/hosts','w')\nPY",
    ),
    (
        "H2-dev-stdin-pipe-free-heredoc-os-remove",
        "python3 /dev/stdin <<'PY'\nimport os\nos.remove('/etc/ho\\\nsts')\nPY",
        "python3 /dev/stdin <<'PY'\nimport os\nos.remove('/etc/hosts')\nPY",
    ),
    (
        "H3-unquoted-heredoc-triple-quoted",
        "python3 - <<PY\nopen('''/etc/ho\\\\\nsts''','w')\nPY",
        "python3 - <<PY\nopen('''/etc/hosts''','w')\nPY",
    ),
    (
        "N1-named-escape-os-remove",
        "python3 -c \"import os; os.remove('\\N{SOLIDUS}etc/hosts')\"",
        "python3 -c \"import os; os.remove('/etc/hosts')\"",
    ),
    (
        "N2-named-escape-new-file-in-etc",
        "python3 -c \"open('/etc/\\N{SNOWMAN}','w')\"",
        "python3 -c \"open('/etc/x','w')\"",
    ),
    (
        "N3-named-escape-unquoted-heredoc",
        "python3 - <<PY\nopen('\\N{SOLIDUS}etc/hosts','w')\nPY",
        "python3 - <<PY\nopen('/etc/hosts','w')\nPY",
    ),
    (
        "N4-named-escape-dev-stdin",
        "python3 /dev/stdin <<'PY'\nopen('/etc/ho\\N{LATIN SMALL LETTER S}ts','w')\nPY",
        "python3 /dev/stdin <<'PY'\nopen('/etc/hosts','w')\nPY",
    ),
    (
        "U1-ascii-named-escape-in-directory-name",
        "python3 -c \"open('/\\N{LATIN SMALL LETTER E}tc/hosts','w')\"",
        "python3 -c \"open('/etc/hosts','w')\"",
    ),
    (
        "N5-capital-letter-digit-and-lowercase-names",
        "python3 -c \"open('/\\N{latin small letter e}tc/\\N{LATIN CAPITAL LETTER X}\\N{DIGIT SEVEN}','w')\"",
        "python3 -c \"open('/etc/X7','w')\"",
    ),
];

/// A NON-ASCII `\N{…}` (not decoded) inside a DIRECTORY name: no stand-in
/// reading names a system directory and the character cannot be known, so
/// the decision is Ask in the main session and Deny in a subagent — never
/// Allow. (`\N{SNOWMAN}` is not printable ASCII, so this path is not /etc.)
const UNDETERMINED_DIR: &[(&str, &str)] = &[(
    "U2-non-ascii-named-escape-in-directory-name",
    "python3 -c \"open('/\\N{SNOWMAN}tc/hosts','w')\"",
)];

/// (label, harmless command with BN inside a literal, decision at main 5bca9481).
const HARMLESS: &[(&str, &str, &str)] = &[
    ("C1-print", "python3 -c \"print('a\\\\\nb')\"", "allow"),
    (
        "C2-open-tmp",
        "python3 -c \"open('/tmp/x\\\\\ny','w')\"",
        "allow",
    ),
    (
        "C3-raw-string-not-etc-hosts",
        "python3 -c \"open(r'/etc/ho\\\\\nsts','w')\"",
        "allow",
    ),
    (
        "C4-print-named-escape",
        "python3 -c \"print('\\N{SNOWMAN}')\"",
        "allow",
    ),
    (
        "C5-open-tmp-named-escape",
        "python3 -c \"open('/tmp/\\N{SNOWMAN}','w')\"",
        "allow",
    ),
    (
        "C6-stdin-quoted-heredoc-print-bn",
        "python3 - <<'PY'\nprint('a\\\nb')\nPY",
        "allow",
    ),
    (
        "C7-stdin-unquoted-heredoc-print-bn",
        "python3 - <<PY\nprint('a\\\\\nb')\nPY",
        "allow",
    ),
    (
        "C8-dev-stdin-open-tmp-bn",
        "python3 /dev/stdin <<'PY'\nopen('/tmp/x\\\ny','w')\nPY",
        "allow",
    ),
    (
        "C9-stdin-print-named-escape",
        "python3 - <<'PY'\nprint('\\N{SNOWMAN}')\nPY",
        "allow",
    ),
];

fn check(cases: &[(&str, &str)], want: impl Fn(&str) -> &'static str) {
    let home = unique_home("chk");
    let mut failures = Vec::new();
    let mut runs = 0usize;
    for (label, command) in cases {
        for subagent in [false, true] {
            runs += 1;
            let (d, r) = run_as(command, &home, subagent);
            let w = want(label);
            println!("[{label}] subagent={subagent} {command:?} -> {d} (want {w}) | {r}");
            if d != w {
                failures.push(format!(
                    "[{label}] subagent={subagent} {command:?} -> {d} (want {w}) | {r}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {runs} run(s) had the wrong decision:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn bn_in_python_literal_system_harm_is_deny_in_both_modes() {
    let cases: Vec<(&str, &str)> = HARM.iter().map(|(l, bn, _)| (*l, *bn)).collect();
    check(&cases, |_| "deny");
}

#[test]
fn control_plain_path_twins_are_deny_in_both_modes() {
    let cases: Vec<(&str, &str)> = HARM.iter().map(|(l, _, plain)| (*l, *plain)).collect();
    check(&cases, |_| "deny");
}

#[test]
fn non_ascii_named_escape_in_directory_name_is_ask_in_main_and_deny_in_subagent() {
    let home = unique_home("und");
    let mut failures = Vec::new();
    for (label, command) in UNDETERMINED_DIR {
        for (subagent, want) in [(false, "ask"), (true, "deny")] {
            let (d, r) = run_as(command, &home, subagent);
            println!("[{label}] subagent={subagent} {command:?} -> {d} (want {want}) | {r}");
            if d != want {
                failures.push(format!(
                    "[{label}] subagent={subagent} -> {d} (want {want}) | {r}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn harmless_bn_literals_and_raw_string_keep_their_decision() {
    let cases: Vec<(&str, &str)> = HARMLESS.iter().map(|(l, c, _)| (*l, *c)).collect();
    check(&cases, |label| {
        HARMLESS
            .iter()
            .find(|(l, _, _)| *l == label)
            .map(|(_, _, w)| *w)
            .unwrap()
    });
}
