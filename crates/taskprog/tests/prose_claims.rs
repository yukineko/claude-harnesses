//! Reproduction tests for taskprog's prose claims. Each `claim_<id>` drives the
//! real binary (or reads the packaged file) and asserts the specific outcome.

use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn fresh_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("taskprog-prose-{}-{tag}-{id}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// A repo root (has `.git`) with an optional progress file, plus a separate HOME.
fn repo(content: Option<&str>) -> (PathBuf, PathBuf) {
    let root = fresh_dir("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    if let Some(c) = content {
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(root.join(".claude/progress.md"), c).unwrap();
    }
    (root, fresh_dir("home"))
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_env(args: &[&str], cwd: &Path, home: &Path, stdin: &str, env: &[(&str, &str)]) -> Out {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_taskprog"));
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("TASKPROG_DISABLED")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn run(args: &[&str], cwd: &Path, home: &Path, stdin: &str) -> Out {
    run_env(args, cwd, home, stdin, &[])
}

fn hook_json(cwd: &Path) -> String {
    serde_json::json!({"cwd": cwd.to_string_lossy(), "session_id": "s1"}).to_string()
}

#[test]
fn claim_session_start_injects() {
    let (root, home) = repo(Some("# Progress\nMARKER-XYZ pending item\n"));
    let o = run(&["session-start"], &root, &home, &hook_json(&root));
    assert_eq!(o.code, 0);
    let v: Value = serde_json::from_str(o.stdout.trim()).expect("stdout is JSON");
    // ACTUAL: the key is top-level `additionalContext`, NOT
    // `hookSpecificOutput.additionalContext`.
    let ctx = v["additionalContext"].as_str().expect("additionalContext");
    assert!(ctx.contains("MARKER-XYZ pending item"), "{ctx}");
    assert!(v.get("hookSpecificOutput").is_none(), "{v}");
}

#[test]
fn claim_stop_prompts_update() {
    let (root, home) = repo(Some("# Progress\nexisting-content\n"));
    let o = run(&["stop"], &root, &home, &hook_json(&root));
    let v: Value = serde_json::from_str(o.stdout.trim()).expect("stdout is JSON");
    let ctx = v["additionalContext"].as_str().unwrap();
    assert!(ctx.contains("please update the progress file"), "{ctx}");
    assert!(ctx.contains("existing-content"), "{ctx}");
}

#[test]
fn claim_hooks_json_two_hooks() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let hooks = v["hooks"].as_object().unwrap();
    let mut keys: Vec<&str> = hooks.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["SessionStart", "Stop"]);
    let cmd = |ev: &str| {
        let g = &hooks[ev].as_array().unwrap();
        assert_eq!(g.len(), 1);
        let hs = g[0]["hooks"].as_array().unwrap();
        assert_eq!(hs.len(), 1);
        hs[0]["command"].as_str().unwrap().to_string()
    };
    assert!(cmd("SessionStart").ends_with("taskprog session-start"));
    assert!(cmd("Stop").ends_with("taskprog stop"));
}

fn settings(home: &Path) -> PathBuf {
    home.join(".claude/settings.json")
}

fn taskprog_cmds(s: &Value, ev: &str) -> Vec<String> {
    s["hooks"][ev]
        .as_array()
        .map(|a| {
            a.iter()
                .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                .filter_map(|h| h["command"].as_str().map(String::from))
                .filter(|c| c.contains("taskprog"))
                .collect()
        })
        .unwrap_or_default()
}

fn home_with_settings() -> (PathBuf, PathBuf, String) {
    let (root, home) = repo(None);
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let orig = "{\"unrelatedKey\": 42}\n".to_string();
    std::fs::write(settings(&home), &orig).unwrap();
    (root, home, orig)
}

#[test]
fn claim_install_merges_hooks() {
    let (root, home, orig) = home_with_settings();
    // --dry-run leaves the file unchanged.
    let d = run(&["install", "--dry-run"], &root, &home, "");
    assert_eq!(d.code, 0);
    assert_eq!(std::fs::read_to_string(settings(&home)).unwrap(), orig);
    // real install keeps the key and adds both entries.
    let i = run(&["install"], &root, &home, "");
    assert_eq!(i.code, 0, "{}", i.stderr);
    let s: Value =
        serde_json::from_str(&std::fs::read_to_string(settings(&home)).unwrap()).unwrap();
    assert_eq!(s["unrelatedKey"], 42);
    let ss = taskprog_cmds(&s, "SessionStart");
    let st = taskprog_cmds(&s, "Stop");
    assert_eq!(ss.len(), 1, "{s}");
    assert_eq!(st.len(), 1, "{s}");
    assert!(ss[0].ends_with("session-start"));
    assert!(st[0].ends_with(" stop"));
}

#[test]
fn claim_uninstall_removes_hooks() {
    let (root, home, _) = home_with_settings();
    run(&["install"], &root, &home, "");
    let installed = std::fs::read(settings(&home)).unwrap();
    let pre: Value = serde_json::from_slice(&installed).unwrap();
    assert_eq!(
        taskprog_cmds(&pre, "SessionStart").len(),
        1,
        "precondition: {pre}"
    );
    assert_eq!(taskprog_cmds(&pre, "Stop").len(), 1, "precondition: {pre}");
    let d = run(&["uninstall", "--dry-run"], &root, &home, "");
    assert_eq!(d.code, 0);
    assert_eq!(std::fs::read(settings(&home)).unwrap(), installed);
    let u = run(&["uninstall"], &root, &home, "");
    assert_eq!(u.code, 0, "{}", u.stderr);
    let s: Value =
        serde_json::from_str(&std::fs::read_to_string(settings(&home)).unwrap()).unwrap();
    assert!(taskprog_cmds(&s, "SessionStart").is_empty(), "{s}");
    assert!(taskprog_cmds(&s, "Stop").is_empty(), "{s}");
    assert_eq!(s["unrelatedKey"], 42);
}

#[test]
fn claim_show_prints_file() {
    let (root, home) = repo(Some("# Progress\nSHOW-ME-123\n"));
    let o = run(&["show"], &root, &home, "");
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains("SHOW-ME-123"), "{}", o.stdout);
}

