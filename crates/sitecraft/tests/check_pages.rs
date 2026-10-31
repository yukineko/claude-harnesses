//! `sitecraft check pages` — behaviour tests, and **the contract between the
//! Rust side and the bundled `crates/sitecraft/assets/check-pages.mjs`**.
//!
//! The Rust side is exercised through the real `sitecraft` binary with
//! `SITECRAFT_NODE` pointed at `tests/fixtures/pages/stub-node.sh`, which
//! ignores the script and replays a fixed stdout fixture and exit status.
//! The `.mjs` side is exercised by `assets/tests/check-pages.test.mjs`
//! (`node --test`, fake `playwright` module on `NODE_PATH`).
//!
//! # Contract (both sides must follow it)
//!
//! ## Invocation (Rust → node)
//!
//! * Program: `$SITECRAFT_NODE` if set and non-empty, else `node` on `PATH`
//!   ([`sitecraft::program_from_env`]). The environment is inherited.
//! * argv: exactly two arguments after the program:
//!   1. the absolute path of a file named `check-pages.mjs` that **exists at
//!      spawn time** (the bundled asset, or a byte-identical copy of it);
//!   2. one JSON object, the input:
//!      `{"base_url": "<origin>", "pages": ["/", "/about", ...]}`.
//!      `base_url` is the `--base-url` value verbatim, or
//!      `http://127.0.0.1:<port>` (no trailing slash) from `sitecraft.toml`.
//!      `pages` is `sitecraft.toml`'s `pages`, in order, unmodified.
//! * cwd: the project root (`--root`), so the project's own
//!   `node_modules/playwright` resolves.
//! * Empty `pages` → `Undetermined` (exit 2) **without spawning node**.
//!
//! ## Script behaviour (`check-pages.mjs`)
//!
//! * Loads Playwright with CommonJS resolution anchored at the cwd:
//!   `createRequire(path.join(process.cwd(), "<any>.cjs"))("playwright")`.
//!   (A bare ESM `import "playwright"` would ignore `NODE_PATH`; the tests
//!   supply a fake through `NODE_PATH`.)
//! * Each page URL is `new URL(page, base_url).href`. Pages are checked in
//!   order; same origin means `new URL(x).origin === new URL(base_url).origin`.
//! * Playwright API surface it may use (the fake implements exactly this;
//!   anything else throws): `chromium.launch(opts)`; `browser.newPage()`,
//!   `browser.newContext()`, `browser.close()`; `context.newPage()`,
//!   `context.close()`, `context.request`; `page.on(event, fn)` for
//!   `console` / `pageerror` / `response` / `requestfailed`, `page.off`,
//!   `page.goto(url, opts)`, `page.$$eval(selector, fn)`, `page.request`,
//!   `page.waitForLoadState()`, `page.waitForTimeout(ms)`,
//!   `page.setDefaultTimeout(ms)`, `page.setDefaultNavigationTimeout(ms)`,
//!   `page.url()`, `page.close()`; `request.get(url, opts)` /
//!   `request.head(url, opts)` / `request.fetch(url, opts)` returning a
//!   response with `status()` / `ok()` / `url()` / `dispose()`.
//!   Event payloads: console message `type()` / `text()` / `location()`;
//!   `pageerror` an `Error`; response `url()` / `status()` / `ok()` /
//!   `statusText()` / `request()` (`url()` / `method()` / `resourceType()`).
//! * Listeners for `console`, `pageerror` and `response` are registered on a
//!   page **before** that page's `goto`.
//! * Findings (exactly these four `kind`s):
//!   - `console-error`: a console message whose `type()` is `"error"`
//!     (other types are not findings);
//!   - `page-error`: a `pageerror` event;
//!   - `asset-http-error`: a **same-origin** response with status >= 400
//!     (the page document included). Cross-origin responses never count;
//!   - `broken-link`: a **same-origin** `a[href]` (collected with
//!     `page.$$eval("a[href]", els => els.map(e => e.href))`, fragment
//!     stripped, de-duplicated) whose `request.get` has status >= 400 or
//!     throws. Cross-origin links are not requested.
//! * The browser is closed in a `finally` (also when navigation throws).
//!
//! ## Output (stdout) — exactly one JSON object, nothing else
//!
//! ```json
//! {
//!   "schema": "sitecraft-check-pages/1",
//!   "status": "ok" | "error" | "browser-missing" | "playwright-missing",
//!   "pages_checked": 2,
//!   "findings": [
//!     {"page": "http://127.0.0.1:5173/about", "kind": "asset-http-error",
//!      "detail": "GET http://127.0.0.1:5173/assets/app.js -> 404",
//!      "url": "http://127.0.0.1:5173/assets/app.js", "status": 404}
//!   ],
//!   "error": "only present when status is not ok"
//! }
//! ```
//!
//! `page` is the absolute URL of the checked page; `detail` is a string;
//! `url` (string) and `status` (number, or `null` when the request threw) are
//! present for `asset-http-error` and `broken-link`.
//!
//! ## Script exit status
//!
//! | exit | status | meaning |
//! |------|--------|---------|
//! | 0 | `ok` | every page was checked; `findings` may be non-empty |
//! | 2 | `error` | could not check (zero pages, bad input, navigation threw, …) |
//! | 3 | `browser-missing` | `chromium.launch` threw "Executable doesn't exist" |
//! | 4 | `playwright-missing` | `playwright` could not be resolved |
//!
//! Findings are **not** signalled by the exit status.
//!
//! ## Rust verdict (CLI exit 0 / 1 / 2)
//!
//! * **Clean (0)** only if: node exited 0, stdout is exactly one JSON object
//!   (surrounding whitespace allowed) matching the schema above (tag
//!   `sitecraft-check-pages/1`, `status` `ok`, `pages_checked` a number,
//!   `findings` an array of objects each with a non-empty string `page`, a
//!   known `kind` and a string `detail`), `pages_checked` equals the number
//!   of configured pages (so >= 1), and `findings` is empty.
//! * **Violation (1)**: the same conditions but `findings` non-empty. stderr
//!   names every finding's page URL and kind.
//! * **Undetermined (2)**: everything else — node missing, non-zero exit
//!   (even with valid JSON, even with findings), empty / non-JSON / trailing
//!   garbage / schema mismatch, a status other than `ok`, `pages_checked`
//!   different from the number of configured pages, zero pages. For a
//!   missing browser or missing Playwright (from `status`, or from stderr
//!   containing `Executable doesn't exist` / `Cannot find module`) the
//!   reason says so (mentions "browser" / "playwright").
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use sitecraft::config::Config;
use sitecraft::Verdict;

