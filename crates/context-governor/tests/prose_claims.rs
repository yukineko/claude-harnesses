//! Prose-claim reproduction tests for context-governor README claims.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Env {
    state: tempfile::TempDir,
    home: tempfile::TempDir,
    cwd: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        Env {
            state: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
            cwd: tempfile::tempdir().unwrap(),
        }
    }
    fn cwd_str(&self) -> String {
        self.cwd.path().to_str().unwrap().to_string()
    }
    fn cmd(&self, cwd: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_context-governor"));
        c.env("CONTEXT_GOVERNOR_STATE_DIR", self.state.path())
            .env("HOME", self.home.path())
            .env("CLAUDE_CODE_SESSION_ID", "prose-claims")
            .env_remove("CONTEXT_GOVERNOR_GROOM_BUDGET")
            .env_remove("CONTEXT_GOVERNOR_REFERENCE_DOC")
            .current_dir(cwd);
        c
    }
    /// (exit code, stdout)
    fn hook(&self, payload: &serde_json::Value) -> (i32, String) {
        let mut child = self
            .cmd(self.cwd.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )
    }
    fn rollup(&self, cwd: &Path) -> (i32, String) {
        let out = self.cmd(cwd).arg("rollup").output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    }
    fn payload(&self, event: &str) -> serde_json::Value {
        serde_json::json!({
            "session_id": "prose-claims",
            "transcript_path": "",
            "cwd": self.cwd_str(),
            "hook_event_name": event,
        })
    }
    fn big_post_tool_use(&self, body: &str) -> serde_json::Value {
        let mut p = self.payload("PostToolUse");
        p["tool_name"] = "Bash".into();
        p["tool_response"] = body.into();
        p
    }
}

fn find_files(root: &Path, name: &str, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                find_files(&p, name, out);
            } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
                out.push(p);
            }
        }
    }
}

/// Find the updatedToolOutput string anywhere in the envelope.
fn updated_output(stdout: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(stdout).expect("json envelope");
    fn walk(v: &serde_json::Value) -> Option<String> {
        match v {
            serde_json::Value::Object(m) => {
                if let Some(s) = m.get("updatedToolOutput").and_then(|s| s.as_str()) {
                    return Some(s.to_string());
                }
                m.values().find_map(walk)
            }
            _ => None,
        }
    }
    walk(&v).unwrap_or_else(|| panic!("no updatedToolOutput in {stdout}"))
}

#[test]
fn claim_non_compact_events_exit_zero_with_envelope() {
    let e = Env::new();
    let mut payloads = vec![];
    let mut p = e.payload("PostToolUse");
    p["tool_name"] = "Read".into();
    p["tool_response"] = "tiny".into();
    payloads.push(p);
    let mut p = e.payload("UserPromptSubmit");
    p["prompt"] = "hi".into();
    payloads.push(p);
    let mut p = e.payload("SessionStart");
    p["source"] = "startup".into();
    payloads.push(p);
    payloads.push(e.payload("Stop"));
    payloads.push(e.payload("SubagentStop"));

    for p in &payloads {
        let ev = p["hook_event_name"].as_str().unwrap();
        let (code, stdout) = e.hook(p);
        assert_eq!(code, 0, "{ev} must exit 0; stdout={stdout}");
        let v: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|_| panic!("{ev}: stdout not JSON: {stdout}"));
        assert!(v.is_object(), "{ev}: not an object: {stdout}");
    }
}

#[test]
fn claim_subagent_stop_never_blocks() {
    let e = Env::new();
    for ev in ["Stop", "SubagentStop"] {
        let (code, stdout) = e.hook(&e.payload(ev));
        assert_eq!(code, 0, "{ev}");
        assert_eq!(stdout, "{}", "{ev}");
    }
}

