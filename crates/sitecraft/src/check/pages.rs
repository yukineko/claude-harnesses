//! `sitecraft check pages` — open every configured page in Playwright (via
//! the bundled `assets/check-pages.mjs`, run by `$SITECRAFT_NODE` / `node`)
//! and block on console errors, page errors, same-origin asset 4xx/5xx and
//! same-origin broken links.
//!
//! Both the subprocess exit status and its stdout JSON decide the verdict;
//! neither alone can yield `Clean`. Zero pages, missing browser, missing
//! Playwright, missing node, unresolvable script, invalid/empty JSON, a
//! status other than `ok`, `pages_checked` different from the configured
//! page count, a timeout, or a non-zero exit with valid JSON →
//! `Undetermined`. The invocation and output contract is the doc comment at
//! the top of `tests/check_pages.rs`.
//!
//! The script is located as `$CLAUDE_PLUGIN_ROOT/assets/check-pages.mjs`, or
//! relative to the running executable (`<ancestor>/assets/check-pages.mjs`
//! for an installed plugin, `<ancestor>/crates/sitecraft/assets/check-pages.mjs`
//! for a workspace build), checking at most [`EXE_ANCESTOR_DEPTH`] ancestors.
//! None found → `Undetermined`.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::{Config, Loaded};
use crate::Verdict;

/// The bundled script's file name.
pub const SCRIPT_NAME: &str = "check-pages.mjs";
/// Output schema tag the script must emit.
pub const SCHEMA: &str = "sitecraft-check-pages/1";
/// How many ancestors of the executable's directory are searched for the script.
pub const EXE_ANCESTOR_DEPTH: usize = 4;

/// The four finding kinds the script may report.
const KINDS: [&str; 4] = [
    "console-error",
    "page-error",
    "asset-http-error",
    "broken-link",
];

const FIX_PLAYWRIGHT: &str =
    "install it in the project root: `npm i -D playwright && npx playwright install chromium`";
const FIX_BROWSER: &str =
    "install the browser in the project root: `npx playwright install chromium`";

/// Check pages of `root` against a running server.
///
/// `base_url` is the server origin; `None` means `http://127.0.0.1:<port>`
/// from the config. Contract: config not loadable (`Absent` or
/// `Undetermined`) → `Undetermined`; otherwise [`check_pages`] with node
/// running in `root`.
pub fn run(root: &Path, base_url: Option<&str>) -> Verdict {
    match crate::config::load(root) {
        Loaded::Absent => Verdict::undetermined(format!(
            "no sitecraft.toml in {}; create one (command, port, pages, build_dir, web_globs) \
             or pass --root <project directory>",
            root.display()
        )),
        Loaded::Undetermined(why) => Verdict::undetermined(why),
        Loaded::Config(cfg, _) => {
            let base = match base_url {
                Some(b) => b.to_string(),
                None => format!("http://127.0.0.1:{}", cfg.port),
            };
            check_pages_in(&cfg, &base, Some(root))
        }
    }
}

/// Check `config.pages` at `base_url`.
///
/// Contract: `Clean` iff the script exited 0, its JSON parsed against the
/// schema, it checked exactly as many pages as are configured (>= 1), and
/// reported zero findings; `Violation` (naming page URL and kind) iff it ran
/// correctly and reported findings; `Undetermined` in every other case (see
/// module docs). Empty `pages` → `Undetermined` without spawning node.
/// node runs in the current working directory.
pub fn check_pages(config: &Config, base_url: &str) -> Verdict {
    check_pages_in(config, base_url, None)
}

