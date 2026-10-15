//! Claim-reproduction tests for the beacon README / packaging prose.
//!
//! Every test drives the real `beacon` binary with a fresh temp HOME and a
//! fresh temp cwd. `osascript` / `notify-send` / `curl` are replaced by fake
//! executables placed first on PATH that record their argv, so nothing ever
//! pops a real notification or touches the network.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::TempDir;

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Env {
    home: TempDir,
    cwd: TempDir,
    fake: TempDir,
}

fn write_exec(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Env {
    fn new() -> Env {
        let e = Env {
            home: tempfile::tempdir().unwrap(),
            cwd: tempfile::tempdir().unwrap(),
            fake: tempfile::tempdir().unwrap(),
        };
        let rec = |name: &str| {
            // one argv element per line, a marker line closes each invocation.
            let log = e.fake.path().join(format!("{name}.log"));
            write_exec(
                &e.fake.path().join(name),
                &format!(
                    "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done >> '{}'\nprintf -- '--END--\\n' >> '{}'\nexit \"${{FAKE_EXIT:-0}}\"\n",
                    log.display(),
                    log.display()
                ),
            );
        };
        rec("osascript");
        rec("notify-send");
        rec("curl");
        e
    }

    fn path_with_curl(&self) -> String {
        format!("{}:/usr/bin:/bin", self.fake.path().display())
    }

    /// PATH where `curl` cannot be found (curl lives in /usr/bin on macOS).
    fn path_without_curl(&self) -> String {
        let d = self.fake.path().join("nocurl");
        std::fs::create_dir_all(&d).unwrap();
        for n in ["osascript", "notify-send"] {
            let _ = std::fs::remove_file(d.join(n));
            std::os::unix::fs::symlink(self.fake.path().join(n), d.join(n)).unwrap();
        }
        format!("{}:/bin", d.display())
    }

    fn home_config(&self, toml: &str) {
        let d = self.home.path().join(".beacon");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.toml"), toml).unwrap();
    }

    fn project_config(&self, toml: &str) {
        std::fs::write(self.cwd.path().join("beacon.toml"), toml).unwrap();
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.fake.path().join(format!("{name}.log"))).unwrap_or_default()
    }

    /// One recorded invocation per element, each a list of argv lines.
    fn invocations(&self, name: &str) -> Vec<Vec<String>> {
        let mut out = vec![];
        let mut cur = vec![];
        for l in self.log(name).lines() {
            if l == "--END--" {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.push(l.to_string());
            }
        }
        out
    }

    fn run_in(
        &self,
        cwd: &Path,
        args: &[&str],
        stdin: &str,
        path: &str,
        envs: &[(&str, &str)],
    ) -> Out {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_beacon"));
        cmd.args(args)
            .current_dir(cwd)
            .env("HOME", self.home.path())
            .env("PATH", path)
            .env_remove("HARNESS_TRUST_ALL")
            .env_remove("BEACON_DISABLE")
            .env_remove("BEACON_SLACK_WEBHOOK")
            .env_remove("BEACON_WEBHOOK")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("FAKE_EXIT")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn beacon");
        if let Some(mut s) = child.stdin.take() {
            let _ = s.write_all(stdin.as_bytes());
        }
        let o = child.wait_with_output().unwrap();
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }

    fn run(&self, args: &[&str], stdin: &str) -> Out {
        let p = self.path_with_curl();
        self.run_in(self.cwd.path(), args, stdin, &p, &[])
    }

    fn run_env(&self, args: &[&str], stdin: &str, envs: &[(&str, &str)]) -> Out {
        let p = self.path_with_curl();
        self.run_in(self.cwd.path(), args, stdin, &p, envs)
    }

    fn settings(&self) -> PathBuf {
        self.home.path().join(".claude").join("settings.json")
    }
}

fn stop_payload(cwd: &Path, transcript: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "cwd": cwd.to_str().unwrap(),
        "transcript_path": transcript,
    })
    .to_string()
}

fn notif_payload(cwd: &Path, msg: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Notification",
        "cwd": cwd.to_str().unwrap(),
        "message": msg,
    })
    .to_string()
}

/// A home-config `command` channel that dumps the four BEACON_* vars to `out`.
fn dump_cmd_config(out: &Path) -> String {
    format!(
        "desktop = false\ncommand = '''printf 'E=%s\\nP=%s\\nT=%s\\nB=%s\\n' \"$BEACON_EVENT\" \"$BEACON_PROJECT\" \"$BEACON_TITLE\" \"$BEACON_BODY\" > '{}'; true'''\n",
        out.display()
    )
}

