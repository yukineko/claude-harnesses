#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `github_mirror = false` in backlog's `config.toml` turns the GitHub issue
//! mirror OFF (user request 2026-11-17: stop the mail from claude-harness ->
//! stop the issue mirror).
//!
//! Written by an independent test writer (CLAUDE.md §2(a)); the implementation
//! is not part of this commit.
//!
//! Spec pinned here, with `github_mirror = false` in `$HOME/.backlog/config.toml`:
//!   1. `backlog add` on a GitHub remote does NOT run `gh issue create`; the
//!      row is written to the store as usual, exit 0, and stderr says the
//!      mirror is disabled (an explicit sentence, not silence).
//!   2. `backlog sync --apply` runs NO `gh` at all (no view / create / close),
//!      says "mirror disabled" and exits 0.
//!   3. The SessionStart hook does NOT emit the "GitHub mirror drift" section;
//!      it states on one line that the mirror is disabled instead.
//!
//! Controls: with `github_mirror` absent or `true`, the mirror runs as before.
//! Non-bool value (`github_mirror = "no"`): must NOT silently fall back to the
//! default; it is reported as an error (expected: non-zero exit, stderr names
//! `github_mirror`, and no `gh` mutation runs, since whether mirroring is
//! allowed cannot be determined — CLAUDE.md §3).
//!
//! OBSERVED existing config.rs behaviour (measured 2026-11-17 at main
//! 76e053bf3, by reading `Config::load` and by the two `observed_*` tests
//! below, which PASS on the current code):
//!   - `config.toml` unreadable (e.g. it is a directory) -> `read_to_string`
//!     fails, the `if let Ok` drops it, ALL keys take their defaults, nothing
//!     is printed.
//!   - `config.toml` not valid TOML (or a field of the wrong type for the
//!     current `FileConfig`) -> `toml::from_str` fails, the `if let Ok` drops
//!     it, ALL keys take their defaults, nothing is printed.
//!   - `FileConfig` has no `deny_unknown_fields`, so today `github_mirror = ...`
//!     of any type is an unknown key and is silently ignored.
//!
//! "nothing is printed" is read from the code, NOT asserted by these tests
//! (they observe only that the mirror stays ON).
//!
//! The spec says unreadable / unparseable config follows that existing
//! behaviour, so the `observed_*` tests pin "defaults => mirror ON" for those
//! two cases. NOTE for the implementer: if `github_mirror` is added to
//! `FileConfig` as `Option<bool>`, a value of `"no"` makes the WHOLE file
//! unparseable and falls into the silent-defaults path — exactly what the
//! non-bool tests below forbid. The two must be told apart.
//!
//! `gh` is a stub on PATH that appends its argv to a log; "was gh called" is
//! read from that log. HOME is a per-test temp dir, so the operator's real
//! `~/.backlog/config.toml` is never read.
mod common;

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn tool_path(name: &str) -> PathBuf {
    for d in std::env::var("PATH").unwrap().split(':') {
        let p = PathBuf::from(d).join(name);
        if p.exists() {
            return p;
        }
    }
    panic!("no {name} on PATH");
}

