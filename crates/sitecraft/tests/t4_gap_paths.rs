//! T4 gap tests: paths the existing R6 / R10 / F12 tests do not reach.
//!
//! * F12: `--prefix=<dir>` (equals form) picks the node cwd like
//!   `--prefix <dir>`; a prefix resolving outside the project root (by `..`
//!   or by a symlink) is undetermined (exit 2, `check/pages.rs` module docs:
//!   "A prefix that does not resolve to an existing directory inside the root
//!   -> Undetermined"), never a clean run.
//! * D10: non-string `api_paths` entries and entries containing whitespace /
//!   control characters make the config unusable (exit 2, nothing checked).
//! * D6: `sitecraft init` failing after it created something removes what it
//!   created again.
//!
//! No test here covers `check security` under the `app/` layout: the spec
//! does not decide which directory `npm audit` runs in (reported separately).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spec_common::*;

// ---------------------------------------------------------------- F12 ----

const BASE: &str = "http://127.0.0.1:5173";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn install_fake_playwright(pkg: &Path) {
    let dst = pkg.join("node_modules").join("playwright");
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::copy(
        manifest_dir().join("assets/tests/fake_modules/playwright/index.js"),
        dst.join("index.js"),
    )
    .unwrap();
}

struct PagesRun {
    code: Option<i32>,
    stderr: String,
    /// The fake Playwright logs every call here; present only if it ran.
    playwright_ran: bool,
}

/// Layout: `<tmp>/proj` (root), `<tmp>/proj/app`, `<tmp>/outside`.
/// Playwright is installed only in the directories listed in `playwright_in`
/// (relative to `<tmp>`).
fn pages_run(
    prefix_args: &str,
    playwright_in: &[&str],
    setup: impl FnOnce(&Path, &Path),
) -> PagesRun {
    let node_ok = Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(node_ok, "`node` is not runnable");
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().canonicalize().unwrap();
    let root = tmp.join("proj");
    let app = root.join("app");
    let outside = tmp.join("outside");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    for d in [&app, &outside] {
        std::fs::write(d.join("package.json"), r#"{"name":"x","private":true}"#).unwrap();
    }
    setup(&root, &outside);
    std::fs::write(
        root.join("sitecraft.toml"),
        format!(
            "command = [\"npm\", {prefix_args}, \"run\", \"dev\", \"--\", \"--port\", \"5173\", \"--strictPort\"]\n\
             port = 5173\npages = [\"/\"]\nbuild_dir = \"app/dist\"\nweb_globs = [\"app/src/**\"]\n"
        ),
    )
    .unwrap();
    for rel in playwright_in {
        install_fake_playwright(&tmp.join(rel));
    }
    let scenario = serde_json::json!({
        "pages": { format!("{BASE}/"): { "status": 200, "events": [], "links": [] } }
    });
    let scenario_path = tmp.join("scenario.json");
    std::fs::write(&scenario_path, scenario.to_string()).unwrap();
    let log = tmp.join("calls.jsonl");
    let out = Command::new(env!("CARGO_BIN_EXE_sitecraft"))
        .args(["--root"])
        .arg(&root)
        .args(["check", "pages", "--base-url", BASE])
        .current_dir(&root)
        .env("SITECRAFT_NODE", "node")
        .env("SITECRAFT_STATE_DIR", tmp.join("state"))
        .env("CLAUDE_PLUGIN_ROOT", manifest_dir())
        .env_remove("NODE_PATH")
        .env_remove("NODE_OPTIONS")
        .env("FAKE_PLAYWRIGHT_SCENARIO", &scenario_path)
        .env("FAKE_PLAYWRIGHT_LOG", &log)
        .output()
        .unwrap();
    PagesRun {
        code: out.status.code(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        playwright_ran: log.exists(),
    }
}

#[test]
fn prefix_equals_form_is_honoured_for_the_node_cwd() {
    // Playwright resolvable ONLY from <root>/app/node_modules.
    let r = pages_run("\"--prefix=app\"", &["proj/app"], |_, _| {});
    assert_eq!(
        r.code,
        Some(0),
        "`--prefix=app` must find app/: {}",
        r.stderr
    );
    assert!(r.playwright_ran, "playwright never ran: {}", r.stderr);
    // Controls: the space form works on the same layout, and with Playwright
    // nowhere the equals form is still "not installed" (so the 0 above really
    // came from the app/ resolution).
    let r = pages_run("\"--prefix\", \"app\"", &["proj/app"], |_, _| {});
    assert_eq!(r.code, Some(0), "space-form control: {}", r.stderr);
    let r = pages_run("\"--prefix=app\"", &[], |_, _| {});
    assert_eq!(r.code, Some(2), "nowhere control: {}", r.stderr);
    assert!(r.stderr.contains("not installed"), "{}", r.stderr);
}

#[test]
fn prefix_outside_the_root_is_undetermined_not_clean() {
    // Playwright exists in the outside directory; resolving the prefix there
    // would give a clean run. Spec: outside the root -> Undetermined (exit 2).
    for (label, args) in [
        ("space form ..", "\"--prefix\", \"../outside\""),
        ("equals form ..", "\"--prefix=../outside\""),
    ] {
        let r = pages_run(args, &["outside"], |_, _| {});
        assert_eq!(r.code, Some(2), "{label}: must be exit 2: {}", r.stderr);
        assert!(!r.playwright_ran, "{label}: node must not have run");
        assert!(r.stderr.contains("outside"), "{label}: {}", r.stderr);
    }
}

#[test]
fn prefix_symlinked_outside_the_root_is_undetermined_not_clean() {
    let r = pages_run("\"--prefix\", \"link\"", &["outside"], |root, outside| {
        std::os::unix::fs::symlink(outside, root.join("link")).unwrap();
    });
    assert_eq!(r.code, Some(2), "symlink escaping the root: {}", r.stderr);
    assert!(!r.playwright_ran, "node must not have run");
    // Positive control: a symlink that stays inside the root is accepted.
    let r = pages_run("\"--prefix\", \"link\"", &["proj/app"], |root, _| {
        std::os::unix::fs::symlink(root.join("app"), root.join("link")).unwrap();
    });
    assert_eq!(r.code, Some(0), "inside symlink control: {}", r.stderr);
}

// ---------------------------------------------------------------- D10 ----

/// Accepts on `port`, records request lines, answers 200 to everything.
fn api_server(port: u16) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let l = TcpListener::bind(("127.0.0.1", port)).expect("bind api listener");
    let rec = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { continue };
            let rec = Arc::clone(&rec);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                rec.lock().unwrap().push(line.trim().to_string());
                let _ = s.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                );
            });
        }
    });
    seen
}