fn parse_dump(s: &str) -> (String, String, String, String) {
    let mut e = (String::new(), String::new(), String::new(), String::new());
    for l in s.lines() {
        if let Some(v) = l.strip_prefix("E=") {
            e.0 = v.into();
        } else if let Some(v) = l.strip_prefix("P=") {
            e.1 = v.into();
        } else if let Some(v) = l.strip_prefix("T=") {
            e.2 = v.into();
        } else if let Some(v) = l.strip_prefix("B=") {
            e.3 = v.into();
        }
    }
    e
}

fn write_transcript(dir: &Path, last_text: &str) -> PathBuf {
    let p = dir.join("t.jsonl");
    let lines = [
        serde_json::json!({"type":"user","message":{"content":"hi"}}),
        serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"earlier answer"}]}}),
        serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":last_text}]}}),
    ];
    let s: Vec<String> = lines.iter().map(|v| v.to_string()).collect();
    std::fs::write(&p, s.join("\n") + "\n").unwrap();
    p
}

// ---------------------------------------------------------------- claims

#[test]
fn claim_exit_zero_never_blocks() {
    // every scenario: exit 0 and EMPTY stdout (no {"decision":"block"}).
    let mut results: Vec<(String, i32, String)> = vec![];

    // (1) valid Stop payload with a working command channel
    let e = Env::new();
    let marker = e.cwd.path().join("ran");
    e.home_config(&format!(
        "desktop = false\ncommand = \"touch '{}'\"\n",
        marker.display()
    ));
    let o = e.run(&["notify"], &stop_payload(e.cwd.path(), ""));
    assert!(marker.exists(), "control: command channel must have run");
    results.push(("valid_stop".into(), o.code, o.stdout));

    // (2) malformed stdin
    let o = e.run(&["notify"], "{not json");
    results.push(("malformed".into(), o.code, o.stdout));

    // (3) empty stdin
    let o = e.run(&["notify"], "");
    results.push(("empty".into(), o.code, o.stdout));

    // (4) a channel that fails: command `exit 7`
    let e4 = Env::new();
    e4.home_config("desktop = false\ncommand = \"exit 7\"\n");
    let o = e4.run(&["notify"], &stop_payload(e4.cwd.path(), ""));
    results.push(("command_exit7".into(), o.code, o.stdout));

    // (5) webhook configured but curl absent from PATH
    let e5 = Env::new();
    e5.home_config("desktop = false\nwebhook = \"http://example.invalid/h\"\n");
    let p = e5.path_without_curl();
    let o = e5.run_in(
        e5.cwd.path(),
        &["notify"],
        &stop_payload(e5.cwd.path(), ""),
        &p,
        &[],
    );
    assert!(
        e5.invocations("curl").is_empty(),
        "control: curl must really be absent"
    );
    results.push(("webhook_no_curl".into(), o.code, o.stdout));

    // (6) webhook where curl itself exits non-zero
    let e6 = Env::new();
    e6.home_config("desktop = false\nwebhook = \"http://example.invalid/h\"\n");
    let o = e6.run_env(
        &["notify"],
        &stop_payload(e6.cwd.path(), ""),
        &[("FAKE_EXIT", "22")],
    );
    assert_eq!(e6.invocations("curl").len(), 1, "control: curl was invoked");
    results.push(("webhook_curl_fails".into(), o.code, o.stdout));

    for (name, code, stdout) in &results {
        assert_eq!(*code, 0, "{name}: must exit 0");
        assert_eq!(stdout, "", "{name}: stdout must be empty (no block JSON)");
        assert!(!stdout.contains("block"), "{name}");
    }
}

#[test]
fn claim_two_events_only() {
    let text = std::fs::read_to_string(manifest().join("hooks/hooks.json")).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let hooks = v["hooks"].as_object().unwrap();
    let mut keys: Vec<&str> = hooks.keys().map(|s| s.as_str()).collect();
    keys.sort();
    assert_eq!(keys, vec!["Notification", "Stop"]);
    for k in ["Stop", "Notification"] {
        let groups = hooks[k].as_array().unwrap();
        assert_eq!(groups.len(), 1, "{k}");
        let hs = groups[0]["hooks"].as_array().unwrap();
        assert_eq!(hs.len(), 1, "{k}");
        assert_eq!(hs[0]["type"], "command");
        assert_eq!(hs[0]["command"], "${CLAUDE_PLUGIN_ROOT}/bin/beacon notify");
    }
}