fn uniq(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("bl-mirroroff-{tag}-{}-{nanos}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn git_in(cwd: &Path, args: &[&str]) -> (bool, String, String) {
    let o = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn git_ok(cwd: &Path, args: &[&str]) -> String {
    let (ok, out, err) = git_in(cwd, args);
    assert!(ok, "git {args:?} in {} failed: {err}", cwd.display());
    out
}

fn write_exec(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `gh` that logs every argv line, answers `issue view` with OPEN JSON and
/// `issue create` with an issue URL, and exits 0 for everything.
fn gh_stub(log: &Path) -> String {
    format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
         if [ \"$1\" = issue ] && [ \"$2\" = view ]; then echo '{{\"state\":\"OPEN\"}}'; fi\n\
         if [ \"$1\" = issue ] && [ \"$2\" = create ]; then echo https://github.com/o/r/issues/901; fi\n\
         exit 0\n",
        log = log.display()
    )
}

/// What to put in `$HOME/.backlog/config.toml`.
enum Cfg {
    /// No config file at all.
    Absent,
    /// A config file with this exact body.
    Body(&'static str),
    /// `config.toml` exists but is a DIRECTORY, so it cannot be read.
    Unreadable,
}

fn write_cfg(home: &Path, cfg: &Cfg) {
    let dir = home.join(".backlog");
    std::fs::create_dir_all(&dir).unwrap();
    match cfg {
        Cfg::Absent => {}
        Cfg::Body(b) => std::fs::write(dir.join("config.toml"), b).unwrap(),
        Cfg::Unreadable => std::fs::create_dir_all(dir.join("config.toml")).unwrap(),
    }
}

fn read_log(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

// ---------------------------------------------------------------- add ------

struct AddEnv {
    root: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    repo: PathBuf,
    ghlog: PathBuf,
    title: String,
}

/// Same shape as `add_integrates_row_to_main_before_issue.rs`: a linked
/// worktree on a work branch, `remote.origin.url` reads as GitHub,
/// `remote.origin.pushurl` is a local bare repo, so `add`'s land-to-main step
/// can succeed and the only observable difference is whether `gh` runs.
fn add_setup(tag: &str, cfg: Cfg) -> AddEnv {
    let root = uniq(&format!("add-{tag}"));
    let (home, bin, repo, bare, ghlog) = (
        root.join("home"),
        root.join("bin"),
        root.join("repo"),
        root.join("origin.git"),
        root.join("gh.log"),
    );
    for d in [&home, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    write_cfg(&home, &cfg);
    common::linked_checkout(&repo);
    let primary = root.join("repo.main");
    git_ok(&repo, &["checkout", "-q", "-b", "work/mirror-off"]);
    common::commit_repro(&repo);

    std::fs::create_dir_all(&bare).unwrap();
    git_ok(&bare, &["init", "-q", "--bare", "-b", "main"]);
    git_ok(&primary, &["push", "-q", bare.to_str().unwrap(), "main"]);
    git_ok(
        &primary,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    git_ok(
        &primary,
        &["config", "remote.origin.pushurl", bare.to_str().unwrap()],
    );

    for t in ["git", "grep", "cat", "sh"] {
        symlink(tool_path(t), bin.join(t)).unwrap();
    }
    symlink("/bin/bash", bin.join("bash")).unwrap();
    write_exec(
        &bin.join("condukt"),
        "#!/bin/sh\necho '{\"claimed\":false}'\nexit 1\n",
    );
    let _ = Command::new(bin.join("condukt"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    write_exec(&bin.join("gh"), &gh_stub(&ghlog));

    let title = format!(
        "mirroroff-{tag}-{}",
        root.file_name().unwrap().to_string_lossy()
    );
    AddEnv {
        root,
        home,
        bin,
        repo,
        ghlog,
        title,
    }
}

fn run_add(e: &AddEnv) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args([
            "add",
            "--title",
            &e.title,
            "--project",
            ".",
            "--repro-test",
            common::COMMITTED_REPRO,
        ])
        .current_dir(&e.repo)
        .env_clear()
        .env("HOME", &e.home)
        .env("PATH", &e.bin)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn says_mirror_disabled(s: &str) -> bool {
    s.lines().any(|l| {
        let l = l.to_lowercase();
        l.contains("mirror") && l.contains("disabled")
    })
}

/// Spec 1. Dies if `add` still runs `gh issue create`, does not write the row,
/// exits non-zero, or is silent about the mirror being off.
#[test]
fn add_with_mirror_false_does_not_create_issue_and_says_so() {
    let e = add_setup("false", Cfg::Body("github_mirror = false\n"));
    let (code, stdout, stderr) = run_add(&e);
    let log = read_log(&e.ghlog);
    assert_eq!(
        code, 0,
        "add must succeed with the mirror off\nstdout: {stdout}\nstderr: {stderr}"
    );
    let id = common::parse_added(&stdout);
    assert!(
        !log.contains("issue create"),
        "github_mirror=false: gh issue create must not run; gh.log:\n{log}\nstderr: {stderr}"
    );
    let store = std::fs::read_to_string(e.repo.join(".backlog/tasks.toml")).unwrap_or_default();
    assert!(
        store.contains(&id) && store.contains(&e.title),
        "the row must be written to the store as usual:\n{store}"
    );
    assert!(
        says_mirror_disabled(&stderr),
        "stderr must say the GitHub mirror is disabled (no silence): {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

/// Control: no `github_mirror` key -> mirror on, exactly one issue create.
#[test]
fn control_add_without_key_creates_issue() {
    let e = add_setup("absent", Cfg::Absent);
    let (code, stdout, stderr) = run_add(&e);
    let log = read_log(&e.ghlog);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        log.matches("issue create").count(),
        1,
        "default must mirror; gh.log:\n{log}"
    );
    assert!(!says_mirror_disabled(&stderr), "stderr: {stderr:?}");
    let _ = std::fs::remove_dir_all(&e.root);
}

/// Control: `github_mirror = true` -> mirror on, exactly one issue create.
#[test]
fn control_add_with_mirror_true_creates_issue() {
    let e = add_setup("true", Cfg::Body("github_mirror = true\n"));
    let (code, stdout, stderr) = run_add(&e);
    let log = read_log(&e.ghlog);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        log.matches("issue create").count(),
        1,
        "github_mirror=true must mirror; gh.log:\n{log}"
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

/// Non-bool value: not silently the default. Dies if `add` exits 0, does not
/// name `github_mirror` on stderr, or runs `gh issue create` anyway.
#[test]
fn add_with_non_bool_mirror_value_is_an_error() {
    let e = add_setup("nonbool", Cfg::Body("github_mirror = \"no\"\n"));
    let (code, stdout, stderr) = run_add(&e);
    let log = read_log(&e.ghlog);
    assert!(
        !log.contains("issue create"),
        "a non-bool github_mirror cannot authorise mirroring; gh.log:\n{log}"
    );
    assert_ne!(
        code, 0,
        "a non-bool github_mirror must be an error, not the default\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("github_mirror"),
        "the error must name the offending key: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

/// OBSERVATION of existing behaviour (spec: follow config.rs as it is). A
/// config.toml that is not valid TOML is dropped wholesale -> defaults ->
/// mirror ON. Passes on 76e053bf3.
#[test]
fn observed_unparseable_config_falls_back_to_defaults_mirror_on() {
    let e = add_setup("badtoml", Cfg::Body("github_mirror = = false\n[[[\n"));
    let (code, stdout, stderr) = run_add(&e);
    let log = read_log(&e.ghlog);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        log.matches("issue create").count(),
        1,
        "observed: unparseable config -> defaults -> mirror on; gh.log:\n{log}"
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

/// OBSERVATION of existing behaviour. An unreadable config.toml (a directory)
/// is dropped -> defaults -> mirror ON. Passes on 76e053bf3.
#[test]
fn observed_unreadable_config_falls_back_to_defaults_mirror_on() {
    let e = add_setup("unreadable", Cfg::Unreadable);
    let (code, stdout, stderr) = run_add(&e);
    let log = read_log(&e.ghlog);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        log.matches("issue create").count(),
        1,
        "observed: unreadable config -> defaults -> mirror on; gh.log:\n{log}"
    );
    let _ = std::fs::remove_dir_all(&e.root);
}

// --------------------------------------------------------------- sync ------

/// One mirrored open row (#77, `gh issue view` target), one pending row never
/// mirrored (Create), one done row whose issue #78 is not closed (Close).
const SYNC_PENDING: &str = r#"[[task]]
id = "open0077"
title = "pending, issue open"
project = "/repo"
tags = []
status = "pending"
notes = ""
created_at = 2
updated_at = 2
weight = 0.0
issue_number = 77
issue_url = "https://github.com/o/r/issues/77"

[[task]]
id = "new00001"
title = "pending, never mirrored"
project = "/repo"
tags = []
status = "pending"
notes = ""
created_at = 3
updated_at = 3
weight = 0.0
"#;

const SYNC_DONE: &str = r#"[[task]]
id = "done0078"
title = "done, issue still open"
project = "/repo"
tags = []
status = "done"
notes = ""
created_at = 4
updated_at = 4
weight = 0.0
issue_number = 78
issue_url = "https://github.com/o/r/issues/78"
"#;

struct SyncRun {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    log: String,
}

fn run_sync_apply(tag: &str, cfg: Cfg) -> SyncRun {
    let root = uniq(&format!("sync-{tag}"));
    let (home, bin, repo, ghlog) = (
        root.join("home"),
        root.join("bin"),
        root.join("repo"),
        root.join("gh.log"),
    );
    for d in [&home, &bin, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    write_cfg(&home, &cfg);
    symlink(tool_path("git"), bin.join("git")).unwrap();
    write_exec(&bin.join("gh"), &gh_stub(&ghlog));
    common::linked_checkout(&repo);
    git_ok(
        &repo,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    std::fs::create_dir_all(repo.join(".backlog")).unwrap();
    std::fs::write(repo.join(".backlog/tasks.toml"), SYNC_PENDING).unwrap();
    std::fs::write(repo.join(".backlog/tasks.done.toml"), SYNC_DONE).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(["sync", "--apply"])
        .current_dir(&repo)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &bin)
        .output()
        .unwrap();
    let r = SyncRun {
        code: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        log: read_log(&ghlog),
    };
    let _ = std::fs::remove_dir_all(&root);
    r
}

/// Spec 2. Dies if `sync --apply` invokes `gh` at all, exits non-zero, or does
/// not say the mirror is disabled.
#[test]
fn sync_apply_with_mirror_false_runs_no_gh_and_says_so() {
    let r = run_sync_apply("false", Cfg::Body("github_mirror = false\n"));
    assert!(
        r.log.trim().is_empty(),
        "github_mirror=false: sync --apply must not run gh at all; gh.log:\n{}\nstdout: {}\nstderr: {}",
        r.log,
        r.stdout,
        r.stderr
    );
    assert_eq!(
        r.code,
        Some(0),
        "stdout: {}\nstderr: {}",
        r.stdout,
        r.stderr
    );
    assert!(
        says_mirror_disabled(&format!("{}\n{}", r.stdout, r.stderr)),
        "sync must say the mirror is disabled: stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );
}

/// Control: no key -> sync --apply creates the unmirrored row's issue and
/// closes the done row's issue. Also proves the fixture really plans both arms,
/// so the empty-log assertion above is not vacuous.
#[test]
fn control_sync_apply_without_key_creates_and_closes() {
    let r = run_sync_apply("absent", Cfg::Absent);
    assert!(
        r.log.contains("issue create") && r.log.contains("issue close"),
        "default must mirror both arms; gh.log:\n{}\nstdout: {}\nstderr: {}",
        r.log,
        r.stdout,
        r.stderr
    );
    assert_eq!(
        r.code,
        Some(0),
        "stdout: {}\nstderr: {}",
        r.stdout,
        r.stderr
    );
}

/// Control: `github_mirror = true` behaves like the default.
#[test]
fn control_sync_apply_with_mirror_true_creates_and_closes() {
    let r = run_sync_apply("true", Cfg::Body("github_mirror = true\n"));
    assert!(
        r.log.contains("issue create") && r.log.contains("issue close"),
        "github_mirror=true must mirror both arms; gh.log:\n{}\nstdout: {}\nstderr: {}",
        r.log,
        r.stdout,
        r.stderr
    );
    assert_eq!(r.code, Some(0), "stderr: {}", r.stderr);
}

/// Non-bool value on sync --apply: an error, and no gh mutation.
#[test]
fn sync_apply_with_non_bool_mirror_value_is_an_error() {
    let r = run_sync_apply("nonbool", Cfg::Body("github_mirror = \"no\"\n"));
    assert!(
        !r.log.contains("issue create") && !r.log.contains("issue close"),
        "a non-bool github_mirror cannot authorise mirroring; gh.log:\n{}",
        r.log
    );
    assert_ne!(
        r.code,
        Some(0),
        "a non-bool github_mirror must be an error\nstdout: {}\nstderr: {}",
        r.stdout,
        r.stderr
    );
    assert!(
        r.stderr.contains("github_mirror"),
        "the error must name the offending key: {:?}",
        r.stderr
    );
}

// --------------------------------------------------------- SessionStart ----

fn hook_ctx(tag: &str, cfg: Cfg) -> (String, String) {
    let root = uniq(&format!("hook-{tag}"));
    let (home, bin, repo, ghlog) = (
        root.join("home"),
        root.join("bin"),
        root.join("repo"),
        root.join("gh.log"),
    );
    for d in [&home, &bin, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    write_cfg(&home, &cfg);
    symlink(tool_path("git"), bin.join("git")).unwrap();
    write_exec(&bin.join("gh"), &gh_stub(&ghlog));
    git_ok(&repo, &["init", "-q", "."]);
    std::fs::create_dir_all(repo.join(".backlog")).unwrap();
    // Drifted shape: one never-mirrored pending row (create) and one done row
    // whose issue is still open (close), with a mirror in use.
    std::fs::write(repo.join(".backlog/tasks.toml"), SYNC_PENDING).unwrap();
    std::fs::write(repo.join(".backlog/tasks.done.toml"), SYNC_DONE).unwrap();
    let payload = format!(
        r#"{{"session_id":"s","cwd":"{}","hook_event_name":"SessionStart"}}"#,
        repo.display()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .arg("session-start")
        .current_dir(&repo)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let ctx = stdout
        .lines()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| {
            v["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_default();
    let log = read_log(&ghlog);
    let _ = std::fs::remove_dir_all(&root);
    (ctx, log)
}

/// Spec 3. Dies if the drift section is still emitted with the mirror off, or
/// if no single line says the mirror is disabled.
#[test]
fn session_start_with_mirror_false_replaces_drift_with_one_disabled_line() {
    let (ctx, log) = hook_ctx("false", Cfg::Body("github_mirror = false\n"));
    assert!(
        !ctx.to_lowercase().contains("mirror drift"),
        "github_mirror=false: no drift warning: {ctx:?}"
    );
    assert!(
        says_mirror_disabled(&ctx),
        "one line must say the GitHub mirror is disabled: {ctx:?}"
    );
    assert!(log.trim().is_empty(), "hook must not run gh: {log}");
}

/// Control: no key -> the drift section is emitted for the same store, which
/// also proves the fixture is in the drifted shape.
#[test]
fn control_session_start_without_key_reports_drift() {
    let (ctx, _) = hook_ctx("absent", Cfg::Absent);
    assert!(
        ctx.contains("GitHub mirror drift"),
        "default must report drift for this store: {ctx:?}"
    );
    assert!(!says_mirror_disabled(&ctx), "{ctx:?}");
}

/// Control: `github_mirror = true` behaves like the default.
#[test]
fn control_session_start_with_mirror_true_reports_drift() {
    let (ctx, _) = hook_ctx("true", Cfg::Body("github_mirror = true\n"));
    assert!(
        ctx.contains("GitHub mirror drift"),
        "github_mirror=true must report drift: {ctx:?}"
    );
}

/// Non-bool value in the hook: no exit code / stderr reaches the agent, so the
/// error must be in additionalContext and must name the key.
#[test]
fn session_start_with_non_bool_mirror_value_reports_the_error() {
    let (ctx, _) = hook_ctx("nonbool", Cfg::Body("github_mirror = \"no\"\n"));
    assert!(
        ctx.contains("github_mirror"),
        "a non-bool github_mirror must be reported in additionalContext: {ctx:?}"
    );
}