#[test]
fn claim_write_from_stdin_default_cwd() {
    let (root, home) = repo(None);
    let o = run(&["write"], &root, &home, "WRITTEN-FROM-STDIN\n");
    assert_eq!(o.code, 0, "{}", o.stderr);
    let got = std::fs::read_to_string(root.join(".claude/progress.md")).unwrap();
    assert!(got.contains("WRITTEN-FROM-STDIN"), "{got}");
}

#[test]
fn claim_init_default_target() {
    let dir = fresh_dir("init");
    let home = fresh_dir("home");
    let o = run(&["init"], &dir, &home, "");
    assert_eq!(o.code, 0, "{}", o.stderr);
    let s = std::fs::read_to_string(dir.join("taskprog.toml")).expect("taskprog.toml exists");
    let t: toml::Value = toml::from_str(&s).expect("parses as TOML");
    assert_eq!(t["enabled"].as_bool(), Some(true));
    assert_eq!(t["inject_limit"].as_integer(), Some(4096));
}

#[test]
fn claim_config_lookup_order() {
    let (root, home) = repo(None);
    std::fs::create_dir_all(home.join(".taskprog")).unwrap();
    std::fs::write(home.join(".taskprog/config.toml"), "inject_limit = 222\n").unwrap();
    // Without the cwd file, HOME config is used.
    let b = run(&["status"], &root, &home, "");
    assert!(b.stdout.contains("inject_limit:   222"), "{}", b.stdout);
    // With a cwd file, it wins.
    std::fs::write(root.join("taskprog.toml"), "inject_limit = 111\n").unwrap();
    let a = run(&["status"], &root, &home, "");
    assert!(a.stdout.contains("inject_limit:   111"), "{}", a.stdout);
}

fn injected(o: &Out) -> String {
    let v: Value = serde_json::from_str(o.stdout.trim()).expect("JSON");
    v["additionalContext"].as_str().unwrap().to_string()
}

#[test]
fn claim_inject_limit_bytes() {
    let body: String = (0..40).map(|i| format!("line-{i:02}-aaaa\n")).collect(); // 13 bytes/line
    let (root, home) = repo(Some(&body));
    std::fs::write(root.join("taskprog.toml"), "inject_limit = 50\n").unwrap();
    let ctx = injected(&run(&["session-start"], &root, &home, &hook_json(&root)));
    assert!(ctx.contains("line-00-aaaa"), "{ctx}");
    assert!(!ctx.contains("line-39-aaaa"), "{ctx}");
    assert!(!ctx.contains("line-10-aaaa"), "{ctx}"); // beyond 50 bytes
    assert!(ctx.contains("truncated"), "{ctx}");
    std::fs::write(root.join("taskprog.toml"), "inject_limit = 0\n").unwrap();
    let ctx = injected(&run(&["session-start"], &root, &home, &hook_json(&root)));
    assert!(
        ctx.contains("line-39-aaaa") && ctx.contains("line-00-aaaa"),
        "{ctx}"
    );
    assert!(!ctx.contains("truncated"), "{ctx}");
}