#[test]
fn claim_stop_title_body() {
    let e = Env::new();
    let out = e.cwd.path().join("dump");
    e.home_config(&dump_cmd_config(&out));
    let proj = e.cwd.path().join("myproj");
    std::fs::create_dir_all(&proj).unwrap();

    // short last message: body carries it
    let t = write_transcript(e.cwd.path(), "all done here");
    let o = e.run(&["notify"], &stop_payload(&proj, t.to_str().unwrap()));
    assert_eq!(o.code, 0);
    let (ev, p, title, body) = parse_dump(&std::fs::read_to_string(&out).unwrap());
    assert_eq!(ev, "stop");
    assert_eq!(p, "myproj");
    assert_eq!(title, "✅ myproj — 完了");
    assert_eq!(body, "all done here");

    // missing transcript: fallback body
    std::fs::remove_file(&out).unwrap();
    let o = e.run(&["notify"], &stop_payload(&proj, "/nonexistent/t.jsonl"));
    assert_eq!(o.code, 0);
    let (_, _, title, body) = parse_dump(&std::fs::read_to_string(&out).unwrap());
    assert_eq!(title, "✅ myproj — 完了");
    assert_eq!(body, "ターンが完了しました。");

    // LONG last message (> snippet_chars). The README once said "tail of
    // Claude's last message". OBSERVED: the snippet is the HEAD (first snippet_chars chars)
    // followed by "…", not the tail.
    std::fs::remove_file(&out).unwrap();
    let long: String = (0..400)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    let t = write_transcript(e.cwd.path(), &long);
    e.run(&["notify"], &stop_payload(&proj, t.to_str().unwrap()));
    let (_, _, _, body) = parse_dump(&std::fs::read_to_string(&out).unwrap());
    let head: String = long.chars().take(160).collect();
    assert_eq!(body, format!("{head}…"), "actual: head + ellipsis");
    assert!(
        !body.ends_with(&long[long.len() - 10..]),
        "actual: body is NOT the tail of the message"
    );
}

#[test]
fn claim_notification_title_body() {
    let e = Env::new();
    let out = e.cwd.path().join("dump");
    e.home_config(&dump_cmd_config(&out));
    let proj = e.cwd.path().join("projN");
    std::fs::create_dir_all(&proj).unwrap();
    let o = e.run(
        &["notify"],
        &notif_payload(&proj, "Claude needs your permission to use Bash"),
    );
    assert_eq!(o.code, 0);
    let (ev, p, title, body) = parse_dump(&std::fs::read_to_string(&out).unwrap());
    assert_eq!(ev, "attention");
    assert_eq!(p, "projN");
    assert_eq!(title, "🔔 projN — 確認");
    assert_eq!(body, "Claude needs your permission to use Bash");
}

#[test]
fn claim_any_combination() {
    let e = Env::new();
    e.home_config(
        "desktop = true\nslack_webhook = \"http://s.invalid\"\nwebhook = \"http://w.invalid\"\ncommand = \"true\"\n",
    );
    let s = e.run(&["status"], "").stdout;
    assert!(s.contains("desktop:          true"), "{s}");
    assert!(s.contains("slack_webhook:    set"), "{s}");
    assert!(s.contains("webhook:          set"), "{s}");
    assert!(s.contains("command:          true"), "{s}");

    // all four really deliver on a Stop
    let o = e.run(&["notify"], &stop_payload(e.cwd.path(), ""));
    assert_eq!(o.code, 0);
    assert_eq!(e.invocations("osascript").len(), 1, "desktop fired");
    assert_eq!(e.invocations("curl").len(), 2, "slack + webhook fired");
    let log = std::fs::read_to_string(e.home.path().join(".beacon/state/log.jsonl")).unwrap();
    let v: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!(
        v["channels"],
        serde_json::json!(["desktop", "slack", "webhook", "command"])
    );

    // only one channel
    let e1 = Env::new();
    e1.home_config("desktop = false\nwebhook = \"http://w.invalid\"\n");
    let s = e1.run(&["status"], "").stdout;
    assert!(s.contains("desktop:          false"), "{s}");
    assert!(s.contains("slack_webhook:    -"), "{s}");
    assert!(s.contains("webhook:          set"), "{s}");
    assert!(s.contains("command:          -"), "{s}");
    e1.run(&["notify"], &stop_payload(e1.cwd.path(), ""));
    assert_eq!(e1.invocations("osascript").len(), 0);
    assert_eq!(e1.invocations("curl").len(), 1);
}

