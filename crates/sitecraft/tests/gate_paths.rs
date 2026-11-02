#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Black-box tests for `sitecraft gate` (the Stop hook), written by an
//! independent test author (CLAUDE.md §2(a)).
//!
//! Contract under test (Claude Code Stop-hook protocol — stdout JSON is only
//! honoured on exit 0, so a block is exit 0 plus a JSON line):
//!   ALLOW = exit 0 and no `"decision":"block"` line on stdout.
//!   BLOCK = exit 0 and a stdout line `{"decision":"block","reason":"…"}`.
//!
//! Every test starts from a fully clean baseline (trusted `sitecraft.toml`,
//! every decision recorded, clean build output and sources, an `npm audit`
//! stub reporting zero vulnerabilities, a transcript without web edits) and
//! perturbs exactly one dimension, so a block can only come from that one.
//!
//! Pages run only when this session's transcript shows a Write/Edit to a file
//! matching `web_globs` (user ruling), or when that cannot be determined
//! (transcript missing, unreadable, unparseable, or no `transcript_path`).
//! Whether pages ran is observed through the `node` stub's marker file.
//!
//! Stubs: `SITECRAFT_NODE` / `SITECRAFT_NPM` point at shell scripts written
//! into the per-test tempdir; the dev server is a shell script (marker, then
//! `python3 -m http.server`) named by `sitecraft.toml`'s `command`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Catalog area ids (pinned by the decide tests).
const AREAS: &[&str] = &[
    "auth",
    "users-roles",
    "admin",
    "storage",
    "data-analytics",
    "web-analytics",
    "processing",
    "workflow",
    "notification",
    "ai",
    "formats",
    "exposure",
];

/// Hard ceiling on one `sitecraft gate` run, so a hanging gate fails the test
/// instead of hanging the suite.
const GATE_DEADLINE: Duration = Duration::from_secs(180);

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    state: PathBuf,
    home: PathBuf,
    port: u16,
    node_stub: PathBuf,
    node_marker: PathBuf,
    npm_stub: PathBuf,
    npm_marker: PathBuf,
    dev_marker: PathBuf,
    transcript: PathBuf,
}

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    l.local_addr().expect("local addr").port()
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, content).expect("write file");
}

fn write_exec(path: &Path, content: &str) {
    write(path, content);
    let st = Command::new("chmod")
        .arg("755")
        .arg(path)
        .status()
        .expect("chmod");
    assert!(st.success(), "chmod {}", path.display());
}

fn audit_json(high: u32, critical: u32) -> String {
    format!(
        "{{\"auditReportVersion\":2,\"vulnerabilities\":{{}},\"metadata\":{{\"vulnerabilities\":{{\"info\":0,\"low\":0,\"moderate\":0,\"high\":{high},\"critical\":{critical},\"total\":{}}},\"dependencies\":{{\"prod\":1,\"dev\":0,\"optional\":0,\"peer\":0,\"peerOptional\":0,\"total\":1}}}}}}",
        high + critical
    )
}

/// Whatever exact schema `check pages` settles on, this payload reports a
/// console error on one checked page; a correct implementation must block on
/// it (as a violation, or as undetermined if the shape differs) — never allow.
fn pages_findings_json(port: u16) -> String {
    format!(
        "{{\"checked\":1,\"pages\":[\"http://127.0.0.1:{port}/\"],\"findings\":[{{\"page\":\"http://127.0.0.1:{port}/\",\"kind\":\"console-error\",\"detail\":\"Uncaught TypeError: boom\"}}]}}"
    )
}

/// A transcript line: an assistant message with one tool_use block.
fn tool_use_line(id: &str, tool: &str, input: serde_json::Value) -> String {
    serde_json::json!({
        "type": "assistant",
        "sessionId": "s",
        "message": {
            "role": "assistant",
            "content": [
                {"type": "tool_use", "id": id, "name": tool, "input": input}
            ]
        }
    })
    .to_string()
}

fn user_line(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "sessionId": "s",
        "message": {"role": "user", "content": text}
    })
    .to_string()
}