#[test]
fn claim_groomer_is_elision_not_summary() {
    let e = Env::new();
    // Distinct chars so a prefix/suffix split is unambiguous.
    let body: String = (0..40_000u32)
        .map(|i| char::from_u32(0x4e00 + (i % 5000)).unwrap())
        .collect();
    let (code, stdout) = e.hook(&e.big_post_tool_use(&body));
    assert_eq!(code, 0);
    let out = updated_output(&stdout);
    let marker_start = out.find("\n…[context-governor: elided ").expect("marker");
    let close = "]…\n";
    let close_at = out[marker_start + 1..].find(close).expect("marker end") + marker_start + 1;
    let head = &out[..marker_start];
    let tail = &out[close_at + close.len()..];
    let marker = &out[marker_start + 1..close_at + close.len() - 1];
    assert!(!head.is_empty() && !tail.is_empty());
    assert!(body.starts_with(head), "head is not a verbatim prefix");
    assert!(body.ends_with(tail), "tail is not a verbatim suffix");
    let elided = body.chars().count() - head.chars().count() - tail.chars().count();
    assert!(elided > 0);
    assert_eq!(
        marker,
        format!(
            "…[context-governor: elided {elided} chars of {}]…",
            body.chars().count()
        )
    );
    assert!(out.chars().count() < body.chars().count());
}

#[test]
fn claim_ledger_path_under_state_dir() {
    let e = Env::new();
    let (code, stdout) = e.hook(&e.big_post_tool_use(&"z".repeat(40_000)));
    assert_eq!(code, 0, "{stdout}");
    let mut found = vec![];
    find_files(e.state.path(), "ledger.jsonl", &mut found);
    assert_eq!(found.len(), 1, "ledger files: {found:?}");
    let rel = found[0].strip_prefix(e.state.path()).unwrap();
    // ACTUAL: <state_dir>/<project_key>/<session>/ledger.jsonl (the README
    // claims <state_dir>/ledger.jsonl, which is FALSE).
    let comps: Vec<_> = rel.components().map(|c| c.as_os_str().to_owned()).collect();
    assert_eq!(comps.len(), 3, "unexpected ledger location: {rel:?}");
    assert!(!comps[0].is_empty());
    assert_eq!(comps[1], "prose-claims");
    assert_eq!(comps[2], "ledger.jsonl");
    let text = std::fs::read_to_string(&found[0]).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1);
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(v["event"], "groomed");
}

#[test]
fn claim_rollup_read_only_deterministic() {
    let e = Env::new();
    let (code, _) = e.hook(&e.big_post_tool_use(&"z".repeat(40_000)));
    assert_eq!(code, 0);
    let mut found = vec![];
    find_files(e.state.path(), "ledger.jsonl", &mut found);
    assert_eq!(found.len(), 1);
    let before = std::fs::read(&found[0]).unwrap();

    let (c1, o1) = e.rollup(e.cwd.path());
    let (c2, o2) = e.rollup(e.cwd.path());
    assert_eq!((c1, c2), (0, 0));
    assert_eq!(o1, o2, "rollup not deterministic");
    assert!(o1.contains("  rows: 1\n"), "{o1}");
    assert!(o1.contains("total_saved_tokens: "), "{o1}");
    assert!(!o1.contains("total_saved_tokens: 0\n"), "{o1}");
    assert!(o1.contains("    groomed: 1\n"), "{o1}");
    assert_eq!(std::fs::read(&found[0]).unwrap(), before, "ledger mutated");
}

#[test]
fn claim_hooks_json_registration() {
    let raw =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/hooks/hooks.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let hooks = v["hooks"].as_object().unwrap();
    let mut keys: Vec<&str> = hooks.keys().map(|s| s.as_str()).collect();
    keys.sort();
    let mut want = vec![
        "PostToolUse",
        "PreCompact",
        "SessionStart",
        "Stop",
        "SubagentStop",
        "UserPromptSubmit",
    ];
    want.sort();
    assert_eq!(keys, want);
    assert_eq!(
        hooks["PostToolUse"][0]["matcher"],
        "Read|Bash|Grep|Glob|WebFetch|BashOutput|NotebookRead|Task"
    );
    for (ev, entries) in hooks {
        for ent in entries.as_array().unwrap() {
            for h in ent["hooks"].as_array().unwrap() {
                assert_eq!(
                    h["command"], "${CLAUDE_PLUGIN_ROOT}/bin/context-governor",
                    "{ev}"
                );
            }
        }
    }
}