#[test]
fn claim_desktop_channel() {
    let e = Env::new();
    e.home_config("desktop = true\nsound = \"Glass\"\n");
    let proj = e.cwd.path().join("dproj");
    std::fs::create_dir_all(&proj).unwrap();
    e.run(&["notify"], &notif_payload(&proj, "needs you"));

    match std::env::consts::OS {
        "macos" => {
            let inv = e.invocations("osascript");
            assert_eq!(inv.len(), 1, "{inv:?}");
            assert_eq!(inv[0][0], "-e");
            assert_eq!(inv[0].len(), 2);
            assert_eq!(
                inv[0][1],
                "display notification \"needs you\" with title \"🔔 dproj — 確認\" sound name \"Glass\""
            );

            // without sound: no sound clause
            let e2 = Env::new();
            e2.home_config("desktop = true\n");
            e2.run(&["notify"], &notif_payload(&proj, "needs you"));
            let inv = e2.invocations("osascript");
            assert_eq!(inv.len(), 1);
            assert_eq!(
                inv[0][1],
                "display notification \"needs you\" with title \"🔔 dproj — 確認\""
            );
            assert!(!inv[0][1].contains("sound"));
            assert_eq!(e2.invocations("notify-send").len(), 0);
        }
        "linux" => {
            let inv = e.invocations("notify-send");
            assert_eq!(
                inv,
                vec![vec!["🔔 dproj — 確認".to_string(), "needs you".to_string()]]
            );
            assert_eq!(e.invocations("osascript").len(), 0);
        }
        other => panic!("unsupported host OS for this claim test: {other}"),
    }
}

fn parse_curl(inv: &[String]) -> (String, String, Value) {
    // returns (url, content-type header, body json)
    let url = inv.last().unwrap().clone();
    let i = inv.iter().position(|a| a == "--data-binary").unwrap();
    let body: Value = serde_json::from_str(&inv[i + 1]).unwrap();
    let h = inv.iter().position(|a| a == "-H").unwrap();
    (url, inv[h + 1].clone(), body)
}

#[test]
fn claim_slack_payload_env_override() {
    let e = Env::new();
    e.home_config("desktop = false\nslack_webhook = \"http://URL_F.invalid\"\n");
    let proj = e.cwd.path().join("sproj");
    std::fs::create_dir_all(&proj).unwrap();

    // with env: curl is called with URL_E, not URL_F
    let o = e.run_env(
        &["notify"],
        &notif_payload(&proj, "hey"),
        &[("BEACON_SLACK_WEBHOOK", "http://URL_E.invalid")],
    );
    assert_eq!(o.code, 0);
    let inv = e.invocations("curl");
    assert_eq!(inv.len(), 1, "{inv:?}");
    let (url, _ct, body) = parse_curl(&inv[0]);
    assert_eq!(url, "http://URL_E.invalid");
    assert!(!inv[0].iter().any(|a| a.contains("URL_F")));
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["text"]);
    assert_eq!(body["text"], "🔔 sproj — 確認\nhey");

    // without env: file URL is used
    let e2 = Env::new();
    e2.home_config("desktop = false\nslack_webhook = \"http://URL_F.invalid\"\n");
    e2.run(&["notify"], &notif_payload(&proj, "hey"));
    let inv = e2.invocations("curl");
    assert_eq!(parse_curl(&inv[0]).0, "http://URL_F.invalid");
}

#[test]
fn claim_webhook_payload_env_override() {
    let e = Env::new();
    e.home_config("desktop = false\nwebhook = \"http://URL_F.invalid\"\n");
    let proj = e.cwd.path().join("wproj");
    std::fs::create_dir_all(&proj).unwrap();
    let o = e.run_env(
        &["notify"],
        &notif_payload(&proj, "hey"),
        &[("BEACON_WEBHOOK", "http://URL_E.invalid")],
    );
    assert_eq!(o.code, 0);
    let inv = e.invocations("curl");
    assert_eq!(inv.len(), 1);
    let (url, ct, body) = parse_curl(&inv[0]);
    assert_eq!(url, "http://URL_E.invalid");
    assert!(
        inv[0].windows(2).any(|w| w == ["-X", "POST"]),
        "{:?}",
        inv[0]
    );
    assert_eq!(ct, "Content-Type: application/json");
    let mut keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(|s| s.as_str())
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["body", "event", "project", "title"]);
    assert_eq!(body["event"], "attention");
    assert_eq!(body["project"], "wproj");
    assert_eq!(body["title"], "🔔 wproj — 確認");
    assert_eq!(body["body"], "hey");

    // no env: file URL
    let e2 = Env::new();
    e2.home_config("desktop = false\nwebhook = \"http://URL_F.invalid\"\n");
    e2.run(&["notify"], &notif_payload(&proj, "hey"));
    assert_eq!(
        parse_curl(&e2.invocations("curl")[0]).0,
        "http://URL_F.invalid"
    );
}