/// Clean baseline (see module docs). Trust is NOT recorded here; tests call
/// [`Fixture::trust`] (or deliberately do not).
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Canonical so the transcript's absolute paths and the payload cwd agree
    // (macOS /var → /private/var).
    let base = tmp.path().canonicalize().expect("canonicalize tempdir");
    let root = base.join("site");
    let state = base.join("state");
    let home = base.join("home");
    for d in [&root, &state, &home] {
        std::fs::create_dir_all(d).expect("mkdir");
    }
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "t@example.com"]);
    git(&root, &["config", "user.name", "t"]);

    let port = free_port();
    let f = Fixture {
        node_stub: base.join("stubs/node"),
        node_marker: base.join("markers/node-ran"),
        npm_stub: base.join("stubs/npm"),
        npm_marker: base.join("markers/npm-ran"),
        dev_marker: base.join("markers/dev-ran"),
        transcript: base.join("transcript.jsonl"),
        _tmp: tmp,
        base,
        root,
        state,
        home,
        port,
    };
    std::fs::create_dir_all(f.base.join("markers")).expect("mkdir markers");

    // Dev server stub: marker, then a real HTTP server on the configured port.
    let dev = f.base.join("stubs/dev-server.sh");
    write_exec(
        &dev,
        &format!(
            "#!/bin/sh\n: > '{marker}'\nexec python3 -m http.server {port} --bind 127.0.0.1 --directory '{root}'\n",
            marker = f.dev_marker.display(),
            port = f.port,
            root = f.root.display()
        ),
    );

    write(
        &f.root.join("sitecraft.toml"),
        &format!(
            "command = [\"{dev}\"]\nport = {port}\nready_path = \"/\"\nready_timeout_secs = 30\npages = [\"/\"]\nbuild_dir = \"dist\"\nweb_globs = [\"src/**\", \"index.html\"]\n",
            dev = dev.display(),
            port = f.port
        ),
    );
    f.write_decisions_complete();
    write(
        &f.root.join("index.html"),
        "<!doctype html><title>t</title>\n",
    );
    write(&f.root.join("src/main.js"), "export const answer = 42;\n");
    write(&f.root.join("dist/app.js"), "console.log('ok');\n");
    write(
        &f.root.join("package.json"),
        "{\"name\":\"t\",\"version\":\"0.0.0\",\"private\":true}\n",
    );
    write(
        &f.root.join("package-lock.json"),
        "{\"name\":\"t\",\"version\":\"0.0.0\",\"lockfileVersion\":3,\"requires\":true,\"packages\":{}}\n",
    );
    git(&f.root, &["add", "-A"]);
    git(&f.root, &["commit", "-q", "--no-verify", "-m", "init"]);

    f.npm_stub(&audit_json(0, 0), 0);
    f.node_stub(&pages_findings_json(f.port), 0);
    f.transcript_without_web_edits();
    f
}

impl Fixture {
    fn write_decisions_complete(&self) {
        let mut s = String::new();
        for a in AREAS {
            s.push_str(&format!("[areas.{a}]\nrelevance = \"irrelevant\"\n\n"));
        }
        write(&self.root.join("sitecraft/decisions.toml"), &s);
    }

    /// `npm` stub: marker, print `stdout_text`, exit `code`.
    fn npm_stub(&self, stdout_text: &str, code: i32) {
        write_exec(
            &self.npm_stub,
            &format!(
                "#!/bin/sh\n: > '{}'\ncat <<'SITECRAFT_EOF'\n{}\nSITECRAFT_EOF\nexit {}\n",
                self.npm_marker.display(),
                stdout_text,
                code
            ),
        );
    }

    /// `node` stub: marker, drain stdin, print `stdout_text`, exit `code`.
    fn node_stub(&self, stdout_text: &str, code: i32) {
        write_exec(
            &self.node_stub,
            &format!(
                "#!/bin/sh\n: > '{}'\ncat >/dev/null 2>&1\ncat <<'SITECRAFT_EOF'\n{}\nSITECRAFT_EOF\nexit {}\n",
                self.node_marker.display(),
                stdout_text,
                code
            ),
        );
    }