const BASE: &str = "http://127.0.0.1:5173";
const PAGES: &[&str] = &["/", "/about"];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pages")
}

/// A project root with a valid `sitecraft.toml` listing `pages`, plus an
/// executable copy of the stub (the copy gets mode 755 regardless of how the
/// fixture was checked out).
struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new(pages: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pages_toml = pages
            .iter()
            .map(|p| format!("{p:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        fs::write(
            dir.path().join("sitecraft.toml"),
            format!(
                "command = [\"npm\", \"run\", \"dev\"]\nport = 5173\npages = [{pages_toml}]\n\
                 build_dir = \"dist\"\nweb_globs = [\"src/**\"]\n"
            ),
        )
        .unwrap();
        let stub = dir.path().join("stub-node.sh");
        fs::copy(fixtures().join("stub-node.sh"), &stub).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        Project { dir }
    }
    fn root(&self) -> &Path {
        self.dir.path()
    }
    fn stub(&self) -> PathBuf {
        self.root().join("stub-node.sh")
    }
    fn rec(&self) -> PathBuf {
        self.root().join("rec")
    }
    fn spawned(&self) -> bool {
        self.rec().join("spawned").exists()
    }
}

/// How the stub behaves for one run.
#[derive(Default)]
struct Stub<'a> {
    stdout_fixture: Option<&'a str>,
    stderr: Option<&'a str>,
    exit: i32,
}

struct Out {
    code: Option<i32>,
    stderr: String,
}

