#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Shared harness for the answerlang integration tests. Every spawn gets a fresh
//! HOME, an explicit locale, a stub for `defaults`, and a session id.

use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn json(&self) -> Option<Value> {
        if self.stdout.trim().is_empty() {
            None
        } else {
            serde_json::from_str(self.stdout.trim()).ok()
        }
    }
    pub fn is_empty(&self) -> bool {
        self.stdout.trim().is_empty()
    }
    pub fn is_block(&self) -> bool {
        self.json()
            .map(|v| v.get("decision").and_then(Value::as_str) == Some("block"))
            .unwrap_or(false)
    }
    pub fn reason(&self) -> String {
        self.json()
            .and_then(|v| v.get("reason").and_then(Value::as_str).map(String::from))
            .unwrap_or_default()
    }
    pub fn system_message(&self) -> Option<String> {
        self.json().and_then(|v| {
            v.get("systemMessage")
                .and_then(Value::as_str)
                .map(String::from)
        })
    }
    pub fn context(&self) -> String {
        self.json()
            .and_then(|v| {
                v.get("hookSpecificOutput")
                    .and_then(|h| h.get("additionalContext"))
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .unwrap_or_default()
    }
    /// Parse `key = value` lines printed by `answerlang show`.
    pub fn show_map(&self) -> std::collections::BTreeMap<String, String> {
        self.stdout
            .lines()
            .filter_map(|l| {
                let (k, v) = l.split_once(" = ")?;
                Some((k.trim().to_string(), v.trim().to_string()))
            })
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Stub {
    /// prints ("ja-JP", "en-US")
    Ja,
    /// exits 1 (macOS step undetermined)
    Fail,
}

pub struct Env {
    pub dir: TempDir,
    pub lc_all: Option<String>,
    pub lc_messages: Option<String>,
    pub lang: Option<String>,
    pub stub: Stub,
    pub session: String,
}

impl Env {
    pub fn new() -> Self {
        Env {
            dir: tempfile::tempdir().unwrap(),
            lc_all: None,
            lc_messages: None,
            lang: Some("C".to_string()),
            stub: Stub::Fail,
            session: "sess-1".to_string(),
        }
    }
    pub fn with_lang(mut self, lang: &str) -> Self {
        self.lang = Some(lang.to_string());
        self
    }
    pub fn with_stub(mut self, s: Stub) -> Self {
        self.stub = s;
        self
    }
    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }
    pub fn config_path(&self) -> PathBuf {
        self.home().join(".answerlang").join("config.toml")
    }
    pub fn write_config(&self, body: &str) {
        let p = self.config_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    pub fn read_config(&self) -> Option<String> {
        std::fs::read_to_string(self.config_path()).ok()
    }
    pub fn counter_path(&self) -> PathBuf {
        self.dir.path().join("counter.txt")
    }
    pub fn stub_calls(&self) -> usize {
        std::fs::read_to_string(self.counter_path())
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
    fn write_stub(&self) -> PathBuf {
        let p = self.dir.path().join("defaults-stub.sh");
        let body = match self.stub {
            Stub::Ja => format!(
                "#!/bin/sh\necho called >> \"{}\"\nprintf '(\\n    \"ja-JP\",\\n    \"en-US\"\\n)\\n'\n",
                self.counter_path().display()
            ),
            Stub::Fail => format!(
                "#!/bin/sh\necho called >> \"{}\"\nexit 1\n",
                self.counter_path().display()
            ),
        };
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    pub fn run(&self, args: &[&str], stdin: Option<&Value>) -> Out {
        std::fs::create_dir_all(self.home()).unwrap();
        let stub = self.write_stub();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_answerlang"));
        cmd.args(args)
            .current_dir(self.dir.path())
            .env("HOME", self.home())
            .env("ANSWERLANG_DEFAULTS_CMD", &stub)
            .env("CLAUDE_CODE_SESSION_ID", &self.session)
            .env_remove("LC_ALL")
            .env_remove("LC_MESSAGES")
            .env_remove("LANG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(v) = &self.lc_all {
            cmd.env("LC_ALL", v);
        }
        if let Some(v) = &self.lc_messages {
            cmd.env("LC_MESSAGES", v);
        }
        if let Some(v) = &self.lang {
            cmd.env("LANG", v);
        }
        let mut child = cmd.spawn().unwrap();
        {
            let mut si = child.stdin.take().unwrap();
            if let Some(v) = stdin {
                let _ = si.write_all(v.to_string().as_bytes());
            }
        }
        let o = child.wait_with_output().unwrap();
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }

    pub fn session_start(&self, sid: &str) -> Out {
        self.run(
            &["session-start"],
            Some(
                &json!({"session_id": sid, "hook_event_name": "SessionStart", "source": "startup"}),
            ),
        )
    }
    pub fn prompt(&self, sid: &str, text: &str) -> Out {
        self.run(
            &["prompt"],
            Some(
                &json!({"session_id": sid, "hook_event_name": "UserPromptSubmit", "prompt": text}),
            ),
        )
    }
    pub fn stop(&self, sid: &str, transcript: Option<&Path>, active: bool) -> Out {
        let mut p =
            json!({"session_id": sid, "hook_event_name": "Stop", "stop_hook_active": active});
        if let Some(t) = transcript {
            p["transcript_path"] = json!(t.to_string_lossy());
        }
        self.run(&["stop"], Some(&p))
    }
    pub fn transcript(&self, name: &str, entries: &[Value]) -> PathBuf {
        let p = self.dir.path().join(name);
        let body: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
        std::fs::write(&p, body.join("\n") + "\n").unwrap();
        p
    }
    /// Transcript whose last assistant text is `text` (preceded by a user entry).
    pub fn answer_transcript(&self, name: &str, text: &str) -> PathBuf {
        self.transcript(name, &[user("do the thing"), assistant(text)])
    }
}

pub fn assistant(text: &str) -> Value {
    json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":text}]}})
}
pub fn user(text: &str) -> Value {
    json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":text}]}})
}

pub const JA_LINE: &str = "Answers and conclusions: write them in japanese. Keep code, identifiers, commit messages and verbatim quotes (gate messages, test output) in their original form.";

/// ~`n` Japanese letters of prose (hiragana/kanji).
pub fn ja(n: usize) -> String {
    let unit = "これは日本語の文章であり結果を報告します";
    let mut s = String::new();
    while s.chars().count() < n {
        s.push_str(unit);
    }
    s.chars().take(n).collect()
}

/// English prose with at least `n` letters.
pub fn en(n: usize) -> String {
    let unit = "This is an English answer that reports the result of the work in detail. ";
    let mut s = String::new();
    while s.chars().filter(|c| c.is_alphabetic()).count() < n {
        s.push_str(unit);
    }
    s
}

pub const JA_CONFIG: &str = "language = \"japanese\"\n";