fn check_pages_in(config: &Config, base_url: &str, cwd: Option<&Path>) -> Verdict {
    if config.pages.is_empty() {
        return Verdict::undetermined(
            "sitecraft.toml lists no `pages`, so nothing can be checked; add at least one path, \
             e.g. pages = [\"/\"]",
        );
    }
    let script = match locate_script() {
        Ok(s) => s,
        Err(why) => return Verdict::undetermined(why),
    };
    let input = serde_json::json!({ "base_url": base_url, "pages": config.pages });
    let node = crate::program_from_env(crate::ENV_NODE, "node");
    let mut cmd = Command::new(&node);
    cmd.arg(&script)
        .arg(input.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group, so a timeout kills node and the browser it launched.
        .process_group(0);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = match run_with_deadline(cmd, deadline_for(config.pages.len())) {
        Ok(o) => o,
        Err(e) => {
            return Verdict::undetermined(format!(
                "cannot run node ({}): {e}; install Node.js or set {} to the node executable",
                node.to_string_lossy(),
                crate::ENV_NODE
            ))
        }
    };
    judge(&out, config.pages.len())
}

/// Rust-side ceiling: the script's own global timeout plus a margin.
fn deadline_for(pages: usize) -> Duration {
    let secs = 30u64.saturating_add(45u64.saturating_mul(pages as u64));
    Duration::from_secs(secs.min(480) + 30)
}

/// Candidate script paths, in priority order.
fn script_candidates() -> Vec<PathBuf> {
    let mut c = Vec::new();
    if let Some(root) = std::env::var_os("CLAUDE_PLUGIN_ROOT").filter(|r| !r.is_empty()) {
        c.push(PathBuf::from(root).join("assets").join(SCRIPT_NAME));
    }
    if let Ok(exe) = std::env::current_exe() {
        let exe = exe.canonicalize().unwrap_or(exe);
        for dir in exe.ancestors().skip(1).take(EXE_ANCESTOR_DEPTH) {
            c.push(dir.join("assets").join(SCRIPT_NAME));
            c.push(
                dir.join("crates")
                    .join("sitecraft")
                    .join("assets")
                    .join(SCRIPT_NAME),
            );
        }
    }
    c
}

fn locate_script() -> Result<PathBuf, String> {
    let candidates = script_candidates();
    for p in &candidates {
        if p.is_file() {
            return p
                .canonicalize()
                .map_err(|e| format!("cannot resolve {}: {e}", p.display()));
        }
    }
    let tried = candidates
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "cannot locate the bundled {SCRIPT_NAME} (tried: {}); set CLAUDE_PLUGIN_ROOT to the \
         sitecraft plugin directory or reinstall the plugin",
        if tried.is_empty() { "nothing" } else { &tried }
    ))
}