fn run_cli(p: &Project, node: &Path, stub: &Stub, base_url: Option<&str>) -> Out {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sitecraft"));
    cmd.arg("--root").arg(p.root()).arg("check").arg("pages");
    if let Some(b) = base_url {
        cmd.arg("--base-url").arg(b);
    }
    cmd.current_dir(p.root())
        .env("SITECRAFT_NODE", node)
        .env("SITECRAFT_STATE_DIR", p.root().join("state"))
        .env("STUB_RECORD_DIR", p.rec())
        .env("STUB_EXIT", stub.exit.to_string())
        .env_remove("STUB_STDOUT_FILE")
        .env_remove("STUB_STDERR");
    if let Some(f) = stub.stdout_fixture {
        cmd.env("STUB_STDOUT_FILE", fixtures().join(f));
    }
    if let Some(e) = stub.stderr {
        cmd.env("STUB_STDERR", e);
    }
    let out = cmd.output().unwrap();
    Out {
        code: out.status.code(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn run_stub(stub: &Stub) -> (Project, Out) {
    let p = Project::new(PAGES);
    let out = run_cli(&p, &p.stub(), stub, Some(BASE));
    (p, out)
}

fn fixture_run(name: &str, exit: i32) -> (Project, Out) {
    run_stub(&Stub {
        stdout_fixture: Some(name),
        stderr: None,
        exit,
    })
}

/// exit 2 *and* node was actually spawned: the undetermined answer came from
/// judging the script's result, not from never getting that far.
fn assert_undetermined_after_spawn(name: &str, p: &Project, out: &Out) {
    assert_eq!(
        out.code,
        Some(2),
        "{name}: expected exit 2 (undetermined); stderr: {}",
        out.stderr
    );
    assert!(
        p.spawned(),
        "{name}: node (the stub) was never spawned, so the result was not judged; stderr: {}",
        out.stderr
    );
}

// ---------------------------------------------------------------- clean ----

#[test]
fn zero_findings_with_all_pages_checked_is_clean() {
    let (p, out) = fixture_run("clean.json", 0);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(p.spawned());
}

#[test]
fn invocation_follows_the_contract() {
    let (p, out) = fixture_run("clean.json", 0);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let rec = p.rec();
    assert_eq!(fs::read_to_string(rec.join("argc")).unwrap(), "2");
    let script = PathBuf::from(fs::read_to_string(rec.join("script")).unwrap());
    assert!(script.is_absolute(), "script path not absolute: {script:?}");
    assert_eq!(
        script.file_name().and_then(|s| s.to_str()),
        Some("check-pages.mjs")
    );
    assert!(
        rec.join("script_exists").exists(),
        "script {script:?} did not exist when node was spawned"
    );
    let input: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(rec.join("input")).unwrap()).unwrap();
    assert_eq!(input["base_url"], BASE);
    assert_eq!(input["pages"], serde_json::json!(["/", "/about"]));
    let cwd = fs::read_to_string(rec.join("cwd")).unwrap();
    assert_eq!(
        PathBuf::from(cwd.trim()),
        p.root().canonicalize().unwrap(),
        "node must run with the project root as cwd"
    );
}

#[test]
fn base_url_defaults_to_loopback_and_configured_port() {
    let p = Project::new(PAGES);
    let out = run_cli(
        &p,
        &p.stub(),
        &Stub {
            stdout_fixture: Some("clean.json"),
            ..Stub::default()
        },
        None,
    );
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    let input: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(p.rec().join("input")).unwrap()).unwrap();
    assert_eq!(input["base_url"], "http://127.0.0.1:5173");
}

// ------------------------------------------------------------ violation ----

fn assert_violation(fixture: &str, kind: &str) {
    let (p, out) = fixture_run(fixture, 0);
    assert_eq!(
        out.code,
        Some(1),
        "{fixture}: expected exit 1; stderr: {}",
        out.stderr
    );
    assert!(p.spawned());
    assert!(
        out.stderr.contains("http://127.0.0.1:5173/about"),
        "{fixture}: stderr must name the page URL: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains(kind),
        "{fixture}: stderr must name the kind {kind}: {}",
        out.stderr
    );
}

#[test]
fn console_error_is_a_violation() {
    assert_violation("finding-console-error.json", "console-error");
}

#[test]
fn page_error_is_a_violation() {
    assert_violation("finding-page-error.json", "page-error");
}

#[test]
fn same_origin_asset_http_error_is_a_violation() {
    assert_violation("finding-asset-http-error.json", "asset-http-error");
}

#[test]
fn same_origin_broken_link_is_a_violation() {
    assert_violation("finding-broken-link.json", "broken-link");
}

// --------------------------------------------------------- undetermined ----

#[test]
fn zero_pages_in_config_never_spawns_node() {
    let p = Project::new(&[]);
    let out = run_cli(
        &p,
        &p.stub(),
        &Stub {
            stdout_fixture: Some("clean.json"),
            ..Stub::default()
        },
        Some(BASE),
    );
    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(!p.spawned(), "node must not be spawned for zero pages");
}

/// Library-level twin of the CLI test above: `check_pages` itself refuses
/// empty `pages` without spawning (config validation is not what stops it).
#[test]
fn check_pages_with_empty_pages_is_undetermined_without_spawning() {
    static ENV: Mutex<()> = Mutex::new(());
    let _g = ENV.lock().unwrap();
    let p = Project::new(PAGES);
    std::env::set_var("SITECRAFT_NODE", p.stub());
    std::env::set_var("STUB_RECORD_DIR", p.rec());
    std::env::set_var("STUB_STDOUT_FILE", fixtures().join("clean.json"));
    let cfg = Config {
        command: vec!["npm".into(), "run".into(), "dev".into()],
        port: 5173,
        ready_path: "/".into(),
        ready_timeout_secs: 60,
        pages: vec![],
        build_dir: "dist".into(),
        web_globs: vec!["src/**".into()],
        max_attempts: 3,
        reset_after_secs: 1800,
    };
    let v = sitecraft::check::pages::check_pages(&cfg, BASE);
    assert!(matches!(v, Verdict::Undetermined(_)), "got {v:?}");
    assert!(!p.spawned(), "node must not be spawned for zero pages");
}

#[test]
fn browser_missing_status_is_undetermined() {
    for exit in [3, 0] {
        let (p, out) = fixture_run("browser-missing.json", exit);
        assert_undetermined_after_spawn("browser-missing.json", &p, &out);
        assert!(
            out.stderr.to_lowercase().contains("browser"),
            "exit {exit}: reason must say the browser is missing: {}",
            out.stderr
        );
    }
}

#[test]
fn executable_doesnt_exist_on_stderr_is_undetermined() {
    let (p, out) = run_stub(&Stub {
        stdout_fixture: None,
        stderr: Some(
            "browserType.launch: Executable doesn't exist at /x/Chromium\n\
             Looks like Playwright was just installed. Please run: npx playwright install",
        ),
        exit: 1,
    });
    assert_undetermined_after_spawn("executable-doesnt-exist", &p, &out);
    assert!(
        out.stderr.to_lowercase().contains("browser"),
        "reason must say the browser is missing: {}",
        out.stderr
    );
}

#[test]
fn playwright_missing_is_undetermined() {
    let (p, out) = fixture_run("playwright-missing.json", 4);
    assert_undetermined_after_spawn("playwright-missing.json", &p, &out);
    assert!(
        out.stderr.to_lowercase().contains("playwright"),
        "{}",
        out.stderr
    );

    let (p, out) = run_stub(&Stub {
        stdout_fixture: None,
        stderr: Some(
            "Error: Cannot find module 'playwright'\nRequire stack:\n- /x/__sitecraft__.cjs",
        ),
        exit: 1,
    });
    assert_undetermined_after_spawn("cannot-find-module", &p, &out);
    assert!(
        out.stderr.to_lowercase().contains("playwright"),
        "{}",
        out.stderr
    );
}

#[test]
fn missing_node_executable_is_undetermined() {
    let p = Project::new(PAGES);
    let missing = p.root().join("no-such-node-sitecraft");
    let out = run_cli(&p, &missing, &Stub::default(), Some(BASE));
    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("no-such-node-sitecraft"),
        "reason must name the node executable that could not be run: {}",
        out.stderr
    );
}

