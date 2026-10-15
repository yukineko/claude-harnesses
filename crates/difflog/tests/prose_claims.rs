#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Prose-claim reproduction tests for difflog (README.md / README.ja.md /
//! SKILL.md). Each test drives the real binary with a fresh tempdir HOME.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

struct Env {
    _td: TempDir,
    home: PathBuf,
    repo: PathBuf,
}

fn env() -> Env {
    let td = tempfile::tempdir().unwrap();
    let home = td.path().join("home");
    let repo = td.path().join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    Env {
        _td: td,
        home,
        repo,
    }
}

fn git_out(repo: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {o:?}");
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn init_repo(repo: &Path) {
    git_out(repo, &["init", "-q"]);
    git_out(repo, &["config", "user.email", "t@t"]);
    git_out(repo, &["config", "user.name", "t"]);
    git_out(repo, &["config", "commit.gpgsign", "false"]);
    git_out(repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
}

fn commit_file(repo: &Path, name: &str, body: &str) {
    std::fs::write(repo.join(name), body).unwrap();
    git_out(repo, &["add", name]);
    git_out(repo, &["commit", "-q", "-m", &format!("add {name}")]);
}

fn run_with(
    e: &Env,
    cwd: &Path,
    args: &[&str],
    stdin: Option<&str>,
    extra: &[(&str, &str)],
) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_difflog"));
    c.args(args)
        .env("HOME", &e.home)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("DIFFLOG_DISABLE")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        c.env(k, v);
    }
    let mut child = c.spawn().unwrap();
    {
        let mut si = child.stdin.take().unwrap();
        if let Some(s) = stdin {
            si.write_all(s.as_bytes()).unwrap();
        }
    }
    child.wait_with_output().unwrap()
}

fn run(e: &Env, args: &[&str]) -> Output {
    run_with(e, &e.repo, args, None, &[])
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn hook_json(e: &Env, sid: &str, event: &str) -> String {
    serde_json::json!({"session_id": sid, "cwd": e.repo, "hook_event_name": event}).to_string()
}

fn logs(e: &Env) -> PathBuf {
    e.home.join(".difflog/logs")
}

fn start(e: &Env, sid: &str) -> Output {
    run_with(
        e,
        &e.repo,
        &["session-start"],
        Some(&hook_json(e, sid, "SessionStart")),
        &[],
    )
}

fn end(e: &Env, sid: &str) -> Output {
    run_with(
        e,
        &e.repo,
        &["session-end"],
        Some(&hook_json(e, sid, "SessionEnd")),
        &[],
    )
}

fn md_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".md"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Full scenario: start, commit a file, end. Returns (env, start_sha).
fn session_scenario(sid: &str) -> (Env, String) {
    let e = env();
    init_repo(&e.repo);
    let start_sha = git_out(&e.repo, &["rev-parse", "HEAD"]).trim().to_string();
    start(&e, sid);
    // Precondition: SessionStart really recorded state.
    assert!(
        logs(&e)
            .join("sessions")
            .join(format!("{sid}.json"))
            .exists(),
        "precondition: session state must exist"
    );
    commit_file(&e.repo, "hello.txt", "hello\n");
    end(&e, sid);
    (e, start_sha)
}

fn skill_text() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/difflog/SKILL.md"))
        .unwrap()
}

fn front_matter_field(text: &str, key: &str) -> Option<String> {
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("---"));
    for l in lines {
        if l == "---" {
            break;
        }
        if let Some(v) = l.strip_prefix(&format!("{key}:")) {
            return Some(v.trim().to_string());
        }
    }
    None
}