fn api_project(api_paths_toml: &str) -> (Project, u16, PathBuf) {
    let p = Project::new();
    let port = p.write_sitecraft_toml(&format!("api_paths = {api_paths_toml}\n"));
    let node = p.base.join("bin/node-stub");
    write_exec(
        &node,
        &format!(
            "#!/bin/sh\nmkdir -p '{m}'\n: > '{m}/node-ran'\necho '{{\"schema\":\"sitecraft-check-pages/1\",\"status\":\"ok\",\"pages_checked\":1,\"findings\":[]}}'\nexit 0\n",
            m = p.base.join("markers").display()
        ),
    );
    (p, port, node)
}

fn check_pages(p: &Project, node: &Path) -> Out {
    p.run_env(&["check", "pages"], &[("SITECRAFT_NODE", os(node))])
}

/// `bad` must give exit 2 with nothing checked and nothing requested; the
/// same root with `good` (and a listener) is clean.
fn assert_unusable_then_control(bad: &str, good: &str, what: &str) {
    let (p, port, node) = api_project(bad);
    let seen = api_server(port);
    let o = check_pages(&p, &node);
    expect_code(&o, 2, what);
    assert!(
        !p.base.join("markers/node-ran").exists(),
        "{what}: nothing may be checked with an unusable config"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "{what}: no request may be sent: {:?}",
        seen.lock().unwrap()
    );
    let cfg = std::fs::read_to_string(p.path("sitecraft.toml")).unwrap();
    assert!(cfg.contains(bad), "{what}: test setup lost the bad value");
    p.write("sitecraft.toml", cfg.replace(bad, good));
    expect_code(&check_pages(&p, &node), 0, &format!("{what}: control"));
}