    fn abs(&self, rel: &str) -> String {
        self.root.join(rel).display().to_string()
    }

    /// Transcript with a Write to a NON-web file and a Read (not a write) of a
    /// web file: neither may trigger pages.
    fn transcript_without_web_edits(&self) {
        let lines = [
            user_line("update the readme"),
            tool_use_line(
                "t1",
                "Read",
                serde_json::json!({"file_path": self.abs("src/main.js")}),
            ),
            tool_use_line(
                "t2",
                "Write",
                serde_json::json!({"file_path": self.abs("README.md"), "content": "hi\n"}),
            ),
        ];
        write(&self.transcript, &(lines.join("\n") + "\n"));
    }

    fn transcript_with(&self, tool: &str, rel: &str) {
        let input = match tool {
            "Write" => serde_json::json!({"file_path": self.abs(rel), "content": "x\n"}),
            _ => serde_json::json!({
                "file_path": self.abs(rel),
                "old_string": "42",
                "new_string": "43"
            }),
        };
        let lines = [
            user_line("change the page"),
            tool_use_line("t1", tool, input),
        ];
        write(&self.transcript, &(lines.join("\n") + "\n"));
    }

    fn trust(&self) {
        let out = Command::new(env!("CARGO_BIN_EXE_sitecraft"))
            .arg("--root")
            .arg(&self.root)
            .arg("trust")
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("SITECRAFT_STATE_DIR", &self.state)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("spawn sitecraft trust");
        assert!(
            out.status.success(),
            "setup: `sitecraft trust` failed (the gate cannot be exercised past trust)\n{}",
            describe(&out)
        );
    }

    fn payload(&self, session: &str, active: bool) -> String {
        serde_json::json!({
            "session_id": session,
            "transcript_path": self.transcript.display().to_string(),
            "cwd": self.root.display().to_string(),
            "hook_event_name": "Stop",
            "stop_hook_active": active,
        })
        .to_string()
    }

    fn payload_without_transcript(&self, session: &str) -> String {
        serde_json::json!({
            "session_id": session,
            "cwd": self.root.display().to_string(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string()
    }

    fn gate(&self, session: &str, active: bool) -> Output {
        self.gate_raw(session, &self.payload(session, active))
    }

    fn gate_raw(&self, session: &str, stdin_text: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sitecraft"))
            .arg("gate")
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("SITECRAFT_STATE_DIR", &self.state)
            .env("SITECRAFT_NODE", &self.node_stub)
            .env("SITECRAFT_NPM", &self.npm_stub)
            .env("CLAUDE_PLUGIN_ROOT", env!("CARGO_MANIFEST_DIR"))
            .env("CLAUDE_CODE_SESSION_ID", session)
            .env_remove("SITECRAFT_MARKETPLACE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sitecraft gate");
        {
            let mut stdin = child.stdin.take().expect("stdin");
            stdin.write_all(stdin_text.as_bytes()).expect("write stdin");
        }
        let mut so = child.stdout.take().expect("stdout");
        let mut se = child.stderr.take().expect("stderr");
        let t_out = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = so.read_to_end(&mut v);
            v
        });
        let t_err = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = se.read_to_end(&mut v);
            v
        });
        let start = Instant::now();
        let status = loop {
            if let Some(st) = child.try_wait().expect("try_wait") {
                break st;
            }
            if start.elapsed() > GATE_DEADLINE {
                let _ = child.kill();
                let _ = child.wait();
                panic!("sitecraft gate did not finish within {GATE_DEADLINE:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Output {
            status,
            stdout: t_out.join().expect("stdout reader"),
            stderr: t_err.join().expect("stderr reader"),
        }
    }

    fn pages_ran(&self) -> bool {
        self.node_marker.exists()
    }

    fn dev_server_ran(&self) -> bool {
        self.dev_marker.exists()
    }

    fn port_is_listening(&self) -> bool {
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", self.port).parse().expect("addr"),
            Duration::from_millis(500),
        )
        .is_ok()
    }