#[test]
fn claim_session_start_snapshots_head() {
    let e = env();
    init_repo(&e.repo);
    commit_file(&e.repo, "a.txt", "a\n");
    let head = git_out(&e.repo, &["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(head.len(), 40);
    let o = start(&e, "sess-start-1");
    assert!(o.status.success(), "{o:?}");
    let p = logs(&e).join("sessions/sess-start-1.json");
    let text = std::fs::read_to_string(&p).expect("session state file must exist");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["start_sha"].as_str(), Some(head.as_str()));
}

/// OBSERVED (claim FALSE): the SessionEnd log is never written because
/// `git diff --stat -- <base>..HEAD` treats the range as a pathspec.
#[test]
fn claim_session_end_writes_log() {
    let (e, start_sha) = session_scenario("sess-end-1");
    let range = format!("{start_sha}..HEAD");
    // Control: the commit exists, so a correct range diff is non-empty.
    let correct = git_out(&e.repo, &["diff", "--stat", &range]);
    assert!(correct.contains("hello.txt"), "control: {correct}");
    // What difflog's git.rs invocation (`--` before the range) returns.
    let as_invoked = git_out(&e.repo, &["diff", "--stat", "--", &range]);
    assert_eq!(as_invoked, "", "range after `--` is a pathspec: empty stat");
    // Consequence: no log file is written.
    assert_eq!(md_files(&logs(&e)), Vec::<String>::new());

    // Pin difflog's OWN git argv through a recording shim first on PATH.
    let real_git = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    assert!(!real_git.is_empty());
    let shim_dir = e.home.join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let argv_log = shim_dir.join("argv.log");
    let shim = shim_dir.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            argv_log.display(),
            real_git
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let o = run_with(
        &e,
        &e.repo,
        &["session-end"],
        Some(&hook_json(&e, "sess-end-1", "SessionEnd")),
        &[("PATH", &path)],
    );
    assert!(o.status.success(), "session-end must exit 0: {o:?}");
    let argv = std::fs::read_to_string(&argv_log).expect("shim must have been invoked");
    let want = format!("diff --stat -- {start_sha}..HEAD");
    assert!(
        argv.lines().any(|l| l == want),
        "difflog's git argv lacks {want:?}; recorded:\n{argv}"
    );
    assert_eq!(md_files(&logs(&e)), Vec::<String>::new());
}

/// OBSERVED (claim FALSE): since no log is ever written, no file exists at
/// the documented `<YYYY-MM-DD>-<session8>.md` path.
#[test]
fn claim_log_path_format() {
    let (e, _) = session_scenario("abcdef123456");
    let today = chrono_today();
    let expected = logs(&e).join(format!("{today}-abcdef12.md"));
    assert!(
        !expected.exists(),
        "no log is written, so {expected:?} does not exist"
    );
    assert!(logs(&e).join("sessions/abcdef123456.json").exists());
    assert_eq!(md_files(&logs(&e)), Vec::<String>::new());
}

fn chrono_today() -> String {
    let o = Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn claim_two_hooks_registered() {
    let text =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json"))
            .unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let hooks = v["hooks"].as_object().unwrap();
    let mut keys: Vec<&str> = hooks.keys().map(|s| s.as_str()).collect();
    keys.sort();
    assert_eq!(keys, vec!["SessionEnd", "SessionStart"]);
    let cmd = |k: &str| {
        let arr = hooks[k].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let hs = arr[0]["hooks"].as_array().unwrap();
        assert_eq!(hs.len(), 1);
        hs[0]["command"].as_str().unwrap().to_string()
    };
    assert!(cmd("SessionStart").ends_with("difflog session-start"));
    assert!(cmd("SessionEnd").ends_with("difflog session-end"));
}

fn settings_path(e: &Env) -> PathBuf {
    e.home.join(".claude/settings.json")
}

fn hook_cmds(v: &serde_json::Value, ev: &str) -> Vec<String> {
    v["hooks"][ev]
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
fn claim_install_merges_hooks() {
    let e = env();
    std::fs::create_dir_all(e.home.join(".claude")).unwrap();
    let seed =
        r#"{"model":"x","hooks":{"Stop":[{"hooks":[{"type":"command","command":"other"}]}]}}"#;
    std::fs::write(settings_path(&e), seed).unwrap();
    let o = run(&e, &["install", "--dry-run"]);
    assert!(o.status.success(), "{o:?}");
    assert_eq!(
        std::fs::read_to_string(settings_path(&e)).unwrap(),
        seed,
        "dry-run must leave the file unchanged"
    );
    let dry: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert!(hook_cmds(&dry, "SessionStart")
        .iter()
        .any(|c| c.contains("difflog session-start")));
    assert!(hook_cmds(&dry, "SessionEnd")
        .iter()
        .any(|c| c.contains("difflog session-end")));

    let o = run(&e, &["install"]);
    assert!(o.status.success(), "{o:?}");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(settings_path(&e)).unwrap()).unwrap();
    assert!(hook_cmds(&v, "SessionStart")
        .iter()
        .any(|c| c.contains("difflog session-start")));
    assert!(hook_cmds(&v, "SessionEnd")
        .iter()
        .any(|c| c.contains("difflog session-end")));
    // Foreign content survives the merge.
    assert_eq!(v["model"].as_str(), Some("x"));
    assert_eq!(hook_cmds(&v, "Stop"), vec!["other".to_string()]);
}

#[test]
fn claim_uninstall_removes_hooks() {
    let e = env();
    assert!(run(&e, &["install"]).status.success());
    let before = std::fs::read_to_string(settings_path(&e)).unwrap();
    assert!(before.contains("difflog session-start"), "precondition");
    let o = run(&e, &["uninstall", "--dry-run"]);
    assert!(o.status.success(), "{o:?}");
    assert_eq!(std::fs::read_to_string(settings_path(&e)).unwrap(), before);
    assert!(
        !stdout(&o).contains("difflog session-"),
        "dry-run output shows removal"
    );
    let o = run(&e, &["uninstall"]);
    assert!(o.status.success(), "{o:?}");
    let after = std::fs::read_to_string(settings_path(&e)).unwrap();
    assert!(!after.contains("difflog"), "hooks remain: {after}");
}

fn write_logs(dir: &Path, names: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    for n in names {
        std::fs::write(dir.join(n), format!("content of {n}\n")).unwrap();
    }
}

#[test]
fn claim_last_prints_newest() {
    let e = env();
    write_logs(
        &logs(&e),
        &["2026-01-01-aaaaaaaa.md", "2026-02-01-bbbbbbbb.md"],
    );
    let o = run(&e, &["last"]);
    assert!(o.status.success(), "{o:?}");
    assert_eq!(stdout(&o), "content of 2026-02-01-bbbbbbbb.md\n");
}

#[test]
fn claim_list_newest_first_limit() {
    let e = env();
    let names: Vec<String> = (1..=12)
        .map(|m| format!("2026-{m:02}-01-aaaaaaaa.md"))
        .collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    write_logs(&logs(&e), &refs);
    let o = run(&e, &["list"]);
    let lines: Vec<String> = stdout(&o).lines().map(String::from).collect();
    let mut expect: Vec<String> = names.iter().rev().take(10).cloned().collect();
    assert_eq!(lines, expect);
    let o = run(&e, &["list", "--limit", "3"]);
    let lines: Vec<String> = stdout(&o).lines().map(String::from).collect();
    expect.truncate(3);
    assert_eq!(lines, expect);
}

#[test]
fn claim_init_writes_starter() {
    let e = env();
    let template =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("difflog.example.toml"))
            .unwrap();
    let p = e.repo.join("difflog.toml");
    assert!(!p.exists());
    assert!(run(&e, &["init"]).status.success());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), template);
    std::fs::write(&p, "enabled = false\n").unwrap();
    assert!(run(&e, &["init"]).status.success());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "enabled = false\n");
    assert!(run(&e, &["init", "--force"]).status.success());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), template);
}

