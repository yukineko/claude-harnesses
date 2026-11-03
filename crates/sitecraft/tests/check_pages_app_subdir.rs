#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! F12 — `sitecraft check pages` must find Playwright in the web-app package
//! when that package lives in a subdirectory of the sitecraft root.
//!
//! Observed 2026-11-03 (sitecraft 0.1.3, testbed ~/src/3d-pipeline-web):
//! `sitecraft.toml` sits at the repo root, the Vite app (with `playwright` in
//! its devDependencies / node_modules) lives in `app/`, and
//! `command = ["npm","--prefix","app","run","dev","--","--port","5173","--strictPort"]`.
//! `check pages` resolves `playwright` only from the root (check-pages.mjs
//! does `createRequire(<cwd = root>/__sitecraft__.cjs)('playwright')`) and
//! exits 2 "playwright is not installed in the project".
//!
//! Contract chosen here (minimal, zero-config): the app package directory is
//! the one the configured `command` already names — `npm --prefix <dir>` —
//! so with exactly the testbed's config and Playwright resolvable ONLY from
//! `<root>/app/node_modules`, `check pages` must resolve it: it must not
//! report "not installed" and, against the fake Playwright answering 200,
//! must exit 0. An implementation that instead requires a new, explicit
//! config key would not satisfy this test with the testbed's unchanged
//! config; that is a deliberate choice of contract (see backlog notes).
//!
//! Controls (both must keep holding after any fix — no fail-open):
//! * `control_playwright_nowhere_still_not_installed` — Playwright neither in
//!   the root nor in `app/` → still exit 2 "playwright is not installed".
//! * `control_playwright_in_root_is_clean` — Playwright in
//!   `<root>/node_modules` (the already-supported layout) → exit 0, proving the
//!   harness (real node + real check-pages.mjs + fake playwright) works.
//!
//! The fake Playwright is the shared `assets/tests/fake_modules/playwright`,
//! copied into the project's node_modules; `NODE_PATH` is removed so nothing
//! but node_modules resolution can find it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BASE: &str = "http://127.0.0.1:5173";
const TESTBED_COMMAND: &str =
    r#"["npm", "--prefix", "app", "run", "dev", "--", "--port", "5173", "--strictPort"]"#;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn node_available() {
    let ok = Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(
        ok,
        "`node` is not runnable; these tests need real node to exercise check-pages.mjs"
    );
}

/// Install the fake playwright as `<pkg>/node_modules/playwright`.
fn install_fake_playwright(pkg: &Path) {
    let dst = pkg.join("node_modules").join("playwright");
    fs::create_dir_all(&dst).unwrap();
    fs::copy(
        manifest_dir().join("assets/tests/fake_modules/playwright/index.js"),
        dst.join("index.js"),
    )
    .unwrap();
}

#[derive(Clone, Copy, PartialEq)]
enum Where {
    Root,
    App,
    Nowhere,
}

struct Out {
    code: Option<i32>,
    stderr: String,
}

fn run(playwright_at: Where) -> Out {
    node_available();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    let app = root.join("app");
    fs::create_dir_all(&app).unwrap();
    fs::write(
        root.join("sitecraft.toml"),
        format!(
            "command = {TESTBED_COMMAND}\nport = 5173\npages = [\"/\"]\n\
             build_dir = \"app/dist\"\nweb_globs = [\"app/src/**\", \"app/index.html\"]\n"
        ),
    )
    .unwrap();
    fs::write(
        app.join("package.json"),
        r#"{"name":"app","private":true,"scripts":{"dev":"vite"},"devDependencies":{"playwright":"*"}}"#,
    )
    .unwrap();
    match playwright_at {
        Where::Root => install_fake_playwright(&root),
        Where::App => install_fake_playwright(&app),
        Where::Nowhere => {}
    }
    let scenario = serde_json::json!({
        "pages": { format!("{BASE}/"): { "status": 200, "events": [], "links": [] } }
    });
    let scenario_path = dir.path().join("scenario.json");
    fs::write(&scenario_path, scenario.to_string()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sitecraft"))
        .arg("--root")
        .arg(&root)
        .arg("check")
        .arg("pages")
        .arg("--base-url")
        .arg(BASE)
        .current_dir(&root)
        .env("SITECRAFT_NODE", "node")
        .env("SITECRAFT_STATE_DIR", dir.path().join("state"))
        .env("CLAUDE_PLUGIN_ROOT", manifest_dir())
        .env_remove("NODE_PATH")
        .env_remove("NODE_OPTIONS")
        .env("FAKE_PLAYWRIGHT_SCENARIO", &scenario_path)
        .env("FAKE_PLAYWRIGHT_LOG", dir.path().join("calls.jsonl"))
        .output()
        .unwrap();
    Out {
        code: out.status.code(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn playwright_in_app_subdir_named_by_npm_prefix_is_resolved() {
    let out = run(Where::App);
    assert!(
        !out.stderr.contains("not installed"),
        "playwright is resolvable from <root>/app/node_modules (the `npm --prefix app` \
         package), but check pages reported it not installed; exit {:?}; stderr: {}",
        out.code,
        out.stderr
    );
    assert_eq!(
        out.code,
        Some(0),
        "fake playwright answers 200 for the only page: expected CLEAN; stderr: {}",
        out.stderr
    );
}

#[test]
fn control_playwright_nowhere_still_not_installed() {
    let out = run(Where::Nowhere);
    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("playwright is not installed"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn control_playwright_in_root_is_clean() {
    let out = run(Where::Root);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
}