#[test]
fn claim_command_env_vars() {
    let e = Env::new();
    let out = e.cwd.path().join("dump");
    // pipe + `;` shell syntax in the command
    e.home_config(&format!(
        "desktop = false\ncommand = '''echo x | cat >/dev/null; printf 'E=%s\\nP=%s\\nT=%s\\nB=%s\\n' \"$BEACON_EVENT\" \"$BEACON_PROJECT\" \"$BEACON_TITLE\" \"$BEACON_BODY\" > '{}' '''\n",
        out.display()
    ));
    let proj = e.cwd.path().join("cproj");
    std::fs::create_dir_all(&proj).unwrap();

    e.run(&["notify"], &notif_payload(&proj, "msg1"));
    let (ev, p, t, b) = parse_dump(&std::fs::read_to_string(&out).unwrap());
    assert_eq!(
        (ev.as_str(), p.as_str(), t.as_str(), b.as_str()),
        ("attention", "cproj", "🔔 cproj — 確認", "msg1")
    );

    std::fs::remove_file(&out).unwrap();
    e.run(&["notify"], &stop_payload(&proj, ""));
    let (ev, p, t, b) = parse_dump(&std::fs::read_to_string(&out).unwrap());
    assert_eq!(
        (ev.as_str(), p.as_str(), t.as_str(), b.as_str()),
        ("stop", "cproj", "✅ cproj — 完了", "ターンが完了しました。")
    );
}

#[test]
fn claim_curl_max_time_no_http_stack() {
    let e = Env::new();
    e.home_config(
        "desktop = false\nwebhook = \"http://w.invalid\"\nslack_webhook = \"http://s.invalid\"\n",
    );
    e.run(&["notify"], &stop_payload(e.cwd.path(), ""));
    let inv = e.invocations("curl");
    assert_eq!(inv.len(), 2);
    for i in &inv {
        assert!(i.windows(2).any(|w| w == ["--max-time", "8"]), "{i:?}");
    }

    let text = std::fs::read_to_string(manifest().join("Cargo.toml")).unwrap();
    let v: toml::Value = toml::from_str(&text).unwrap();
    let deps: Vec<String> = v["dependencies"]
        .as_table()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert!(deps.contains(&"clap".to_string()), "control: {deps:?}");
    for bad in [
        "reqwest",
        "ureq",
        "hyper",
        "isahc",
        "attohttpc",
        "curl",
        "surf",
        "minreq",
        "http",
        "native-tls",
        "rustls",
    ] {
        assert!(!deps.iter().any(|d| d == bad), "{bad} in deps: {deps:?}");
    }
}

#[test]
fn claim_config_locations_default_desktop() {
    // no config anywhere: defaults, desktop on
    let e = Env::new();
    let s = e.run(&["status"], "").stdout;
    assert!(s.contains("(defaults — no config file)"), "{s}");
    assert!(s.contains("desktop:          true"), "{s}");

    // home config honored
    let e = Env::new();
    e.home_config("desktop = false\nwebhook = \"http://w.invalid\"\n");
    let s = e.run(&["status"], "").stdout;
    assert!(s.contains("desktop:          false"), "{s}");
    assert!(s.contains("webhook:          set"), "{s}");
    assert!(s.contains(".beacon/config.toml"), "{s}");

    // project beacon.toml honored and wins over home (no merge)
    e.project_config("desktop = true\nslack_webhook = \"http://s.invalid\"\n");
    let s = e.run(&["status"], "").stdout;
    assert!(s.contains("desktop:          true"), "{s}");
    assert!(s.contains("slack_webhook:    set"), "{s}");
    assert!(s.contains("webhook:          -"), "layers not merged: {s}");
    assert!(s.contains("beacon.toml"), "{s}");
    assert!(!s.contains(".beacon/config.toml"), "{s}");
}

#[test]
fn claim_init_force() {
    let e = Env::new();
    let f = e.cwd.path().join("beacon.toml");
    let o = e.run(&["init"], "");
    assert_eq!(o.code, 0);
    let first = std::fs::read_to_string(&f).unwrap();
    assert!(first.contains("desktop = true"));

    std::fs::write(&f, "# mine\n").unwrap();
    let o = e.run(&["init"], "");
    assert_eq!(o.code, 1, "second init without --force exits 1");
    assert!(o.stderr.contains("already exists"), "{}", o.stderr);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "# mine\n");

    let o = e.run(&["init", "--force"], "");
    assert_eq!(o.code, 0);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), first);
}

#[test]
fn claim_test_fires_sample() {
    let e = Env::new();
    let out = e.cwd.path().join("runs");
    e.home_config(&format!(
        "desktop = false\ncommand = \"echo \\\"$BEACON_EVENT\\\" >> '{}'\"\n",
        out.display()
    ));
    let o = e.run(&["test"], "");
    assert_eq!(o.code, 0);
    assert_eq!(o.stdout.trim(), "sent via: command");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "test\n");
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn beacon_groups(v: &Value, event: &str) -> Vec<Value> {
    v["hooks"][event]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|g| g.to_string().contains("beacon"))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn seed_settings(e: &Env) -> String {
    let dir = e.home.path().join(".claude");
    std::fs::create_dir_all(&dir).unwrap();
    let orig = serde_json::json!({
        "theme": "dark",
        "hooks": {"Stop": [{"hooks":[{"type":"command","command":"/usr/bin/foreign-hook run"}]}]}
    });
    let text = serde_json::to_string_pretty(&orig).unwrap();
    std::fs::write(e.settings(), &text).unwrap();
    text
}