#[test]
fn claim_config_project_wins() {
    let e = env();
    std::fs::create_dir_all(e.home.join(".difflog")).unwrap();
    std::fs::write(
        e.home.join(".difflog/config.toml"),
        "log_dir = \"/cfg/A\"\n",
    )
    .unwrap();
    // Control: home config alone is honoured.
    assert!(stdout(&run(&e, &["status"])).contains("log_dir:          /cfg/A"));
    std::fs::write(e.repo.join("difflog.toml"), "log_dir = \"/cfg/B\"\n").unwrap();
    let s = stdout(&run(&e, &["status"]));
    assert!(s.contains("log_dir:          /cfg/B"), "{s}");
    std::fs::remove_file(e.repo.join("difflog.toml")).unwrap();
    let s = stdout(&run(&e, &["status"]));
    assert!(s.contains("log_dir:          /cfg/A"), "{s}");

    // Either/or, not layered: home `enabled = false` must not leak through
    // when a project config exists.
    std::fs::write(
        e.home.join(".difflog/config.toml"),
        "enabled = false\nlog_dir = \"/cfg/A\"\n",
    )
    .unwrap();
    let s = stdout(&run(&e, &["status"]));
    assert!(s.contains("enabled:          false"), "control: {s}");
    std::fs::write(e.repo.join("difflog.toml"), "log_dir = \"/cfg/B\"\n").unwrap();
    let s = stdout(&run(&e, &["status"]));
    assert!(s.contains("enabled:          true"), "{s}");
    assert!(s.contains("log_dir:          /cfg/B"), "{s}");
}

#[test]
fn claim_config_defaults() {
    let e = env();
    let s = stdout(&run(&e, &["status"]));
    let want = format!(
        "enabled:          true\nlog_dir:          {}\ndiff_body_limit:  4096 bytes\nexclude_globs:    [\"Cargo.lock\", \"package-lock.json\", \"*.min.js\", \"*.min.css\"]\n",
        e.home.join(".difflog/logs").display()
    );
    assert_eq!(s, want);
}