#[test]
fn claim_tilde_expanded() {
    let (root, home) = repo(None);
    std::fs::create_dir_all(home.join("x")).unwrap();
    std::fs::write(home.join("x/progress.md"), "TILDE-CONTENT\n").unwrap();
    std::fs::write(
        root.join("taskprog.toml"),
        "progress_file = \"~/x/progress.md\"\n",
    )
    .unwrap();
    let o = run(&["session-start"], &root, &home, &hook_json(&root));
    assert!(injected(&o).contains("TILDE-CONTENT"), "{}", o.stdout);
    let s = run(&["status"], &root, &home, "");
    let want = format!("progress_file:  {}", home.join("x/progress.md").display());
    assert!(s.stdout.contains(&want), "{}", s.stdout);
}

#[test]
fn claim_disabled_env() {
    for v in ["1", "true"] {
        let (root, home) = repo(Some("# Progress\nSOMETHING\n"));
        let o = run_env(
            &["session-start"],
            &root,
            &home,
            &hook_json(&root),
            &[("TASKPROG_DISABLED", v)],
        );
        assert_eq!(o.stdout.trim(), "", "session-start with {v}: {}", o.stdout);
        let (root2, home2) = repo(None);
        let o = run_env(
            &["stop"],
            &root2,
            &home2,
            &hook_json(&root2),
            &[("TASKPROG_DISABLED", v)],
        );
        assert_eq!(o.stdout.trim(), "", "stop with {v}: {}", o.stdout);
        assert!(
            !root2.join(".claude/progress.md").exists(),
            "file written with {v}"
        );
    }
    // =0 is normal behaviour.
    let (root, home) = repo(Some("# Progress\nSOMETHING\n"));
    let o = run_env(
        &["session-start"],
        &root,
        &home,
        &hook_json(&root),
        &[("TASKPROG_DISABLED", "0")],
    );
    assert!(injected(&o).contains("SOMETHING"));
    let (root2, home2) = repo(None);
    let o = run_env(
        &["stop"],
        &root2,
        &home2,
        &hook_json(&root2),
        &[("TASKPROG_DISABLED", "0")],
    );
    assert!(injected(&o).contains("progress file"));
    assert!(root2.join(".claude/progress.md").exists());
}

#[test]
fn claim_license_mit() {
    let m =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let t: toml::Value = toml::from_str(&m).unwrap();
    let direct = t["package"]["license"].as_str().map(String::from);
    let resolved = match direct {
        Some(l) => l,
        None => {
            assert_eq!(t["package"]["license"]["workspace"].as_bool(), Some(true));
            let r = std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"),
            )
            .unwrap();
            let r: toml::Value = toml::from_str(&r).unwrap();
            r["workspace"]["package"]["license"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    assert_eq!(resolved, "MIT");
}

#[test]
fn claim_skill_reset_arg() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/taskprog/SKILL.md");
    let s = std::fs::read_to_string(p).unwrap();
    let fm = s
        .strip_prefix("---\n")
        .unwrap()
        .split("\n---")
        .next()
        .unwrap();
    let line = |k: &str| {
        fm.lines()
            .find_map(|l| l.strip_prefix(k).map(|r| r.trim().to_string()))
            .unwrap_or_default()
    };
    assert_eq!(line("name:"), "taskprog");
    assert!(line("argument-hint:").contains("--reset"), "{fm}");
}

#[test]
fn claim_no_root_says_so() {
    let dir = fresh_dir("noroot");
    let home = fresh_dir("home");
    let mut p = dir.clone();
    while let Some(par) = p.parent() {
        assert!(
            !par.join(".git").exists(),
            "precondition: .git above {}",
            par.display()
        );
        p = par.to_path_buf();
    }
    let o = run(&["stop"], &dir, &home, &hook_json(&dir));
    assert!(!dir.join(".claude").exists(), "wrote into cwd");
    assert!(!dir.join(".claude/progress.md").exists());
    let all = format!("{}{}", o.stdout, o.stderr);
    assert!(all.contains("no ancestor") && all.contains(".git"), "{all}");
    assert!(all.contains("no progress file was written"), "{all}");
}