#[test]
fn claim_install_merges_hooks() {
    let e = Env::new();
    seed_settings(&e);
    let o = e.run(&["install"], "");
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v = read_json(&e.settings());
    assert_eq!(v["theme"], "dark", "other keys kept");
    for ev in ["Stop", "Notification"] {
        let g = beacon_groups(&v, ev);
        assert_eq!(g.len(), 1, "{ev}: {v}");
        let cmd = g[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.ends_with(" notify"), "{cmd}");
        assert!(cmd.contains("beacon"), "{cmd}");
    }
}

#[test]
fn claim_status_active_channels() {
    let e = Env::new();
    e.home_config("desktop = true\nsound = \"Ping\"\nslack_webhook = \"http://s.invalid\"\ncommand = \"echo hi\"\n");
    let o = e.run(&["status"], "");
    assert_eq!(o.code, 0);
    for line in [
        "desktop:          true (os: ",
        "sound:            Ping",
        "slack_webhook:    set",
        "webhook:          -",
        "command:          echo hi",
    ] {
        assert!(
            o.stdout.contains(line),
            "missing {line:?} in:\n{}",
            o.stdout
        );
    }
    assert!(o.stdout.contains(".beacon/config.toml"));
}

#[test]
fn claim_trust_cli() {
    let e = Env::new();
    let out = e.cwd.path().join("ran");
    let proj = tempfile::tempdir().unwrap();
    std::fs::write(
        proj.path().join("beacon.toml"),
        format!("desktop = false\ncommand = \"touch '{}'\"\n", out.display()),
    )
    .unwrap();
    let p = e.path_with_curl();
    let payload = stop_payload(proj.path(), "");

    // before trust: command not run
    let o = e.run_in(proj.path(), &["notify"], &payload, &p, &[]);
    assert_eq!(o.code, 0);
    assert!(!out.exists(), "untrusted project command must not run");

    // trust, with cwd = project
    let o = e.run_in(proj.path(), &["trust"], "", &p, &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("trusted "), "{}", o.stdout);
    assert!(
        e.home.path().join(".harness/trust.toml").exists(),
        "trust store lives under HOME"
    );

    // after trust: command runs
    let o = e.run_in(proj.path(), &["notify"], &payload, &p, &[]);
    assert_eq!(o.code, 0);
    assert!(out.exists(), "trusted project command must run");
}

#[test]
fn claim_uninstall_removes() {
    let e = Env::new();
    seed_settings(&e);
    e.run(&["install"], "");
    let v = read_json(&e.settings());
    assert_eq!(beacon_groups(&v, "Stop").len(), 1, "control: installed");
    let o = e.run(&["uninstall"], "");
    assert_eq!(o.code, 0);
    let v = read_json(&e.settings());
    for ev in ["Stop", "Notification"] {
        assert!(beacon_groups(&v, ev).is_empty(), "{ev}: {v}");
    }
    assert!(!v.to_string().contains("beacon"), "{v}");
}

fn backups(e: &Env) -> Vec<PathBuf> {
    let mut b: Vec<PathBuf> = std::fs::read_dir(e.home.path().join(".claude"))
        .unwrap()
        .map(|d| d.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("settings.json.bak-")
        })
        .collect();
    b.sort();
    b
}

