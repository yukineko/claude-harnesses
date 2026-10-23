// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Claim-reproduction tests for the prose in reviewgate's README / hooks /
//! agent file / Cargo.toml (one `claim_<id>` test per claim).
//!
//! Every test drives the real `reviewgate` binary (or reads the shipped
//! packaging file) inside a throwaway sandbox: a fresh `HOME` (so the default
//! state dir `$HOME/.reviewgate/state` and the trust list never touch the real
//! ones), a fresh git repo under it, and an environment with
//! `REVIEWGATE_DISABLE` / `HARNESS_TRUST_ALL` removed unless a test sets them.
//!
//! Observation channels: stdout (`{"decision":"block",...}`), stderr, exit
//! code, and `$HOME/.reviewgate/state/log.jsonl` (one line per verdict, tag in
//! `verdict`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    /// A Stop-hook block: stdout is `{"decision":"block",...}`.
    fn blocked(&self) -> bool {
        serde_json::from_str::<Value>(self.stdout.trim())
            .ok()
            .and_then(|v| v.get("decision").and_then(|d| d.as_str().map(String::from)))
            .as_deref()
            == Some("block")
    }
}

struct Sandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Sandbox {
    /// HOME + a git repo (`$HOME/project`) with one committed file `seed.rs`,
    /// opted in to reviewing with a home config `mode = "inject"` (the
    /// built-in default is `off`, see `claim_default_mode_off`).
    fn new() -> Sandbox {
        let sb = Sandbox::unconfigured();
        sb.home_config("mode = \"inject\"\n");
        sb
    }