#[test]
fn non_string_api_paths_entries_are_unusable() {
    for (bad, what) in [
        ("[1]", "integer entry"),
        ("[\"/api/ok\", 1]", "integer after a valid entry"),
        ("[{ path = \"/api/ok\" }]", "table entry"),
        ("[true]", "boolean entry"),
        ("[[\"/api/ok\"]]", "nested array entry"),
    ] {
        assert_unusable_then_control(bad, "[\"/api/ok\"]", &format!("api_paths {what}"));
    }
}

#[test]
fn api_paths_not_an_array_is_unusable() {
    assert_unusable_then_control("\"/api/ok\"", "[\"/api/ok\"]", "api_paths as a string");
}

// code contract from config::api_paths docstring; spec D10 silent on whitespace
#[test]
fn api_path_with_whitespace_or_control_chars_is_unusable() {
    for (bad, what) in [
        ("[\"/api/a b\"]", "space"),
        ("[\"/api/a\\tb\"]", "tab"),
        ("[\"/api/x\\r\\nHost: evil\"]", "CRLF header injection"),
        ("[\"/api/x\\u0000\"]", "NUL"),
    ] {
        assert_unusable_then_control(bad, "[\"/api/a%20b\"]", &format!("api path with {what}"));
    }
}

// ----------------------------------------------------------------- D6 ----

fn init_with_fake_git(p: &Project, dir: &Path, init_body: &str) -> Out {
    let bin = p.base.join("fake-git-bin");
    write_exec(
        &bin.join("git"),
        &format!(
            "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = init ]; then\n    for d in \"$@\"; do :; done\n    {init_body}\n  fi\ndone\necho 'fatal: not a git repository (or any of the parent directories): .git' >&2\nexit 128\n"
        ),
    );
    let mut c = p.bare_cmd();
    c.arg("init")
        .arg(dir)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()));
    run_deadline(c, None, Duration::from_secs(90))
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn init_failing_after_git_init_half_ran_leaves_nothing_in_a_new_tree() {
    let p = Project::new();
    // `git init` leaves a half-made repo dir and fails.
    let top = p.base.join("a");
    let dir = top.join("b/site");
    let o = init_with_fake_git(&p, &dir, "mkdir -p \"$d/.git\"; exit 1");
    expect_code(&o, 2, "git init failing after creating its directory");
    assert!(
        !top.exists(),
        "everything init created (including parents) must be removed: {}",
        o.describe()
    );
}

#[test]
fn init_failing_after_git_init_half_ran_leaves_an_existing_empty_dir_empty() {
    let p = Project::new();
    let dir = p.base.join("empty-site");
    std::fs::create_dir_all(&dir).unwrap();
    let o = init_with_fake_git(&p, &dir, "mkdir -p \"$d/.git\"; exit 1");
    expect_code(&o, 2, "git init failing in an existing empty dir");
    assert!(dir.is_dir(), "the user's own directory must stay");
    assert_eq!(names(&dir), Vec::<String>::new(), "{}", o.describe());
}

#[test]
fn init_failing_to_create_sitecraft_dir_removes_what_it_made() {
    let p = Project::new();
    // git "succeeds" and leaves a regular file named sitecraft, so creating
    // the sitecraft/ directory fails after the repo dir exists.
    let body = "mkdir -p \"$d/.git\"; : > \"$d/sitecraft\"; exit 0";
    let dir = p.base.join("c/site");
    let o = init_with_fake_git(&p, &dir, body);
    expect_code(&o, 2, "creating sitecraft/ failing");
    assert!(!p.base.join("c").exists(), "{}", o.describe());
}

#[test]
fn init_with_git_exit_0_but_no_repo_dir_is_exit_2_and_cleaned() {
    let p = Project::new();
    let dir = p.base.join("d/site");
    let o = init_with_fake_git(&p, &dir, "exit 0");
    expect_code(&o, 2, "git init exits 0 without creating the repo dir");
    assert!(!p.base.join("d").exists(), "{}", o.describe());
}