#[test]
fn claim_install_dryrun_idempotent_backup_foreign() {
    let e = Env::new();
    let orig = seed_settings(&e);
    let foreign = "/usr/bin/foreign-hook run";

    // (a) --dry-run leaves bytes unchanged for both
    let o = e.run(&["install", "--dry-run"], "");
    assert_eq!(o.code, 0);
    assert_eq!(
        std::fs::read_to_string(e.settings()).unwrap(),
        orig,
        "(a) install"
    );
    assert!(backups(&e).is_empty(), "(a) dry-run writes no backup");
    let o = e.run(&["uninstall", "--dry-run"], "");
    assert_eq!(o.code, 0);
    assert_eq!(
        std::fs::read_to_string(e.settings()).unwrap(),
        orig,
        "(a) uninstall"
    );

    // (c) backup of the previous settings.json exists after a write
    e.run(&["install"], "");
    let b = backups(&e);
    assert_eq!(b.len(), 1, "(c) {b:?}");
    assert_eq!(
        std::fs::read_to_string(&b[0]).unwrap(),
        orig,
        "(c) backup is previous content"
    );
    let name = b[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("settings.json.bak-"), "(c) name {name}");

    // (d) foreign Stop group survives install
    let v = read_json(&e.settings());
    let stop = v["hooks"]["Stop"].to_string();
    assert!(stop.contains(foreign), "(d) after install: {stop}");

    // (b) idempotent: install twice -> exactly one beacon group per event
    let after_first = std::fs::read_to_string(e.settings()).unwrap();
    e.run(&["install"], "");
    let v = read_json(&e.settings());
    for ev in ["Stop", "Notification"] {
        assert_eq!(beacon_groups(&v, ev).len(), 1, "(b) {ev}: {v}");
    }
    assert_eq!(
        v["hooks"]["Stop"].as_array().unwrap().len(),
        2,
        "(b)+(d) foreign + one beacon"
    );
    assert_eq!(
        std::fs::read_to_string(e.settings()).unwrap(),
        after_first,
        "(b) byte-idempotent"
    );

    // uninstall: backup of the installed state, foreign survives, idempotent
    let before_un = std::fs::read_to_string(e.settings()).unwrap();
    e.run(&["uninstall"], "");
    let v = read_json(&e.settings());
    assert!(
        v["hooks"]["Stop"].to_string().contains(foreign),
        "(d) after uninstall: {v}"
    );
    assert!(!v.to_string().contains("beacon"));
    assert!(
        backups(&e)
            .iter()
            .any(|p| std::fs::read_to_string(p).unwrap() == before_un),
        "(c) uninstall backs up previous settings"
    );
    let after_un = std::fs::read_to_string(e.settings()).unwrap();
    let o = e.run(&["uninstall"], "");
    assert_eq!(o.code, 0, "(b) second uninstall succeeds");
    assert_eq!(
        std::fs::read_to_string(e.settings()).unwrap(),
        after_un,
        "(b) uninstall idempotent"
    );
}

#[test]
fn claim_example_toml_knobs() {
    let text = std::fs::read_to_string(manifest().join("beacon.example.toml")).unwrap();
    for k in [
        "on_stop",
        "on_notification",
        "include_snippet",
        "snippet_chars",
        "desktop",
        "sound",
        "slack_webhook",
        "webhook",
        "command",
        "log",
        "state_dir",
    ] {
        assert!(
            text.lines().any(|l| {
                let l = l.trim_start_matches('#').trim_start();
                l.starts_with(k) && l[k.len()..].trim_start().starts_with('=')
            }),
            "key {k} not documented"
        );
    }

    // it parses into the Config: mutate two uncommented values and observe them
    let e = Env::new();
    let modded = text
        .replace("snippet_chars = 160", "snippet_chars = 77")
        .replace("desktop = true", "desktop = false");
    assert_ne!(modded, text);
    e.home_config(&modded);
    let s = e.run(&["status"], "").stdout;
    assert!(s.contains("include_snippet:  true (77 chars)"), "{s}");
    assert!(s.contains("desktop:          false"), "{s}");

    // on_stop=false mutes Stop but not Notification
    let e = Env::new();
    let ran = e.cwd.path().join("ran");
    let cfg = text
        .replace("on_stop = true", "on_stop = false")
        .replace("desktop = true", "desktop = false")
        + &format!("\ncommand = \"echo x >> '{}'\"\n", ran.display());
    e.home_config(&cfg);
    e.run(&["notify"], &stop_payload(e.cwd.path(), ""));
    assert!(!ran.exists(), "Stop must be muted by on_stop=false");
    e.run(&["notify"], &notif_payload(e.cwd.path(), "m"));
    assert!(ran.exists(), "Notification still delivered");
}

#[test]
fn claim_disable_env() {
    let e = Env::new();
    let ran = e.cwd.path().join("ran");
    e.home_config(&format!(
        "desktop = true\nwebhook = \"http://w.invalid\"\ncommand = \"touch '{}'\"\n",
        ran.display()
    ));
    let o = e.run_env(
        &["notify"],
        &stop_payload(e.cwd.path(), ""),
        &[("BEACON_DISABLE", "1")],
    );
    assert_eq!(o.code, 0);
    assert!(!ran.exists(), "command must not run");
    assert!(e.invocations("osascript").is_empty() && e.invocations("curl").is_empty());
    assert!(!e.home.path().join(".beacon/state/log.jsonl").exists());

    // control: same config without the env var delivers
    let o = e.run(&["notify"], &stop_payload(e.cwd.path(), ""));
    assert_eq!(o.code, 0);
    assert!(ran.exists());

    // `beacon test` is NOT silenced by BEACON_DISABLE=1: it still runs the command.
    let e2 = Env::new();
    let out = e2.cwd.path().join("dump");
    e2.home_config(&dump_cmd_config(&out));
    let o = e2.run_env(&["test"], "", &[("BEACON_DISABLE", "1")]);
    assert_eq!(o.code, 0);
    assert_eq!(o.stdout.trim(), "sent via: command", "{}", o.stdout);
    let (ev, _, _, _) = parse_dump(&std::fs::read_to_string(&out).unwrap());
    assert_eq!(ev, "test");
}