/// OBSERVED (claim cannot be exercised): no session-end log is ever written
/// (see claim_session_end_writes_log), so `diff_body_limit` has no observable
/// effect through the binary. Pin: with limit 0 and with a small limit the
/// outcome is identical -- no log.
#[test]
fn claim_diff_body_limit_no_log_effect() {
    {
        let e = env();
        std::fs::write(e.repo.join("difflog.toml"), "diff_body_limit = 7\n").unwrap();
        let s = stdout(&run(&e, &["status"]));
        assert!(s.contains("diff_body_limit:  7 bytes"), "{s}");
    }
    for limit in ["0", "10"] {
        let e = env();
        init_repo(&e.repo);
        std::fs::write(
            e.repo.join("difflog.toml"),
            format!("diff_body_limit = {limit}\n"),
        )
        .unwrap();
        start(&e, "sess-lim");
        assert!(
            logs(&e).join("sessions/sess-lim.json").exists(),
            "precondition"
        );
        commit_file(&e.repo, "big.txt", &"x\n".repeat(200));
        end(&e, "sess-lim");
        assert_eq!(md_files(&logs(&e)), Vec::<String>::new(), "limit={limit}");
    }
}

/// OBSERVED: `exclude_globs` is parsed into Config and printed by `status`,
/// but never read by git.rs / record.rs.
#[test]
fn claim_exclude_globs_effect() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut users = Vec::new();
    let mut seen = 0;
    for ent in std::fs::read_dir(&src).unwrap() {
        let p = ent.unwrap().path();
        if p.extension().is_some_and(|x| x == "rs") {
            seen += 1;
            if std::fs::read_to_string(&p)
                .unwrap()
                .contains("exclude_globs")
            {
                users.push(p.file_name().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    users.sort();
    assert!(seen >= 6, "precondition: enumerated src/*.rs, saw {seen}");
    assert_eq!(users, vec!["config.rs", "main.rs"]);
    // In main.rs the only mention is the status printout.
    let main = std::fs::read_to_string(src.join("main.rs")).unwrap();
    let hits: Vec<&str> = main
        .lines()
        .filter(|l| l.contains("exclude_globs"))
        .collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("println!(\"exclude_globs:"), "{hits:?}");
    // main.rs only prints it (status); control that the binary shows it.
    let e = env();
    std::fs::write(e.repo.join("difflog.toml"), "exclude_globs = [\"zzz\"]\n").unwrap();
    assert!(stdout(&run(&e, &["status"])).contains("[\"zzz\"]"));
}

#[test]
fn claim_disable_env() {
    // Control: without the env, state is written.
    let e = env();
    init_repo(&e.repo);
    start(&e, "sess-dis");
    assert!(logs(&e).join("sessions/sess-dis.json").exists());
    // With the env, nothing is written.
    let e = env();
    init_repo(&e.repo);
    let o = run_with(
        &e,
        &e.repo,
        &["session-start"],
        Some(&hook_json(&e, "sess-dis", "SessionStart")),
        &[("DIFFLOG_DISABLE", "1")],
    );
    assert!(o.status.success(), "{o:?}");
    assert!(!logs(&e).join("sessions/sess-dis.json").exists());
}

#[test]
fn claim_skill_slash_command() {
    let t = skill_text();
    assert_eq!(front_matter_field(&t, "name").as_deref(), Some("difflog"));
}

#[test]
fn claim_skill_session_arg() {
    let t = skill_text();
    let h = front_matter_field(&t, "argument-hint").unwrap();
    assert!(h.contains("--session <id>"), "{h}");
}

#[test]
fn claim_skill_invocation_namespace() {
    let pj = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".claude-plugin/plugin.json"),
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&pj).unwrap();
    assert_eq!(v["name"].as_str(), Some("difflog"));
    assert_eq!(
        front_matter_field(&skill_text(), "name").as_deref(),
        Some("difflog")
    );
}

#[test]
fn claim_log_dir_holds_session_state() {
    let e = env();
    init_repo(&e.repo);
    let head = git_out(&e.repo, &["rev-parse", "HEAD"]).trim().to_string();
    let custom = e.home.parent().unwrap().join("custom");
    std::fs::write(
        e.repo.join("difflog.toml"),
        format!("log_dir = \"{}\"\n", custom.display()),
    )
    .unwrap();
    let o = start(&e, "sess-cust");
    assert!(o.status.success(), "{o:?}");
    let p = custom.join("sessions/sess-cust.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).expect("state in custom log_dir"))
            .unwrap();
    assert_eq!(v["start_sha"].as_str(), Some(head.as_str()));
    assert!(!logs(&e).join("sessions/sess-cust.json").exists());
}