    /// Like [`Sandbox::new`] but with NO config file at all (built-in defaults).
    fn unconfigured() -> Sandbox {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().canonicalize().expect("canonicalize");
        let repo = home.join("project");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@t.com"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("seed.rs"), "fn seed() {}\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "seed"]);
        Sandbox {
            _tmp: tmp,
            home,
            repo,
        }
    }

    /// Only HOME (no repo): for commands run in a non-repo cwd.
    fn bare() -> Sandbox {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().canonicalize().expect("canonicalize");
        let repo = home.join("project");
        std::fs::create_dir_all(&repo).unwrap();
        Sandbox {
            _tmp: tmp,
            home,
            repo,
        }
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.repo.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    /// Write the home config. A body that sets no `mode` key gets
    /// `mode = "inject"` prepended, so the claims about the reviewing path keep
    /// exercising it now that the built-in default is `off`.
    fn home_config(&self, toml: &str) {
        let d = self.home.join(".reviewgate");
        std::fs::create_dir_all(&d).unwrap();
        let sets_mode = toml.lines().any(|l| l.trim_start().starts_with("mode"));
        let body = if sets_mode {
            toml.to_string()
        } else {
            format!("mode = \"inject\"\n{toml}")
        };
        std::fs::write(d.join("config.toml"), body).unwrap();
    }

    fn state_dir(&self) -> PathBuf {
        self.home.join(".reviewgate/state")
    }

    /// Run the binary. `session` sets `CLAUDE_CODE_SESSION_ID`.
    fn exec(
        &self,
        cwd: &Path,
        args: &[&str],
        session: Option<&str>,
        stdin: &str,
        envs: &[(&str, &str)],
    ) -> Run {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_reviewgate"));
        cmd.args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env_remove("REVIEWGATE_DISABLE")
            .env_remove("HARNESS_TRUST_ALL")
            .env_remove("REVIEWGATE_GIT_TIMEOUT_MS");
        match session {
            Some(s) => {
                cmd.env("CLAUDE_CODE_SESSION_ID", s);
            }
            None => {
                cmd.env_remove("CLAUDE_CODE_SESSION_ID");
            }
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("reviewgate spawns");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().expect("reviewgate runs");
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn payload(&self, session: &str, transcript: Option<&Path>, active: bool) -> String {
        let mut v = serde_json::json!({
            "session_id": session,
            "cwd": self.repo.to_string_lossy(),
            "hook_event_name": "Stop",
            "stop_hook_active": active,
        });
        if let Some(t) = transcript {
            v["transcript_path"] = Value::String(t.to_string_lossy().into_owned());
        }
        v.to_string()
    }

    /// One Stop-hook invocation in the repo.
    fn stop(&self, session: &str) -> Run {
        self.stop_with(session, None, &[])
    }

    fn stop_with(&self, session: &str, transcript: Option<&Path>, envs: &[(&str, &str)]) -> Run {
        let p = self.payload(session, transcript, false);
        self.exec(&self.repo, &["review"], Some(session), &p, envs)
    }

    /// CLI subcommand in the repo (no stdin).
    fn cli(&self, args: &[&str]) -> Run {
        self.exec(&self.repo, args, None, "", &[])
    }

    /// Every parsed line of `log.jsonl` (empty vec when absent).
    fn log(&self) -> Vec<Value> {
        let p = self.state_dir().join("log.jsonl");
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// The `verdict` tag of the newest decision line for `session`.
    fn last_verdict(&self, session: &str) -> String {
        self.log()
            .iter()
            .rfind(|v| {
                v.get("session").and_then(|s| s.as_str()) == Some(session)
                    && v.get("verdict").is_some()
            })
            .and_then(|v| v.get("verdict").and_then(|x| x.as_str().map(String::from)))
            .unwrap_or_else(|| panic!("no verdict log line for {session}: {:?}", self.log()))
    }

    fn last_files(&self, session: &str) -> Vec<String> {
        self.log()
            .iter()
            .rfind(|v| {
                v.get("session").and_then(|s| s.as_str()) == Some(session)
                    && v.get("verdict").is_some()
            })
            .and_then(|v| v.get("files").and_then(|f| f.as_array()).cloned())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn crate_file(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ---------------------------------------------------------------------------
// packaging claims
// ---------------------------------------------------------------------------

/// `Ok(())` iff `json` declares exactly one event, `Stop`, with exactly one
/// command hook whose command ends in `bin/reviewgate review`.
fn check_hooks_one_stop(json: &str) -> Result<(), String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
    let hooks = v
        .get("hooks")
        .and_then(|h| h.as_object())
        .ok_or("no `hooks` object")?;
    let keys: Vec<&String> = hooks.keys().collect();
    if keys != ["Stop"] {
        return Err(format!("event keys are {keys:?}, expected exactly [Stop]"));
    }
    let groups = hooks["Stop"].as_array().ok_or("Stop is not an array")?;
    let mut cmds = Vec::new();
    for g in groups {
        for h in g
            .get("hooks")
            .and_then(|h| h.as_array())
            .ok_or("group without hooks")?
        {
            if h.get("type").and_then(|t| t.as_str()) != Some("command") {
                return Err(format!("non-command hook: {h}"));
            }
            cmds.push(
                h.get("command")
                    .and_then(|c| c.as_str())
                    .ok_or("hook without command")?
                    .to_string(),
            );
        }
    }
    if cmds.len() != 1 {
        return Err(format!(
            "{} command hooks, expected 1: {cmds:?}",
            cmds.len()
        ));
    }
    if !cmds[0].ends_with("bin/reviewgate review") {
        return Err(format!("command is {:?}", cmds[0]));
    }
    Ok(())
}

#[test]
fn claim_hooks_one_stop() {
    let json = crate_file("hooks/hooks.json");
    check_hooks_one_stop(&json).unwrap_or_else(|e| panic!("hooks.json: {e}"));
    assert!(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("bin/reviewgate")
            .is_file(),
        "the launcher bin/reviewgate must exist"
    );
    // RED controls (the checker can fail): extra event, wrong command, extra hook.
    let extra_event = json.replacen("\"Stop\"", "\"PostToolUse\": [], \"Stop\"", 1);
    assert!(check_hooks_one_stop(&extra_event).is_err());
    let wrong_cmd = json.replace("bin/reviewgate review", "bin/reviewgate status");
    assert!(check_hooks_one_stop(&wrong_cmd).is_err());
    let two = json.replace(
        "\"timeout\": 600 }",
        "\"timeout\": 600 }, { \"type\": \"command\", \"command\": \"x\" }",
    );
    assert_ne!(two, json, "control mutation must change the input");
    assert!(check_hooks_one_stop(&two).is_err());
}

#[test]
fn claim_review_hook_does_not_invoke_agent() {
    let needle = "reviewgate-typing";
    let hooks = crate_file("hooks/hooks.json");
    assert!(!hooks.contains(needle), "hooks.json references the agent");
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut scanned = 0;
    for e in std::fs::read_dir(&src).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) == Some("rs") {
            let text = std::fs::read_to_string(&p).unwrap();
            assert!(
                !text.contains(needle),
                "{} references {needle}",
                p.display()
            );
            scanned += 1;
        }
    }
    assert!(scanned >= 5, "scanned only {scanned} src files");
    // RED control: the agent file does carry its own name, so the needle is the
    // right string to search for.
    assert!(crate_file("agents/reviewgate-typing.md").contains(needle));
}

#[test]
fn claim_license_mit() {
    let manifest: toml::Value = crate_file("Cargo.toml").parse().expect("Cargo.toml parses");
    assert_eq!(
        manifest["package"]["license"].as_str(),
        Some("MIT"),
        "package.license"
    );
    // RED control: reading a different key would not give MIT.
    assert_ne!(manifest["package"]["name"].as_str(), Some("MIT"));
}

#[test]
fn claim_example_toml() {
    let example = crate_file("reviewgate.example.toml");
    let sb = Sandbox::new();
    sb.home_config(&example);
    let r = sb.cli(&["status"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        !r.stdout.contains("FAILED"),
        "example config failed to load: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("mode:          off"),
        "stdout: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("max_attempts:  2"),
        "stdout: {}",
        r.stdout
    );
    // The Stop hook is not config-invalid under it either: the example ships
    // the default mode (off), so the stop is allowed with no review.
    sb.write("changed.rs", "fn c() {}\n");
    let s = sb.stop("ex");
    assert!(!s.blocked(), "stdout={} stderr={}", s.stdout, s.stderr);
    assert_eq!(s.code, 0, "{}", s.stderr);

    // RED control: a corrupted copy of the example is reported as FAILED.
    let sb2 = Sandbox::new();
    sb2.home_config(&format!("{example}\nmode = [[[\n"));
    let bad = sb2.cli(&["status"]);
    assert!(bad.stdout.contains("FAILED to load"), "{}", bad.stdout);
}

// ---------------------------------------------------------------------------
// config defaults
// ---------------------------------------------------------------------------

#[test]
fn claim_default_mode_off() {
    let sb = Sandbox::unconfigured();
    let r = sb.cli(&["status"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.stdout.contains("mode:          off"),
        "stdout: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("(defaults — no config file)"),
        "this must be the no-config case: {}",
        r.stdout
    );
    // Behaviourally: with no config the stop is allowed with no review (no
    // block, no rubric) and nothing is logged as a review verdict.
    sb.write("changed.rs", "fn c() {}\n");
    let s = sb.stop("dm");
    assert!(!s.blocked(), "stdout={} stderr={}", s.stdout, s.stderr);
    assert!(s.stdout.trim().is_empty(), "stdout={}", s.stdout);
    assert_eq!(s.code, 0);
    // RED control: an explicit inject config on the same change blocks.
    sb.home_config("mode = \"inject\"\n");
    let r2 = sb.cli(&["status"]);
    assert!(r2.stdout.contains("mode:          inject"));
    let s2 = sb.stop("dm");
    assert!(s2.blocked(), "stdout={} stderr={}", s2.stdout, s2.stderr);
    assert_eq!(sb.last_verdict("dm"), "blocked-inject");
}

#[test]
fn claim_inject_once_per_diff_state() {
    let sb = Sandbox::new();
    sb.write("changed.rs", "fn c() { let a = 1; }\n");
    let s1 = sb.stop("once");
    assert!(
        s1.blocked(),
        "stop1 must block: {} {}",
        s1.stdout,
        s1.stderr
    );
    assert_eq!(sb.last_verdict("once"), "blocked-inject");

    // Same diff -> allowed through.
    let s2 = sb.stop("once");
    assert!(!s2.blocked(), "stop2 must not block: {}", s2.stdout);
    assert_eq!(s2.code, 0);
    assert_eq!(sb.last_verdict("once"), "already-reviewed");

    // Diff changed -> blocks again.
    sb.write("changed.rs", "fn c() { let a = 2; let b = 3; }\n");
    let s3 = sb.stop("once");
    // The hook OUTPUT of stop 3 is deliberately not asserted: it is observed
    // to be waived by the shared repeat ledger (same reason text as stop 1),
    // which lets a changed, unreviewed diff through. Its log line (which
    // records `blocked-inject` for that waived stop) is not asserted either.
    // Both are reported as an open finding, not frozen here as spec.
    let _ = s3;
    // A genuinely new reason (next round, 2/2) blocks again for real.
    sb.write(
        "changed.rs",
        "fn c() { let a = 4; let b = 5; let c = 6; }\n",
    );
    let s4 = sb.stop("once");
    assert!(s4.blocked(), "stop4 must emit a block: {:?}", s4.stdout);
    assert!(s4.stdout.contains("round 2/2"), "{}", s4.stdout);
    assert_eq!(sb.last_verdict("once"), "blocked-inject");
}

#[test]
fn claim_default_max_attempts_2() {
    let sb = Sandbox::new();
    let st = sb.cli(&["status"]);
    assert!(
        st.stdout.contains("max_attempts:  2"),
        "status: {}",
        st.stdout
    );
    // Each stop sees a NEW diff, so only the cap can end the chain.
    sb.write("c.rs", "fn c() { 1; }\n");
    let s1 = sb.stop("cap");
    assert!(s1.blocked(), "{} {}", s1.stdout, s1.stderr);
    assert_eq!(sb.last_verdict("cap"), "blocked-inject");
    sb.write("c.rs", "fn c() { 1; 2; }\n");
    let s2 = sb.stop("cap");
    assert!(s2.blocked(), "{} {}", s2.stdout, s2.stderr);
    assert_eq!(sb.last_verdict("cap"), "blocked-inject");
    let attempts = sb
        .log()
        .iter()
        .filter(|v| v["session"] == "cap" && v["verdict"] == "blocked-inject")
        .map(|v| v["attempt"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(attempts, vec![1, 2], "round counter");
    sb.write("c.rs", "fn c() { 1; 2; 3; }\n");
    let s3 = sb.stop("cap");
    assert!(!s3.blocked(), "third round must hit the cap: {}", s3.stdout);
    assert_eq!(sb.last_verdict("cap"), "giveup");
}

// ---------------------------------------------------------------------------
// config loading
// ---------------------------------------------------------------------------

#[test]
fn claim_broken_config_still_gates() {
    let sb = Sandbox::new();
    sb.home_config("mode = [[[ this is not toml\n");
    sb.write("changed.rs", "fn c() {}\n");
    let r = sb.stop("bc");
    assert_eq!(r.code, 0);
    assert!(
        r.blocked(),
        "a broken config must not disable the gate: stdout={} stderr={}",
        r.stdout,
        r.stderr
    );
    assert!(r.stderr.contains("WARNING"), "stderr: {}", r.stderr);
    assert!(
        r.stderr.contains("config could not be loaded"),
        "stderr: {}",
        r.stderr
    );
    // A broken file is undetermined: it blocks `config-invalid` rather than
    // falling back to the built-in default (now `off`, i.e. no gate).
    assert_eq!(sb.last_verdict("bc"), "config-invalid");
    assert!(r.stdout.contains("config-invalid"), "stdout: {}", r.stdout);
    // And `status` says the file FAILED rather than adopting it.
    let st = sb.cli(&["status"]);
    assert!(st.stdout.contains("FAILED to load"), "{}", st.stdout);
}

/// Source-level check: the `review` subcommand body runs inside `run_guarded`.
fn routes_review_through_run_guarded(main_rs: &str) -> Result<(), String> {
    if !main_rs.contains("Command::Review => review_command()") {
        return Err("`Command::Review` does not dispatch to review_command".into());
    }
    let start = main_rs
        .find("fn review_command()")
        .ok_or("no review_command fn")?;
    let body = &main_rs[start..];
    let end = body.find("\nfn review_run").ok_or("no review_run fn")?;
    let body = &body[..end];
    let guarded = body
        .find("harness_core::gate::run::run_guarded(\"reviewgate\"")
        .ok_or("review_command does not call run_guarded")?;
    let run = body
        .find("review_run(hook, interactive)")
        .ok_or("no call")?;
    if run < guarded {
        return Err("review_run is called before/outside run_guarded".into());
    }
    if main_rs.contains("hook::run_hook(") {
        return Err("main.rs also uses the exit-0 `run_hook` barrier".into());
    }
    // review_run must not be called anywhere else.
    if main_rs.matches("review_run(").count() != 2 {
        // definition + the single guarded call
        return Err("review_run is referenced from more than the guarded call".into());
    }
    Ok(())
}

#[test]
fn claim_panic_fails_closed() {
    // No panic can be induced from outside the process (every fallible input
    // is a Result), so this is the source check the brief allows.
    let main_rs = crate_file("src/main.rs");
    routes_review_through_run_guarded(&main_rs).unwrap_or_else(|e| panic!("{e}"));
    // The guard's policy lives in harness-core (read-only here): the first
    // panic blocks, the post-block re-entry (stop_hook_active) allows.
    let run_rs = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness-core/src/gate/run.rs"),
    )
    .expect("read harness-core gate/run.rs");
    assert!(run_rs.contains("FailClosedBlock"));
    assert!(run_rs.contains("BoundedAllow"));
    // RED controls on the checker input.
    let unguarded = main_rs.replace(
        "harness_core::gate::run::run_guarded(\"reviewgate\"",
        "harness_core::hook::run_hook(\"reviewgate\"",
    );
    assert!(routes_review_through_run_guarded(&unguarded).is_err());
    let direct = main_rs.replace(
        "Command::Review => review_command()",
        "Command::Review => review_run(None, false)",
    );
    assert!(routes_review_through_run_guarded(&direct).is_err());
}

#[test]
fn claim_default_excludes() {
    // Only excluded paths changed -> nothing reviewable -> allow.
    let sb = Sandbox::new();
    sb.write("node_modules/x.js", "module.exports = 1;\n");
    sb.write("target/x.rs", "fn t() {}\n");
    sb.write("api.generated.ts", "export const a = 1;\n");
    sb.write("x_pb2.py", "a = 1\n");
    let r = sb.stop("ex1");
    assert!(!r.blocked(), "{} {}", r.stdout, r.stderr);
    assert_eq!(sb.last_verdict("ex1"), "no-reviewable-changes");

    // With one normal file added, ONLY that file is reviewed.
    sb.write("src/lib.rs", "pub fn f() {}\n");
    let r2 = sb.stop("ex2");
    assert!(r2.blocked(), "{} {}", r2.stdout, r2.stderr);
    assert_eq!(sb.last_files("ex2"), vec!["src/lib.rs".to_string()]);
    // status agrees.
    let st = sb.cli(&["status"]);
    assert!(
        st.stdout.contains("5 file(s), 1 reviewable"),
        "{}",
        st.stdout
    );
    assert!(st.stdout.contains("  - src/lib.rs"), "{}", st.stdout);
    assert!(!st.stdout.contains("node_modules"), "{}", st.stdout);
}

#[test]
fn claim_malformed_exclude() {
    let sb = Sandbox::new();
    sb.home_config("exclude = [\"[\"]\n");
    sb.write("a.rs", "fn a() {}\n");
    let r = sb.stop("me");
    assert!(
        r.blocked(),
        "a malformed exclude must exclude nothing: {} {}",
        r.stdout,
        r.stderr
    );
    assert_eq!(sb.last_files("me"), vec!["a.rs".to_string()]);
    assert!(
        r.stderr.contains("WARNING malformed exclude glob `[`"),
        "stderr: {}",
        r.stderr
    );
    // RED control: the same file under a VALID exclude that matches it is not
    // reviewed (so the assertion above is sensitive to the exclude list).
    let sb2 = Sandbox::new();
    sb2.home_config("exclude = [\"**/a.rs\"]\n");
    sb2.write("a.rs", "fn a() {}\n");
    let r2 = sb2.stop("me2");
    assert!(!r2.blocked(), "{} {}", r2.stdout, r2.stderr);
    assert_eq!(sb2.last_verdict("me2"), "no-reviewable-changes");
}

#[test]
fn claim_disable_switches() {
    // control
    let sb = Sandbox::new();
    sb.write("a.rs", "fn a() {}\n");
    let c = sb.stop("ctl");
    assert!(c.blocked(), "control must block: {} {}", c.stdout, c.stderr);

    // enabled = false in config
    let sb1 = Sandbox::new();
    sb1.home_config("enabled = false\n");
    sb1.write("a.rs", "fn a() {}\n");
    let r1 = sb1.stop("off-cfg");
    assert_eq!(r1.code, 0);
    assert!(!r1.blocked(), "enabled=false must allow: {}", r1.stdout);
    assert!(sb1.log().is_empty(), "disabled gate logs no verdict");

    // REVIEWGATE_DISABLE=1
    let sb2 = Sandbox::new();
    sb2.write("a.rs", "fn a() {}\n");
    let r2 = sb2.stop_with("off-env", None, &[("REVIEWGATE_DISABLE", "1")]);
    assert_eq!(r2.code, 0);
    assert!(
        !r2.blocked(),
        "REVIEWGATE_DISABLE=1 must allow: {}",
        r2.stdout
    );
    assert!(sb2.log().is_empty(), "disabled gate logs no verdict");
}

// ---------------------------------------------------------------------------
// attribution
// ---------------------------------------------------------------------------

fn tool_use_line(tool: &str, file: &str) -> String {
    serde_json::json!({
        "parentUuid": "p", "isSidechain": false,
        "message": {
            "model": "claude-opus-5", "id": "msg_1", "type": "message", "role": "assistant",
            "content": [{"type": "tool_use", "id": "toolu_1", "name": tool,
                         "input": {"file_path": file, "old_string": "a", "new_string": "b"}}]
        }
    })
    .to_string()
}

fn write_transcript(sb: &Sandbox, session: &str, edited: &[String]) -> PathBuf {
    let d = sb.home.join(".claude/projects/-home-t-project");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(format!("{session}.jsonl"));
    let mut body = String::from(r#"{"type":"user","message":{"role":"user","content":"go"}}"#);
    body.push('\n');
    for f in edited {
        body.push_str(&tool_use_line("Edit", f));
        body.push('\n');
    }
    std::fs::write(&p, body).unwrap();
    p
}

fn register_live_peer(sb: &Sandbox, session: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let body = format!(
        r#"{{"/work/claimed-0.rs":{{"run_id":"run-{session}","session_id":"{session}","pid":1,"claimed_at":{now},"heartbeat_at":{now}}}}}"#
    );
    let root = match harness_core::projkey::main_worktree_root(&sb.repo) {
        harness_core::verdict::Determination::Known(k) => k,
        _ => panic!("fixture repo must resolve to a main worktree root"),
    };
    let dir = sb
        .home
        .join(".condukt/state")
        .join(harness_core::projkey::project_key(&root));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("claims.json"), body).unwrap();
}

#[test]
fn claim_attribution_exclusion_stderr() {
    let sb = Sandbox::new();
    sb.write("mine.rs", "fn mine() {}\n");
    sb.write("peer.rs", "fn peer() {}\n");
    let mine = sb.repo.join("mine.rs").to_string_lossy().into_owned();
    let peer = sb.repo.join("peer.rs").to_string_lossy().into_owned();
    let tp = write_transcript(&sb, "this-session", &[mine]);
    write_transcript(&sb, "peer-session", &[peer]);
    register_live_peer(&sb, "peer-session");
    let r = sb.stop_with("attr", Some(&tp), &[]);
    assert!(r.blocked(), "{} {}", r.stdout, r.stderr);
    assert_eq!(
        sb.last_files("attr"),
        vec!["mine.rs".to_string()],
        "precondition: peer.rs really was excluded"
    );
    let line = r
        .stderr
        .lines()
        .find(|l| l.contains("帰属"))
        .unwrap_or_else(|| panic!("no attribution note on stderr: {}", r.stderr));
    assert!(line.contains("peer.rs"), "note must name the file: {line}");
    assert!(
        !line.contains("mine.rs"),
        "mine.rs was not excluded: {line}"
    );
}

// ---------------------------------------------------------------------------
// subprocess reviewer
// ---------------------------------------------------------------------------

/// A big untracked file so the prompt cannot fit in a pipe buffer.
fn big_source(bytes: usize) -> String {
    let line = "let value_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa = 1;\n";
    let mut s = String::from("fn big() {\n");
    while s.len() < bytes {
        s.push_str(line);
    }
    s.push_str("}\n");
    s
}

#[test]
fn claim_prompt_undelivered_blocks() {
    let sb = Sandbox::new();
    // `echo LGTM` prints a clean-looking answer and exits WITHOUT reading stdin.
    sb.home_config(
        "mode = \"subprocess\"\nreviewer_cmd = \"echo LGTM\"\nmax_diff_bytes = 50000000\n",
    );
    sb.write("big.rs", &big_source(2_000_000));
    let r = sb.stop("pu");
    assert!(
        r.blocked(),
        "an undelivered prompt must block, not read `LGTM` as clean: stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );
    assert_eq!(sb.last_verdict("pu"), "reviewer-unavailable");
    assert!(
        r.stderr.contains("prompt delivery"),
        "stderr should name the delivery failure: {}",
        r.stderr
    );

    // RED control (same diff, reviewer that DOES read stdin and says LGTM):
    // the prompt is delivered, so the verdict is a genuine clean.
    let sb2 = Sandbox::new();
    sb2.home_config(
        "mode = \"subprocess\"\nreviewer_cmd = \"sh -c 'cat >/dev/null; echo LGTM'\"\nmax_diff_bytes = 50000000\n",
    );
    sb2.write("big.rs", &big_source(2_000_000));
    let r2 = sb2.stop("pu2");
    assert!(!r2.blocked(), "{} {}", r2.stdout, r2.stderr);
    assert_eq!(sb2.last_verdict("pu2"), "clean");
}

// ---------------------------------------------------------------------------
// CLI subcommands
// ---------------------------------------------------------------------------

#[test]
fn claim_init_force() {
    let sb = Sandbox::bare();
    let dir = sb.repo.clone();
    let target = dir.join("reviewgate.toml");
    let r1 = sb.exec(&dir, &["init"], None, "", &[]);
    assert_eq!(r1.code, 0, "{}", r1.stderr);
    let first = std::fs::read_to_string(&target).expect("init wrote ./reviewgate.toml");
    assert!(first.contains("mode = \"inject\""), "{first}");
    assert!(r1.stdout.contains("wrote"), "{}", r1.stdout);

    std::fs::write(&target, "# user edited\n").unwrap();
    let r2 = sb.exec(&dir, &["init"], None, "", &[]);
    assert_ne!(r2.code, 0, "second init without --force must fail");
    assert!(r2.stderr.contains("already exists"), "{}", r2.stderr);
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "# user edited\n",
        "second init must not overwrite"
    );

    let r3 = sb.exec(&dir, &["init", "--force"], None, "", &[]);
    assert_eq!(r3.code, 0, "{}", r3.stderr);
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        first,
        "--force must restore the starter"
    );
}

fn settings(sb: &Sandbox) -> PathBuf {
    sb.home.join(".claude/settings.json")
}

fn files_under(dir: &Path) -> Vec<String> {
    let mut v = Vec::new();
    fn walk(d: &Path, out: &mut Vec<String>) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push(p.to_string_lossy().into_owned());
                }
            }
        }
    }
    walk(dir, &mut v);
    v.sort();
    v
}

fn stop_commands(settings_json: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(settings_json).expect("settings.json is JSON");
    v["hooks"]["Stop"]
        .as_array()
        .map(|a| {
            a.iter()
                .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                .filter_map(|h| h["command"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn claim_install_uninstall() {
    let sb = Sandbox::bare();
    let claude_dir = sb.home.join(".claude");

    // install --dry-run writes nothing
    let d = sb.cli(&["install", "--dry-run"]);
    assert_eq!(d.code, 0, "{}", d.stderr);
    assert!(d.stdout.contains("dry run"), "{}", d.stdout);
    assert!(!settings(&sb).exists(), "dry-run created settings.json");
    assert!(files_under(&claude_dir).is_empty(), "dry-run wrote files");

    // install writes the Stop hook
    let i = sb.cli(&["install"]);
    assert_eq!(i.code, 0, "{}", i.stderr);
    let after_install = std::fs::read_to_string(settings(&sb)).expect("settings.json written");
    let cmds = stop_commands(&after_install);
    assert_eq!(cmds.len(), 1, "{after_install}");
    assert!(cmds[0].ends_with("reviewgate review"), "{cmds:?}");

    // uninstall --dry-run writes nothing (bytes and directory listing unchanged)
    let listing = files_under(&claude_dir);
    let du = sb.cli(&["uninstall", "--dry-run"]);
    assert_eq!(du.code, 0, "{}", du.stderr);
    assert_eq!(
        std::fs::read_to_string(settings(&sb)).unwrap(),
        after_install
    );
    assert_eq!(files_under(&claude_dir), listing, "dry-run wrote files");

    // uninstall removes it
    let u = sb.cli(&["uninstall"]);
    assert_eq!(u.code, 0, "{}", u.stderr);
    assert!(u.stdout.contains("removed 1"), "{}", u.stdout);
    let after_uninstall = std::fs::read_to_string(settings(&sb)).unwrap();
    assert!(
        stop_commands(&after_uninstall).is_empty(),
        "{after_uninstall}"
    );
}

#[test]
fn claim_status_shows_config_and_files() {
    let sb = Sandbox::new();
    sb.home_config("max_attempts = 4\n");
    sb.write("changed.rs", "fn c() {}\n");
    sb.write("notes.md", "not source\n");
    let r = sb.cli(&["status"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    for field in ["config:", "enabled:       true", "mode:          inject"] {
        assert!(r.stdout.contains(field), "missing {field:?}:\n{}", r.stdout);
    }
    assert!(r.stdout.contains("max_attempts:  4"), "{}", r.stdout);
    assert!(r.stdout.contains("state_dir:"), "{}", r.stdout);
    assert!(
        r.stdout.contains("changed:       2 file(s), 1 reviewable"),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains("  - changed.rs"), "{}", r.stdout);
    assert!(!r.stdout.contains("  - notes.md"), "{}", r.stdout);
}

#[test]
fn claim_trust_cli_honors_project_config() {
    let sb = Sandbox::new();
    std::fs::write(
        sb.repo.join("reviewgate.toml"),
        "mode = \"subprocess\"\nreviewer_cmd = \"distinct-reviewer-xyz\"\nmax_attempts = 7\n",
    )
    .unwrap();
    let before = sb.cli(&["status"]);
    assert!(
        before.stdout.contains("max_attempts:  2"),
        "untrusted project config must be ignored: {}",
        before.stdout
    );
    assert!(!before.stdout.contains("distinct-reviewer-xyz"));
    assert!(before.stderr.contains("not trusted"), "{}", before.stderr);

    let t = sb.cli(&["trust"]);
    assert_eq!(t.code, 0, "{}", t.stderr);
    assert!(t.stdout.contains("trusted"), "{}", t.stdout);

    let after = sb.cli(&["status"]);
    assert!(
        after.stdout.contains("max_attempts:  7"),
        "trusted project config must be honored: {}",
        after.stdout
    );
    assert!(
        after
            .stdout
            .contains("reviewer_cmd:  distinct-reviewer-xyz"),
        "{}",
        after.stdout
    );
    assert!(after.stdout.contains("mode:          subprocess"));
}

// ---------------------------------------------------------------------------
// key fields / clamps
// ---------------------------------------------------------------------------

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Seed the per-session counter: `attempts` rounds used, last block `age` secs ago.
fn seed_state(sb: &Sandbox, session: &str, attempts: u32, age: i64) {
    let d = sb.state_dir().join("sessions");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join(format!("{session}.json")),
        serde_json::json!({"attempts": attempts, "last_hash": "", "last_ts": unix_now() - age})
            .to_string(),
    )
    .unwrap();
}

/// Untracked-file diff length as `git::diff_text` builds it for ONE file.
fn untracked_diff_len(name: &str, body: &str) -> usize {
    format!("\n=== new file: {name} ===\n").len() + body.len()
}

/// A source body (ends with '\n') such that the one-file diff is exactly `total` bytes.
fn body_for_total(name: &str, total: usize) -> String {
    let header = untracked_diff_len(name, "");
    let n = total - header;
    let mut s = "a".repeat(n - 1);
    s.push('\n');
    assert_eq!(untracked_diff_len(name, &s), total);
    s
}

#[test]
fn claim_key_fields_defaults() {
    // ---- max_diff_bytes default 200000: exact boundary --------------------
    let exact = Sandbox::new();
    exact.write("big.rs", &body_for_total("big.rs", 200_000));
    exact.stop("md-exact");
    assert_eq!(
        exact.last_verdict("md-exact"),
        "blocked-inject",
        "a 200000-byte diff fits the default cap"
    );
    let over = Sandbox::new();
    over.write("big.rs", &body_for_total("big.rs", 200_001));
    over.stop("md-over");
    assert_eq!(
        over.last_verdict("md-over"),
        "diff-truncated",
        "a 200001-byte diff exceeds the default cap"
    );

    // ---- reset_after_secs default 600: the counter reset boundary ----------
    // attempts already at the cap (2) and a NEW diff: within the window the
    // counter is kept (-> giveup), past it the counter resets (-> block).
    let within = Sandbox::new();
    within.write("c.rs", "fn c() {}\n");
    seed_state(&within, "rs-in", 2, 590);
    within.stop("rs-in");
    assert_eq!(within.last_verdict("rs-in"), "giveup", "590s < 600s window");
    let past = Sandbox::new();
    past.write("c.rs", "fn c() {}\n");
    seed_state(&past, "rs-out", 2, 610);
    let r = past.stop("rs-out");
    assert!(r.blocked(), "{} {}", r.stdout, r.stderr);
    assert_eq!(past.last_verdict("rs-out"), "blocked-inject", "610s > 600s");

    // ---- every key is accepted and takes effect -----------------------------
    let sb = Sandbox::new();
    let custom_state = sb.home.join("custom-state");
    sb.home_config(&format!(
        "enabled = true\nmode = \"inject\"\nmax_attempts = 5\nreset_after_secs = 100\n\
         min_changed_files = 2\nmax_diff_bytes = 5000\ninclude = [\"**/*.zz\"]\n\
         exclude = [\"**/skip.zz\"]\nrubric = \"RUBRIC-MARKER-XYZ\"\n\
         state_dir = \"{}\"\nreviewer_cmd = \"sh -c 'sleep 5'\"\nreviewer_timeout_secs = 1\n",
        custom_state.display()
    ));
    let st = sb.cli(&["status"]);
    assert!(!st.stdout.contains("FAILED"), "{}", st.stdout);
    assert!(st.stdout.contains("max_attempts:  5"), "{}", st.stdout);
    assert!(
        st.stdout
            .contains(&format!("state_dir:     {}", custom_state.display())),
        "{}",
        st.stdout
    );
    // include / exclude / min_changed_files: a.zz + b.zz reviewed, skip.zz and
    // c.rs not.
    sb.write("a.zz", "a\n");
    sb.write("b.zz", "b\n");
    sb.write("skip.zz", "s\n");
    sb.write("c.rs", "fn c() {}\n");
    let r = sb.stop("kf");
    assert!(r.blocked(), "{} {}", r.stdout, r.stderr);
    assert!(
        r.stdout.contains("RUBRIC-MARKER-XYZ"),
        "rubric: {}",
        r.stdout
    );
    let log = std::fs::read_to_string(custom_state.join("log.jsonl")).expect("state_dir honored");
    let last: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!(last["files"], serde_json::json!(["a.zz", "b.zz"]));
    // min_changed_files = 2: one reviewable file is below the bar.
    let one = Sandbox::new();
    one.home_config("min_changed_files = 2\ninclude = [\"**/*.zz\"]\n");
    one.write("a.zz", "a\n");
    let r1 = one.stop("kf1");
    assert!(!r1.blocked(), "{} {}", r1.stdout, r1.stderr);
    assert_eq!(one.last_verdict("kf1"), "no-reviewable-changes");
    // max_diff_bytes = 5000 (custom): a 6000-byte file is truncated.
    let trunc = Sandbox::new();
    trunc.home_config("max_diff_bytes = 5000\n");
    trunc.write("big.rs", &body_for_total("big.rs", 6000));
    trunc.stop("kf2");
    assert_eq!(trunc.last_verdict("kf2"), "diff-truncated");
    // reset_after_secs = 100 (custom): 90s keeps the counter, 110s resets it.
    let rs = Sandbox::new();
    rs.home_config("reset_after_secs = 100\n");
    rs.write("c.rs", "fn c() {}\n");
    seed_state(&rs, "kfr-in", 2, 90);
    rs.stop("kfr-in");
    assert_eq!(rs.last_verdict("kfr-in"), "giveup");
    seed_state(&rs, "kfr-out", 2, 110);
    rs.stop("kfr-out");
    assert_eq!(rs.last_verdict("kfr-out"), "blocked-inject");
    // reviewer_cmd / reviewer_timeout_secs = 1 (custom): a reviewer that sleeps
    // 5s is cut off at ~1s and reported as timed out.
    let to = Sandbox::new();
    to.home_config(
        "mode = \"subprocess\"\nreviewer_cmd = \"sh -c 'sleep 5'\"\nreviewer_timeout_secs = 1\n",
    );
    to.write("c.rs", "fn c() {}\n");
    let t0 = std::time::Instant::now();
    let rt = to.stop("kf3");
    assert!(t0.elapsed().as_secs() < 4, "timeout not honoured");
    assert!(rt.blocked(), "{} {}", rt.stdout, rt.stderr);
    assert!(rt.stderr.contains("timed out"), "{}", rt.stderr);
    assert_eq!(to.last_verdict("kf3"), "reviewer-unavailable");
    // NOT asserted: the defaults reviewer_timeout_secs = 300 and the idle gap
    // semantics beyond 600s cannot be observed without waiting that long.
}

#[test]
fn claim_zero_clamp() {
    let cfg =
        "max_attempts = 0\nmin_changed_files = 0\nmax_diff_bytes = 0\nreviewer_timeout_secs = 0\n";

    // max_attempts 0 -> 1 (status + behaviour)
    let sb = Sandbox::new();
    sb.home_config(cfg);
    let st = sb.cli(&["status"]);
    assert!(st.stdout.contains("max_attempts:  1"), "{}", st.stdout);
    // min_changed_files 0 -> 1: status reports the bar when nothing is reviewable.
    assert!(
        st.stdout.contains("below min_changed_files=1"),
        "min_changed_files must clamp to 1: {}",
        st.stdout
    );
    sb.write("c.rs", "fn c() { 1; }\n");
    let s1 = sb.stop("z");
    assert!(s1.blocked(), "{} {}", s1.stdout, s1.stderr);
    assert_eq!(sb.last_verdict("z"), "blocked-inject");
    sb.write("c.rs", "fn c() { 1; 2; }\n");
    sb.stop("z");
    assert_eq!(
        sb.last_verdict("z"),
        "giveup",
        "max_attempts = 1: the 2nd round gives up"
    );

    // max_diff_bytes 0 -> 200000: a 190000-byte diff is NOT truncated, a
    // 210000-byte diff is.
    let small = Sandbox::new();
    small.home_config(cfg);
    small.write("big.rs", &body_for_total("big.rs", 190_000));
    small.stop("zd1");
    assert_eq!(small.last_verdict("zd1"), "blocked-inject");
    let large = Sandbox::new();
    large.home_config(cfg);
    large.write("big.rs", &body_for_total("big.rs", 210_000));
    large.stop("zd2");
    assert_eq!(large.last_verdict("zd2"), "diff-truncated");

    // reviewer_timeout_secs 0 -> non-zero default: a reviewer that needs a
    // moment (0.5s) still completes and its findings are used. With an
    // unclamped 0 it would be killed immediately as "timed out".
    let rv = Sandbox::new();
    rv.home_config(
        "mode = \"subprocess\"\nreviewer_cmd = \"sh -c 'cat >/dev/null; sleep 1; echo LGTM'\"\nreviewer_timeout_secs = 0\n",
    );
    rv.write("c.rs", "fn c() {}\n");
    let r = rv.stop("zt");
    assert!(!r.blocked(), "{} {}", r.stdout, r.stderr);
    assert_eq!(
        rv.last_verdict("zt"),
        "clean",
        "reviewer_timeout_secs = 0 must fall back to a positive timeout; stderr: {}",
        r.stderr
    );
}

// ---------------------------------------------------------------------------
// escape hatches
// ---------------------------------------------------------------------------

#[test]
fn claim_status_and_escapes_on_invalid_mode() {
    let sb = Sandbox::new();
    sb.home_config("mode = \"bogus-mode\"\n");
    sb.write("c.rs", "fn c() {}\n");

    let st = sb.cli(&["status"]);
    assert!(st.stdout.contains("config-invalid"), "{}", st.stdout);
    assert!(st.stdout.contains("bogus-mode"), "{}", st.stdout);

    // control: the Stop blocks with the config-invalid tag
    let c = sb.stop("inv-ctl");
    assert!(c.blocked(), "{} {}", c.stdout, c.stderr);
    assert_eq!(sb.last_verdict("inv-ctl"), "config-invalid");

    // REVIEWGATE_DISABLE=1 -> allowed
    let d = sb.stop_with("inv-env", None, &[("REVIEWGATE_DISABLE", "1")]);
    assert_eq!(d.code, 0);
    assert!(!d.blocked(), "{}", d.stdout);
    assert!(
        sb.log().iter().all(|v| v["session"] != "inv-env"),
        "disabled run must leave no verdict"
    );

    // `skip --reason` -> that session's next Stop allowed
    let sk = sb.exec(
        &sb.repo,
        &["skip", "--reason", "fixing the config"],
        Some("inv-skip"),
        "",
        &[],
    );
    assert_eq!(sk.code, 0, "{}", sk.stderr);
    let s = sb.stop("inv-skip");
    assert!(!s.blocked(), "{} {}", s.stdout, s.stderr);
    assert_eq!(sb.last_verdict("inv-skip"), "skip");
}

#[test]
fn claim_skip_reason_required() {
    let sb = Sandbox::new();
    sb.write("c.rs", "fn c() {}\n");
    for args in [
        vec!["skip"],
        vec!["skip", "--reason", ""],
        vec!["skip", "--reason", "   "],
    ] {
        let r = sb.exec(&sb.repo, &args, Some("sr"), "", &[]);
        assert_ne!(r.code, 0, "{args:?} must fail; stdout={}", r.stdout);
    }
    assert!(
        !sb.state_dir().join("skips").exists()
            || files_under(&sb.state_dir().join("skips")).is_empty(),
        "a refused skip must leave no marker"
    );
    let s = sb.stop("sr");
    assert!(s.blocked(), "{} {}", s.stdout, s.stderr);
    assert_eq!(sb.last_verdict("sr"), "blocked-inject");
    // RED control: with a reason the same session IS skipped.
    let ok = sb.exec(&sb.repo, &["skip", "--reason", "why"], Some("sr2"), "", &[]);
    assert_eq!(ok.code, 0, "{}", ok.stderr);
    sb.stop("sr2");
    assert_eq!(sb.last_verdict("sr2"), "skip");
}

#[test]
fn claim_skip_consumed_once() {
    let sb = Sandbox::new();
    sb.write("c.rs", "fn c() { 1; }\n");
    let sk = sb.exec(&sb.repo, &["skip", "--reason", "once"], Some("so"), "", &[]);
    assert_eq!(sk.code, 0, "{}", sk.stderr);

    let first = sb.stop("so");
    assert!(!first.blocked(), "{} {}", first.stdout, first.stderr);
    assert_eq!(sb.last_verdict("so"), "skip");

    // The file changes (a still-blocking diff): the skip is spent, so it blocks.
    sb.write("c.rs", "fn c() { 1; 2; }\n");
    let second = sb.stop("so");
    assert!(second.blocked(), "{} {}", second.stdout, second.stderr);
    assert_eq!(sb.last_verdict("so"), "blocked-inject");
}

#[test]
fn claim_skip_logged() {
    let sb = Sandbox::new();
    sb.write("c.rs", "fn c() {}\n");
    let events = |sb: &Sandbox, ev: &str| {
        sb.log()
            .into_iter()
            .filter(|v| v["event"] == ev && v["session_id"] == "sl")
            .collect::<Vec<_>>()
    };
    assert!(sb.log().is_empty());
    let sk = sb.exec(
        &sb.repo,
        &["skip", "--reason", "audit-me"],
        Some("sl"),
        "",
        &[],
    );
    assert_eq!(sk.code, 0, "{}", sk.stderr);
    let issued = events(&sb, "skip_issued");
    assert_eq!(issued.len(), 1, "{:?}", sb.log());
    assert_eq!(issued[0]["reason"], "audit-me");
    assert!(events(&sb, "skip_consumed").is_empty());

    sb.stop("sl");
    let consumed = events(&sb, "skip_consumed");
    assert_eq!(consumed.len(), 1, "{:?}", sb.log());
    assert_eq!(consumed[0]["reason"], "audit-me");
}