#[test]
fn claim_make_targets() {
    let dir = manifest();
    let mk = |t: &str| {
        let o = Command::new("make")
            .args(["-n", t])
            .current_dir(&dir)
            .output()
            .expect("make available");
        assert!(
            o.status.success(),
            "make -n {t}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    let mac = mk("mac");
    let linux = mk("linux");
    let bins = mk("bins");
    assert!(mac.contains("cargo build --release"), "{mac}");
    assert!(mac.contains("bin/beacon-darwin-"), "{mac}");
    assert!(
        linux.contains("cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17"),
        "{linux}"
    );
    assert!(linux.contains("bin/beacon-linux-x86_64"), "{linux}");
    assert!(
        bins.contains("cargo build --release") && bins.contains("cargo zigbuild"),
        "{bins}"
    );

    // Where `cp` reads from must be inside where cargo builds.
    let cp_src = |out: &str| -> String {
        out.lines()
            .find(|l| l.starts_with("cp "))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_string()
    };
    let mac_src = cp_src(&mac);
    let linux_src = cp_src(&linux);

    let o = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version=1", "--offline"])
        .current_dir(&dir)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("cargo metadata");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let meta: Value = serde_json::from_slice(&o.stdout).unwrap();
    let target_dir = PathBuf::from(meta["target_directory"].as_str().unwrap());
    let ws_root = PathBuf::from(meta["workspace_root"].as_str().unwrap());
    assert_eq!(
        target_dir,
        ws_root.join("target"),
        "cargo builds into the workspace-root target/"
    );
    let norm = |p: PathBuf| -> PathBuf {
        let mut out = PathBuf::new();
        for c in p.components() {
            match c {
                std::path::Component::ParentDir => {
                    out.pop();
                }
                std::path::Component::CurDir => {}
                c => out.push(c),
            }
        }
        out
    };
    let mac_abs = norm(dir.join(&mac_src));
    let linux_abs = norm(dir.join(&linux_src));
    let td = norm(target_dir.clone());
    assert_eq!(
        mac_abs,
        td.join("release/beacon"),
        "mac cp source {mac_src}"
    );
    assert_eq!(
        linux_abs,
        td.join("x86_64-unknown-linux-gnu/release/beacon"),
        "linux cp source {linux_src}"
    );
    assert!(mac_abs.starts_with(&td) && linux_abs.starts_with(&td));
}

#[test]
fn claim_platform_bins_untracked() {
    let dir = manifest();
    let tracked = Command::new("git")
        .args(["ls-files", "--", "bin"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(tracked.status.success());
    let tracked: Vec<String> = String::from_utf8_lossy(&tracked.stdout)
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(tracked, vec!["bin/beacon".to_string()]);

    for f in ["bin/beacon-darwin-arm64", "bin/beacon-linux-x86_64"] {
        let o = Command::new("git")
            .args(["check-ignore", "-q", "--no-index", f])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert_eq!(o.code(), Some(0), "{f} must be git-ignored");
    }
    // negative control: a tracked file is not ignored
    let o = Command::new("git")
        .args(["check-ignore", "-q", "--no-index", "bin/beacon"])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert_eq!(o.code(), Some(1), "launcher must NOT be ignored");

    // The Makefile header no longer tells the reader to commit the binaries.
    let mk = std::fs::read_to_string(dir.join("Makefile")).unwrap();
    assert!(!mk.contains("commit them"), "stale 'commit them' comment");
    assert!(
        mk.contains("not\n# committed") || mk.contains("not committed"),
        "Makefile must say the binaries are not committed"
    );

    // rollout: build from source (cargo) + copy into the plugin cache
    let root = dir.parent().unwrap().parent().unwrap();
    let roll = std::fs::read_to_string(root.join("scripts/rollout-plugins.sh")).unwrap();
    let rebuild = std::fs::read_to_string(root.join("scripts/rebuild-plugins.sh")).unwrap();
    assert!(
        roll.contains("rebuild-plugins.sh"),
        "rollout drives rebuild-plugins.sh"
    );
    assert!(
        rebuild.contains("cargo build"),
        "rebuild builds from source with cargo"
    );
}