    /// Lines mentioning `not-configured` across every file under the state dir.
    fn not_configured_lines(&self) -> usize {
        fn walk(dir: &Path, n: &mut usize) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, n);
                } else if let Ok(s) = std::fs::read_to_string(&p) {
                    *n += s.lines().filter(|l| l.contains("not-configured")).count();
                }
            }
        }
        let mut n = 0;
        walk(&self.state, &mut n);
        n
    }
}

fn describe(out: &Output) -> String {
    format!(
        "status={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Stdout lines that parse as a JSON object.
fn json_lines(out: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .filter(serde_json::Value::is_object)
        .collect()
}

fn block_reason(out: &Output) -> Option<String> {
    json_lines(out).into_iter().find_map(|v| {
        if v.get("decision").and_then(|d| d.as_str()) == Some("block") {
            v.get("reason").and_then(|r| r.as_str()).map(str::to_string)
        } else {
            None
        }
    })
}

/// Asserts BLOCK (exit 0 + `{"decision":"block","reason":…}`) and returns the
/// lowercased reason.
fn assert_block(out: &Output) -> String {
    assert_eq!(
        out.status.code(),
        Some(0),
        "BLOCK must exit 0 (Claude Code ignores stdout JSON on a non-zero exit)\n{}",
        describe(out)
    );
    let reason = block_reason(out).unwrap_or_else(|| {
        panic!(
            "expected a stdout line {{\"decision\":\"block\",\"reason\":…}}\n{}",
            describe(out)
        )
    });
    assert!(
        !reason.trim().is_empty(),
        "block reason is empty\n{}",
        describe(out)
    );
    reason.to_lowercase()
}

fn assert_allow(out: &Output) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit 0\n{}",
        describe(out)
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("\"decision\""),
        "expected ALLOW (no decision on stdout)\n{}",
        describe(out)
    );
}

// ---------------------------------------------------------------- not configured

#[test]
fn no_sitecraft_toml_allows_and_appends_one_not_configured_line() {
    let f = fixture();
    std::fs::remove_file(f.root.join("sitecraft.toml")).expect("rm config");
    git(&f.root, &["add", "-A"]);
    git(&f.root, &["commit", "-q", "--no-verify", "-m", "no config"]);
    f.transcript_with("Write", "src/main.js");

    let out = f.gate("s-noconf", false);
    assert_allow(&out);
    assert_eq!(
        f.not_configured_lines(),
        1,
        "exactly one not-configured line must be appended under the state dir\n{}",
        describe(&out)
    );
    assert!(!f.pages_ran(), "an unconfigured repo must not run pages");

    let out2 = f.gate("s-noconf", false);
    assert_allow(&out2);
    assert_eq!(
        f.not_configured_lines(),
        2,
        "a second unconfigured stop appends (does not overwrite)\n{}",
        describe(&out2)
    );
}

// ---------------------------------------------------------------- all clean

#[test]
fn all_clean_without_web_edits_allows_and_skips_pages() {
    let f = fixture();
    f.trust();
    let out = f.gate("s-clean", false);
    assert_allow(&out);
    assert!(
        !f.pages_ran(),
        "no Write/Edit matching web_globs (only a Read of src/main.js and a Write of README.md): pages must not run\n{}",
        describe(&out)
    );
    assert!(
        !f.dev_server_ran(),
        "pages skipped: the dev server must not be started\n{}",
        describe(&out)
    );
    assert!(
        f.npm_marker.exists(),
        "security (npm audit) must still run when pages are skipped\n{}",
        describe(&out)
    );
}

// ---------------------------------------------------------------- trust

#[test]
fn untrusted_config_blocks_with_trust_guidance_and_spawns_nothing() {
    let f = fixture();
    f.transcript_with("Write", "src/main.js");
    let out = f.gate("s-untrusted", false);
    let reason = assert_block(&out);
    assert!(
        reason.contains("sitecraft trust"),
        "reason must tell the user to run `sitecraft trust`\n{}",
        describe(&out)
    );
    assert!(
        !f.dev_server_ran(),
        "an untrusted command must not be spawned\n{}",
        describe(&out)
    );
    assert!(
        !f.pages_ran(),
        "pages need the untrusted dev server: node must not run\n{}",
        describe(&out)
    );
}