struct Output {
    /// `None` when the deadline passed and the process group was killed.
    status: Option<ExitStatus>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_with_deadline(mut cmd: Command, limit: Duration) -> std::io::Result<Output> {
    let mut child = cmd.spawn()?;
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let r = so.as_mut().map(|s| s.read_to_end(&mut b));
        (b, r.map(|r| r.map(|_| ())))
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let r = se.as_mut().map(|s| s.read_to_end(&mut b));
        (b, r.map(|r| r.map(|_| ())))
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(s) => break Some(s),
            None if start.elapsed() >= limit => {
                kill_group(&mut child);
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let join =
        |t: std::thread::JoinHandle<(Vec<u8>, Option<std::io::Result<()>>)>, name: &str| match t
            .join()
        {
            Ok((b, Some(Ok(())))) => Ok(b),
            Ok((_, Some(Err(e)))) => Err(std::io::Error::other(format!("reading {name}: {e}"))),
            Ok((_, None)) => Err(std::io::Error::other(format!("{name} was not captured"))),
            Err(_) => Err(std::io::Error::other(format!("{name} reader panicked"))),
        };
    let stdout = join(t_out, "stdout")?;
    let stderr = join(t_err, "stderr")?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn kill_group(child: &mut std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: plain syscall on the child's own process group (it was
        // spawned with process_group(0), so pgid == pid).
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// One validated finding.
struct Finding {
    page: String,
    kind: String,
    detail: String,
}

/// Validated `ok` output.
struct Report {
    pages_checked: u64,
    findings: Vec<Finding>,
}

/// Map the subprocess result to a verdict (pure; see module docs).
fn judge(out: &Output, expected_pages: usize) -> Verdict {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stderr_tail = tail(&stderr);
    let Some(status) = out.status else {
        return Verdict::undetermined(format!(
            "check-pages.mjs did not finish within the deadline and was killed; is the dev \
             server running and responsive? (start it with `sitecraft serve start`){stderr_tail}"
        ));
    };
    let parsed = parse_stdout(&out.stdout);

    // A missing browser / Playwright is named whatever else went wrong.
    let script_status = parsed
        .as_ref()
        .ok()
        .and_then(|v| v.get("status"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let script_error = parsed
        .as_ref()
        .ok()
        .and_then(|v| v.get("error"))
        .and_then(Value::as_str)
        .map(|e| format!(" ({e})"))
        .unwrap_or_default();
    if script_status.as_deref() == Some("browser-missing")
        || stderr.contains("Executable doesn't exist")
    {
        return Verdict::undetermined(format!(
            "the Playwright browser (chromium) is missing{script_error}; {FIX_BROWSER}{stderr_tail}"
        ));
    }
    if script_status.as_deref() == Some("playwright-missing")
        || stderr.contains("Cannot find module")
    {
        return Verdict::undetermined(format!(
            "playwright is not installed in the project{script_error}; {FIX_PLAYWRIGHT}{stderr_tail}"
        ));
    }

    if !status.success() {
        let code = status
            .code()
            .map_or_else(|| "a signal".to_string(), |c| format!("exit {c}"));
        return Verdict::undetermined(format!(
            "check-pages.mjs ended with {code} (status {}){script_error}; findings are only \
             trusted from a successful run — fix the error and rerun{stderr_tail}",
            script_status.as_deref().unwrap_or("unknown")
        ));
    }

    let value = match parsed {
        Ok(v) => v,
        Err(why) => {
            return Verdict::undetermined(format!(
                "check-pages.mjs exited 0 but its stdout is unusable: {why}{stderr_tail}"
            ))
        }
    };
    let report = match validate(&value) {
        Ok(r) => r,
        Err(why) => {
            return Verdict::undetermined(format!(
                "check-pages.mjs output does not match {SCHEMA}: {why}; reinstall the sitecraft \
                 plugin so the script and binary match"
            ))
        }
    };
    if report.pages_checked != expected_pages as u64 {
        return Verdict::undetermined(format!(
            "check-pages.mjs checked {} of {expected_pages} configured pages; every page must be \
             checked before the result can be trusted",
            report.pages_checked
        ));
    }
    if report.findings.is_empty() {
        return Verdict::from_findings(Vec::new());
    }
    let lines = report
        .findings
        .iter()
        .map(|f| format!("  {} [{}] {}", f.page, f.kind, f.detail))
        .collect::<Vec<_>>()
        .join("\n");
    Verdict::violation(format!(
        "{} broken-page finding(s) on {expected_pages} checked page(s); fix each and rerun \
         `sitecraft check pages`:\n{lines}",
        report.findings.len()
    ))
}

/// Exactly one JSON object (surrounding whitespace allowed).
fn parse_stdout(stdout: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(stdout).map_err(|e| format!("not UTF-8: {e}"))?;
    if text.trim().is_empty() {
        return Err("empty".to_string());
    }
    let v: Value =
        serde_json::from_str(text.trim()).map_err(|e| format!("not one JSON value: {e}"))?;
    if !v.is_object() {
        return Err("top level is not a JSON object".to_string());
    }
    Ok(v)
}

fn validate(v: &Value) -> Result<Report, String> {
    let schema = v.get("schema").and_then(Value::as_str);
    if schema != Some(SCHEMA) {
        return Err(format!("schema is {schema:?}, expected {SCHEMA:?}"));
    }
    match v.get("status").and_then(Value::as_str) {
        Some("ok") => {}
        other => return Err(format!("status is {other:?}, expected \"ok\"")),
    }
    let pages_checked = v
        .get("pages_checked")
        .and_then(Value::as_u64)
        .ok_or("pages_checked is missing or not a non-negative integer")?;
    let raw = v
        .get("findings")
        .and_then(Value::as_array)
        .ok_or("findings is missing or not an array")?;
    let mut findings = Vec::with_capacity(raw.len());
    for (i, f) in raw.iter().enumerate() {
        let obj = f
            .as_object()
            .ok_or_else(|| format!("findings[{i}] is not an object"))?;
        let page = obj
            .get("page")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| format!("findings[{i}].page is missing or empty"))?;
        let kind = obj
            .get("kind")
            .and_then(Value::as_str)
            .filter(|k| KINDS.contains(k))
            .ok_or_else(|| format!("findings[{i}].kind is missing or unknown"))?;
        let detail = obj
            .get("detail")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("findings[{i}].detail is missing or not a string"))?;
        findings.push(Finding {
            page: page.to_string(),
            kind: kind.to_string(),
            detail: detail.to_string(),
        });
    }
    Ok(Report {
        pages_checked,
        findings,
    })
}

/// Last few stderr lines, as a suffix for a reason (empty when none).
fn tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return String::new();
    }
    let start = lines.len().saturating_sub(5);
    format!("\nnode stderr:\n  {}", lines[start..].join("\n  "))
}