#[test]
fn empty_stdout_is_undetermined() {
    let (p, out) = run_stub(&Stub::default());
    assert_undetermined_after_spawn("empty stdout", &p, &out);
}

#[test]
fn malformed_or_mismatched_stdout_is_undetermined() {
    for name in [
        "not-json.txt",
        "trailing-garbage.txt",
        "top-level-array.json",
        "wrong-schema-tag.json",
        "missing-schema-tag.json",
        "missing-pages-checked.json",
        "missing-findings.json",
        "findings-not-array.json",
        "unknown-kind.json",
        "finding-without-page.json",
        "unknown-status.json",
    ] {
        let (p, out) = fixture_run(name, 0);
        assert_undetermined_after_spawn(name, &p, &out);
    }
}

#[test]
fn zero_or_partial_pages_checked_is_undetermined() {
    for name in ["zero-checked.json", "partial-checked.json"] {
        let (p, out) = fixture_run(name, 0);
        assert_undetermined_after_spawn(name, &p, &out);
    }
}

#[test]
fn valid_json_with_nonzero_exit_is_undetermined() {
    for (name, exit) in [
        ("clean.json", 1),
        ("clean.json", 2),
        ("finding-console-error.json", 1),
    ] {
        let (p, out) = fixture_run(name, exit);
        assert_undetermined_after_spawn(&format!("{name} exit {exit}"), &p, &out);
    }
}