#[test]
fn config_changed_after_trust_blocks_with_trust_guidance() {
    let f = fixture();
    f.trust();
    let cfg = f.root.join("sitecraft.toml");
    let mut s = std::fs::read_to_string(&cfg).expect("read config");
    s.push_str("# edited after trust\n");
    std::fs::write(&cfg, s).expect("rewrite config");
    let out = f.gate("s-changed", false);
    let reason = assert_block(&out);
    assert!(
        reason.contains("sitecraft trust"),
        "reason must tell the user to run `sitecraft trust`\n{}",
        describe(&out)
    );
}

// ---------------------------------------------------------------- decisions

#[test]
fn incomplete_decisions_block_with_decide_next_guidance() {
    let f = fixture();
    f.trust();
    // Only one area judged: the other eleven are pending.
    write(
        &f.root.join("sitecraft/decisions.toml"),
        "[areas.auth]\nrelevance = \"irrelevant\"\n",
    );
    let out = f.gate("s-decide", false);
    let reason = assert_block(&out);
    assert!(
        reason.contains("decide next"),
        "reason must point at `sitecraft decide next`\n{}",
        describe(&out)
    );
}

#[test]
fn missing_decisions_record_blocks() {
    let f = fixture();
    f.trust();
    std::fs::remove_file(f.root.join("sitecraft/decisions.toml")).expect("rm decisions");
    let out = f.gate("s-nodecide", false);
    let reason = assert_block(&out);
    assert!(
        reason.contains("decide"),
        "reason must point at the decide workflow\n{}",
        describe(&out)
    );
}

// ---------------------------------------------------------------- security

#[test]
fn security_high_vulnerability_blocks() {
    let f = fixture();
    f.trust();
    f.npm_stub(&audit_json(1, 0), 1);
    let out = f.gate("s-sechigh", false);
    let reason = assert_block(&out);
    assert!(
        f.npm_marker.exists(),
        "npm stub must have been run\n{}",
        describe(&out)
    );
    assert!(
        reason.contains("high") || reason.contains("audit"),
        "reason must name the npm audit finding\n{}",
        describe(&out)
    );
}

#[test]
fn security_undetermined_blocks_and_says_undetermined() {
    let f = fixture();
    f.trust();
    // ENOLOCK-style error object: `check security` cannot conclude (exit 2).
    f.npm_stub(
        "{\"error\":{\"code\":\"ENOLOCK\",\"summary\":\"This command requires an existing lockfile.\",\"detail\":\"\"}}",
        1,
    );
    let out = f.gate("s-secundet", false);
    let reason = assert_block(&out);
    assert!(
        reason.contains("undetermined"),
        "an undetermined check must say so in the reason\n{}",
        describe(&out)
    );
}

#[test]
fn security_empty_audit_stdout_blocks_undetermined() {
    let f = fixture();
    f.trust();
    f.npm_stub("", 0);
    let out = f.gate("s-secempty", false);
    let reason = assert_block(&out);
    assert!(
        reason.contains("undetermined"),
        "empty npm audit stdout is undetermined, never clean\n{}",
        describe(&out)
    );
}

// ---------------------------------------------------------------- pages selection

#[test]
fn write_to_web_file_runs_pages_and_findings_block_and_server_is_stopped() {
    let f = fixture();
    f.trust();
    f.transcript_with("Write", "src/main.js");
    let out = f.gate("s-webwrite", false);
    assert_block(&out);
    assert!(
        f.dev_server_ran(),
        "a web edit must start the dev server for pages\n{}",
        describe(&out)
    );
    assert!(
        f.pages_ran(),
        "a Write matching web_globs must run the pages check (node stub marker)\n{}",
        describe(&out)
    );
    assert!(
        !f.port_is_listening(),
        "the gate must stop the dev server it started\n{}",
        describe(&out)
    );
}

