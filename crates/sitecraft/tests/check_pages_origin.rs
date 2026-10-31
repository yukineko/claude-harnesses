#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `sitecraft check pages` must not report Clean for a `pages` entry that
//! leaves the configured origin (absolute `http://other.example/x` or
//! protocol-relative `//other.example/x`).
//!
//! These run the real `sitecraft` binary, the real bundled
//! `assets/check-pages.mjs` and real `node`, with the shared fake
//! `playwright` (`assets/tests/fake_modules`) on `NODE_PATH`, so the outcome
//! is the same whether the off-origin entry is rejected at config load
//! (exit 2) or by the script (finding: exit 1, or error: exit 2). Only exit 0
//! is rejected. Controls prove the harness reaches the script and that a
//! same-origin failure of the same kind is detected.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const BASE: &str = "http://127.0.0.1:5173";

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

struct Out {
    code: Option<i32>,
    stderr: String,
}

/// Write a project with `pages` and a fake-playwright scenario, then run
/// `sitecraft check pages --base-url BASE`.
fn run(pages: &[&str], scenario: &serde_json::Value) -> Out {
    node_available();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    fs::create_dir_all(&root).unwrap();
    let pages_toml = pages
        .iter()
        .map(|p| format!("{p:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        root.join("sitecraft.toml"),
        format!(
            "command = [\"npm\", \"run\", \"dev\"]\nport = 5173\npages = [{pages_toml}]\n\
             build_dir = \"dist\"\nweb_globs = [\"src/**\"]\n"
        ),
    )
    .unwrap();
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
        .env(
            "NODE_PATH",
            manifest_dir().join("assets/tests/fake_modules"),
        )
        .env("FAKE_PLAYWRIGHT_SCENARIO", &scenario_path)
        .env("FAKE_PLAYWRIGHT_LOG", dir.path().join("calls.jsonl"))
        .env_remove("NODE_OPTIONS")
        .output()
        .unwrap();
    Out {
        code: out.status.code(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn page(status: u16) -> serde_json::Value {
    serde_json::json!({ "status": status, "events": [], "links": [] })
}

/// Exit 1 or 2, never 0; and the answer is about the off-origin entry, not
/// an unrelated harness failure.
fn assert_not_clean(name: &str, out: &Out) {
    assert_ne!(
        out.code,
        Some(0),
        "{name}: off-origin pages entry reported CLEAN (fail-open); stderr: {}",
        out.stderr
    );
    assert!(
        matches!(out.code, Some(1) | Some(2)),
        "{name}: expected exit 1 or 2, got {:?}; stderr: {}",
        out.code,
        out.stderr
    );
    assert!(
        out.stderr.contains("other.example"),
        "{name}: verdict does not name the off-origin entry; stderr: {}",
        out.stderr
    );
    for unrelated in ["no scenario for", "cannot run node", "Cannot find module"] {
        assert!(
            !out.stderr.contains(unrelated),
            "{name}: non-clean for an unrelated reason ({unrelated}); stderr: {}",
            out.stderr
        );
    }
}

#[test]
fn absolute_off_origin_pages_entry_is_not_clean() {
    let scenario = serde_json::json!({ "pages": { "http://other.example/x": page(503) } });
    let out = run(&["http://other.example/x"], &scenario);
    assert_not_clean("absolute", &out);
}

#[test]
fn protocol_relative_off_origin_pages_entry_is_not_clean() {
    let scenario = serde_json::json!({ "pages": { "http://other.example/x": page(503) } });
    let out = run(&["//other.example/x"], &scenario);
    assert_not_clean("protocol-relative", &out);
}

#[test]
fn off_origin_entry_beside_same_origin_page_is_not_clean() {
    let scenario = serde_json::json!({
        "pages": { format!("{BASE}/"): page(200), "http://other.example/x": page(503) }
    });
    let out = run(&["/", "http://other.example/x"], &scenario);
    assert_not_clean("mixed", &out);
}

#[test]
fn control_same_origin_pages_are_clean_through_real_script() {
    let scenario = serde_json::json!({
        "pages": { format!("{BASE}/"): page(200), format!("{BASE}/about"): page(200) }
    });
    let out = run(&["/", "/about"], &scenario);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
}

#[test]
fn control_same_origin_503_document_is_a_violation() {
    let scenario = serde_json::json!({ "pages": { format!("{BASE}/x"): page(503) } });
    let out = run(&["/x"], &scenario);
    assert_eq!(out.code, Some(1), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("asset-http-error"),
        "stderr: {}",
        out.stderr
    );
}
