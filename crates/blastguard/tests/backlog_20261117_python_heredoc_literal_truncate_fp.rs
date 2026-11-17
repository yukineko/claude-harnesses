#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! FP RED (observed 2026-11-17 while editing a test file): a `python3 -`
//! quoted heredoc whose body only holds the word `truncate -s 0 /tmp/x`
//! inside a Python string literal is refused with "truncate can shrink a file
//! to zero bytes". Nothing in the command runs `truncate` as a shell command:
//! the heredoc body is Python source fed to `python3` on stdin, and the
//! word is a string literal there (memory: heredoc body judged by target
//! language; `.py` / python stdin is not shell).
//!
//! The minimal shape (`print("truncate -s 0 /tmp/x")`) is ALREADY Allow
//! (control). The refused shape is pinned verbatim in `REFUSED`; which part
//! of its body triggers the reading is NOT isolated here.
//!
//! Main session only.
//!
//! DOES NOT PROVE: subagent behaviour, unquoted heredocs, a body that really
//! writes a file, other words than `truncate`, or which rule fires.
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

fn unique_home() -> Home {
    let dir = std::env::temp_dir().join(format!("bg-pyhd-trunc-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// `(decision, reason)`. The empty-stdout = Allow branch is reached only
/// after the exit-status assertion (a crashed hook is never an Allow).
fn run_main(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "python-heredoc-literal-truncate-fp-20261117",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
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
    assert!(
        out.status.success(),
        "hook exited {:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
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

/// Minimal shape suggested for the repro. Observed 2026-11-17: this one is
/// ALREADY Allow (control), so it is not the shape that was refused.
#[test]
fn python_heredoc_string_literal_truncate_word_allows() {
    let home = unique_home();
    let command = "python3 - <<'EOF'\nprint(\"truncate -s 0 /tmp/x\")\nEOF";
    let (d, r) = run_main(command, &home);
    assert_eq!(d, "allow", "{command:?} expected Allow, got {d} ({r})");
}

/// The EXACT command that was refused on 2026-11-17 (recovered verbatim from
/// the session transcript; the hook only judges it, nothing is run). Its
/// heredoc body is Python that rewrites ignore labels in a test file.
const REFUSED: &str = r##"cd /Users/yuki/src/.harness-worktrees/bg-s2-red-20261117 && python3 - <<'EOF'
p = "crates/blastguard/tests/ast_effect_rules_20261117.rs"
s = open(p).read()
rep = [
 ('// test-weakening-justified: new RED for unbuilt S3 behaviour (2026-11-17)\n#[test]\n#[ignore = "RED: S3 Table C Scratch x Truncate truncate (S6 if the legacy confined Ask stays raise-only)"]',
  '// test-weakening-justified: new RED for unbuilt S6 behaviour (2026-11-17)\n#[test]\n#[ignore = "RED: S6 Table C Scratch x Truncate truncate (legacy confined Ask raise-only until S6, ruling 14)"]'),
 ('// test-weakening-justified: new RED for unbuilt S3 behaviour (2026-11-17)\n#[test]\n#[ignore = "RED: S3 Table C Scratch x Truncate dd of= (S6 if the legacy dd reading stays raise-only)"]',
  '// test-weakening-justified: new RED for unbuilt S6 behaviour (2026-11-17)\n#[test]\n#[ignore = "RED: S6 Table C Scratch x Truncate dd of= (legacy dd reading raise-only until S6, ruling 14)"]'),
 ('// test-weakening-justified: new RED for unbuilt S3 behaviour (2026-11-17)\n#[test]\n#[ignore = "RED: S3 Table C Scratch x Delete find -delete (S6 if the legacy confined Ask stays raise-only)"]',
  '// test-weakening-justified: new RED for unbuilt S6 behaviour (2026-11-17)\n#[test]\n#[ignore = "RED: S6 Table C Scratch x Delete find -delete (legacy confined Ask raise-only until S6, ruling 14)"]'),
 ('// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)\n#[test]\n#[ignore = "RED: S4 B.5 . file Ask (ruling 13)"]',
  '// test-weakening-justified: new RED for unbuilt quarantine-plugin behaviour (2026-11-17)\n#[test]\n#[ignore = "RED: quarantine B.5 . file Ask together with the quarantine plugin (rulings 13, 16)"]'),
 ('// test-weakening-justified: new RED for unbuilt S4 behaviour (2026-11-17)\n#[test]\n#[ignore = "RED: S4 B.5 source file Ask (ruling 13)"]',
  '// test-weakening-justified: new RED for unbuilt quarantine-plugin behaviour (2026-11-17)\n#[test]\n#[ignore = "RED: quarantine B.5 source file Ask together with the quarantine plugin (rulings 13, 16)"]'),
 ('''//! Stage labels on `dd` / `truncate` / `find -delete` Scratch twins say "S6
//! if ...": SUBSET.md "Undetermined handling" 3 keeps legacy readings
//! raise-only until S6, and today's legacy answer there is Deny / Ask, so
//! S3 alone may not turn them GREEN.''',
  '''//! Stage labels on the `dd` / `truncate` / `find -delete` Scratch twins are
//! S6: ruling 14 keeps the whole legacy string layer raise-only until S6, and
//! today's legacy answer there is Deny / Ask. The `.` / `source` REDs are
//! labelled `quarantine` (ruling 16: they become Ask together with the
//! quarantine plugin).'''),
]
for a, b in rep:
    assert s.count(a) == 1, a[:120]
    s = s.replace(a, b)
open(p, "w").write(s)
EOF
grep -n "S<n>" crates/blastguard/tests/ast_effect_rules_20261117.rs"##;

/// The refused command must not be DENIED with the shell-`truncate` reason:
/// no shell word of it runs `truncate` (the only `truncate` words are inside
/// Python string literals of the heredoc body). Ask is not asserted against:
/// python stdin is legacy until S5 (ruling 15) and its own file write may be
/// a question; the FP is the Deny for a command it does not contain.
/// Observed 2026-11-17 (probe): decision=deny reason="truncate can shrink a
/// file to zero bytes".
// test-weakening-justified: new RED for an observed FP (2026-11-17)
#[test]
#[ignore = "RED: python heredoc literal read as shell truncate"]
fn python_heredoc_label_rewrite_is_not_denied_as_truncate() {
    let home = unique_home();
    let (d, r) = run_main(REFUSED, &home);
    assert!(
        !(d == "deny" && r.contains("truncate can shrink")),
        "refused command denied as shell truncate: {d} ({r})"
    );
}