#[test]
fn edit_to_web_file_runs_pages() {
    let f = fixture();
    f.trust();
    f.transcript_with("Edit", "index.html");
    let out = f.gate("s-webedit", false);
    assert_block(&out);
    assert!(
        f.pages_ran(),
        "an Edit matching web_globs (index.html) must run pages\n{}",
        describe(&out)
    );
    assert!(
        !f.port_is_listening(),
        "the gate must stop the dev server it started\n{}",
        describe(&out)
    );
}

#[test]
fn missing_transcript_path_runs_pages() {
    let f = fixture();
    f.trust();
    let out = f.gate_raw("s-nopath", &f.payload_without_transcript("s-nopath"));
    assert_block(&out);
    assert!(
        f.pages_ran(),
        "no transcript_path: web changes cannot be ruled out, pages must run\n{}",
        describe(&out)
    );
}

#[test]
fn nonexistent_transcript_runs_pages() {
    let f = fixture();
    f.trust();
    std::fs::remove_file(&f.transcript).expect("rm transcript");
    let out = f.gate("s-notranscript", false);
    assert_block(&out);
    assert!(
        f.pages_ran(),
        "unreadable transcript: pages must run\n{}",
        describe(&out)
    );
}

#[test]
fn transcript_path_that_is_a_directory_runs_pages() {
    let f = fixture();
    f.trust();
    std::fs::remove_file(&f.transcript).expect("rm transcript");
    std::fs::create_dir_all(&f.transcript).expect("dir in place of transcript");
    let out = f.gate("s-dirtranscript", false);
    assert_block(&out);
    assert!(
        f.pages_ran(),
        "unreadable transcript (a directory): pages must run\n{}",
        describe(&out)
    );
}

#[test]
fn unparseable_transcript_runs_pages() {
    let f = fixture();
    f.trust();
    write(&f.transcript, "this is not json at all\n{{{\n");
    let out = f.gate("s-garbage", false);
    assert_block(&out);
    assert!(
        f.pages_ran(),
        "unparseable transcript: pages must run\n{}",
        describe(&out)
    );
}

#[test]
fn transcript_with_one_corrupt_line_runs_pages() {
    let f = fixture();
    f.trust();
    // A valid non-web Write followed by a line that cannot be parsed: that line
    // could have been a web edit, so the change set is undetermined.
    let lines = [
        tool_use_line(
            "t1",
            "Write",
            serde_json::json!({"file_path": f.abs("README.md"), "content": "x\n"}),
        ),
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Wri"
            .to_string(),
    ];
    write(&f.transcript, &(lines.join("\n") + "\n"));
    let out = f.gate("s-corrupt", false);
    assert_block(&out);
    assert!(
        f.pages_ran(),
        "a corrupt transcript line hides possible web edits: pages must run\n{}",
        describe(&out)
    );
}

// ---------------------------------------------------------------- stdin

#[test]
fn non_json_stdin_blocks() {
    let f = fixture();
    f.trust();
    let out = f.gate_raw("s-badstdin", "this is { not json");
    assert_block(&out);
}

// ---------------------------------------------------------------- bounded give-up

#[test]
fn bounded_give_up_is_loud_and_per_session() {
    let f = fixture();
    // Untrusted: blocks every time. An untrusted config cannot set its own
    // give-up budget, so the built-in default (3) applies.
    let o1 = f.gate("s-giveup", false);
    assert_block(&o1);
    let o2 = f.gate("s-giveup", true);
    assert_block(&o2);
    let o3 = f.gate("s-giveup", true);
    assert_block(&o3);

    let o4 = f.gate("s-giveup", true);
    assert_eq!(o4.status.code(), Some(0), "{}", describe(&o4));
    assert!(
        block_reason(&o4).is_none(),
        "4th consecutive blocked stop should give up (no block)\n{}",
        describe(&o4)
    );
    let msg = json_lines(&o4)
        .into_iter()
        .find_map(|v| {
            v.get("systemMessage")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| panic!("give-up must carry a systemMessage\n{}", describe(&o4)));
    assert!(
        msg.contains("NOTHING WAS VERIFIED"),
        "give-up systemMessage must say NOTHING WAS VERIFIED\n{}",
        describe(&o4)
    );

    let other = f.gate("s-giveup-other", true);
    assert_block(&other);
}
