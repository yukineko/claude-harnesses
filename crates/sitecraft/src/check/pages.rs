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
//!
//! # Where node runs (Playwright resolution)
//!
//! node runs in the web-app package directory, so `playwright` is resolved
//! from its `node_modules` (and, by node's normal lookup, every ancestor's —
//! the project root included). That directory is `<root>/<dir>` when the
//! configured `command` is `npm … --prefix <dir> …` / `--prefix=<dir>` (before
//! any `--`), else the project root. A prefix that does not resolve to an
//! existing directory inside the root → `Undetermined`.
//!
//! # `api_paths`
//!
//! After the pages, each `api_paths` entry of `sitecraft.toml` gets one plain
//! `GET <path>` (HTTP/1.1, `Connection: close`, redirects not followed) on the
//! server's loopback address: `127.0.0.1:<port>`, or the `[::1]`/`127.0.0.1`
//! address of `--base-url` when that names a loopback `http://` origin (the
//! gate passes the address `serve start` found ready). A 2xx status line is
//! clean; any other status (a 3xx included), a refused / reset connection, or
//! an answer without an HTTP status line is a `Violation`; no status line
//! within [`API_PATH_TIMEOUT`] per path is `Undetermined` (reason `timeout`).
//! A violation is reported with every undetermined reason appended, so a
//! timeout is never hidden behind it.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
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

/// Per-path limit of one `api_paths` GET (connect, send, status line).
pub const API_PATH_TIMEOUT: Duration = Duration::from_secs(10);

const FIX_PLAYWRIGHT: &str = "install it in the web-app package (the `npm --prefix` directory \
     of `command`, else the project root): `npm i -D playwright && npx playwright install chromium`";
const FIX_BROWSER: &str = "install the browser from the web-app package (the `npm --prefix` \
     directory of `command`, else the project root): `npx playwright install chromium`";

/// Check pages (and `api_paths`) of `root` against a running server.
///
/// `base_url` is the server origin; `None` means `http://127.0.0.1:<port>`
/// from the config. Contract: config not loadable (`Absent` or
/// `Undetermined`), or a `--prefix` package directory that does not resolve
/// inside the root → `Undetermined` without spawning; otherwise
/// [`check_pages`] with node running in the package directory (module docs),
/// combined with the `api_paths` GETs (module docs): any violation →
/// `Violation`, else any undetermined → `Undetermined`, else `Clean`.
pub fn run(root: &Path, base_url: Option<&str>) -> Verdict {
    match crate::config::load(root) {
        Loaded::Absent => Verdict::undetermined(format!(
            "no sitecraft.toml in {}; create one (command, port, pages, build_dir, web_globs) \
             or pass --root <project directory>",
            root.display()
        )),
        Loaded::Undetermined(why) => Verdict::undetermined(why),
        Loaded::Config(cfg, bytes) => {
            let api_paths = match crate::config::api_paths(&bytes) {
                Ok(a) => a,
                Err(why) => {
                    return Verdict::undetermined(format!(
                        "{} is unusable: {why}",
                        crate::config::path_for(root).display()
                    ))
                }
            };
            let node_dir = match package_dir(&cfg.command, root) {
                Ok(d) => d,
                Err(why) => return Verdict::undetermined(why),
            };
            let base = match base_url {
                Some(b) => b.to_string(),
                None => format!("http://127.0.0.1:{}", cfg.port),
            };
            let pages = check_pages_in(&cfg, &base, Some(&node_dir));
            if api_paths.is_empty() {
                return pages;
            }
            let api = check_api_paths(api_target(base_url, cfg.port), &api_paths);
            combine(vec![pages, api])
        }
    }
}

/// [`Verdict::worst_of`], except that a resulting `Violation` carries every
/// undetermined reason too (a timeout must stay visible next to a finding).
fn combine(verdicts: Vec<Verdict>) -> Verdict {
    let undetermined: Vec<String> = verdicts
        .iter()
        .filter_map(|v| match v {
            Verdict::Undetermined(u) => Some(u.as_str().to_string()),
            _ => None,
        })
        .collect();
    match Verdict::worst_of(verdicts) {
        Verdict::Violation(r) if !undetermined.is_empty() => Verdict::violation(format!(
            "{}\nadditionally undetermined (fix these too):\n  {}",
            r.as_str(),
            undetermined.join("\n  ")
        )),
        other => other,
    }
}

/// The `npm --prefix <dir>` / `--prefix=<dir>` of `command` (program named
/// `npm`, options before any `--`), or `None`.
fn npm_prefix(command: &[String]) -> Option<&str> {
    let (prog, args) = command.split_first()?;
    if Path::new(prog).file_name().and_then(|n| n.to_str()) != Some("npm") {
        return None;
    }
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            return None;
        }
        if a == "--prefix" {
            return it.next().map(String::as_str);
        }
        if let Some(v) = a.strip_prefix("--prefix=") {
            return Some(v);
        }
    }
    None
}

/// The directory node runs in (and `check security` runs `npm audit` in, U8):
/// the `npm --prefix` package dir (canonical,
/// inside the canonical root) or `root` itself. A prefix that is empty,
/// cannot be resolved, is not a directory, or lies outside the root → `Err`.
pub(crate) fn package_dir(command: &[String], root: &Path) -> Result<PathBuf, String> {
    let Some(prefix) = npm_prefix(command) else {
        return Ok(root.to_path_buf());
    };
    let fix = "fix `--prefix` in `command` of sitecraft.toml (then `sitecraft trust`) so it \
               names the web-app package directory inside the project root";
    if prefix.is_empty() {
        return Err(format!("`npm --prefix` in `command` is empty; {fix}"));
    }
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve the project root {}: {e}", root.display()))?;
    let dir = root.join(prefix);
    let canon = dir.canonicalize().map_err(|e| {
        format!(
            "cannot resolve the web-app package directory {} named by `npm --prefix {prefix}`: \
             {e}; {fix}",
            dir.display()
        )
    })?;
    if !canon.starts_with(&canon_root) {
        return Err(format!(
            "`npm --prefix {prefix}` resolves to {}, outside the project root {}; {fix}",
            canon.display(),
            canon_root.display()
        ));
    }
    if !canon.is_dir() {
        return Err(format!(
            "`npm --prefix {prefix}` names {}, which is not a directory; {fix}",
            canon.display()
        ));
    }
    Ok(canon)
}

/// Where the `api_paths` GETs go: the loopback address of `base_url` when it
/// is `http://<loopback ip>:<port>` (optionally with a trailing `/`), else
/// `127.0.0.1:<port>`.
fn api_target(base_url: Option<&str>, port: u16) -> SocketAddr {
    let parsed = base_url
        .and_then(|b| b.strip_prefix("http://"))
        .map(|rest| rest.strip_suffix('/').unwrap_or(rest))
        .and_then(|hostport| hostport.parse::<SocketAddr>().ok())
        .filter(|a| a.ip().is_loopback());
    parsed.unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
}

/// Outcome of one `api_paths` GET.
enum ApiOutcome {
    /// A 2xx status line.
    Ok,
    /// Ran to a conclusion that is not 2xx (status, refused, reset, garbage).
    Bad(String),
    /// No conclusion: the time limit passed, or the socket could not be set up.
    Undetermined(String),
}

/// GET every path at `addr` (sequentially, [`API_PATH_TIMEOUT`] each).
fn check_api_paths(addr: SocketAddr, paths: &[String]) -> Verdict {
    let mut bad = Vec::new();
    let mut undetermined = Vec::new();
    for p in paths {
        match get_status(addr, p, API_PATH_TIMEOUT) {
            ApiOutcome::Ok => {}
            ApiOutcome::Bad(why) => bad.push(format!("  GET http://{addr}{p}: {why}")),
            ApiOutcome::Undetermined(why) => {
                undetermined.push(format!("  GET http://{addr}{p}: {why}"))
            }
        }
    }
    let n = paths.len();
    if !bad.is_empty() {
        let mut msg = format!(
            "{} of {n} api_paths did not answer 2xx (redirects are not followed); fix the API \
             and rerun `sitecraft check pages`:\n{}",
            bad.len(),
            bad.join("\n")
        );
        if !undetermined.is_empty() {
            msg.push_str(&format!(
                "\nadditionally undetermined:\n{}",
                undetermined.join("\n")
            ));
        }
        return Verdict::violation(msg);
    }
    if !undetermined.is_empty() {
        return Verdict::undetermined(format!(
            "{} of {n} api_paths could not be checked; is the dev server running and \
             responsive? (start it with `sitecraft serve start`):\n{}",
            undetermined.len(),
            undetermined.join("\n")
        ));
    }
    Verdict::from_findings(Vec::new())
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

/// One `GET <path>` at `addr` within `limit` (see module docs).
fn get_status(addr: SocketAddr, path: &str, limit: Duration) -> ApiOutcome {
    let timeout = || {
        crate::history::note_timeout();
        ApiOutcome::Undetermined(format!(
            "timeout: no HTTP status line within {}s",
            limit.as_secs()
        ))
    };
    let deadline = Instant::now() + limit;
    let remaining = || deadline.saturating_duration_since(Instant::now());
    let mut s = match TcpStream::connect_timeout(&addr, limit) {
        Ok(s) => s,
        Err(e) if is_timeout(&e) => return timeout(),
        Err(e) => return ApiOutcome::Bad(format!("connection failed ({e})")),
    };
    let left = remaining();
    if left.is_zero() {
        return timeout();
    }
    if let Err(e) = s.set_write_timeout(Some(left)) {
        return ApiOutcome::Undetermined(format!("cannot set the socket write timeout: {e}"));
    }
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: */*\r\nUser-Agent: sitecraft\r\n\
         Connection: close\r\n\r\n"
    );
    match s.write_all(req.as_bytes()) {
        Ok(()) => {}
        Err(e) if is_timeout(&e) => return timeout(),
        Err(e) => return ApiOutcome::Bad(format!("connection failed while sending ({e})")),
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let left = remaining();
        if left.is_zero() {
            return timeout();
        }
        if let Err(e) = s.set_read_timeout(Some(left)) {
            return ApiOutcome::Undetermined(format!("cannot set the socket read timeout: {e}"));
        }
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'\n') || buf.len() >= 4096 {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if is_timeout(&e) => return timeout(),
            Err(e) => return ApiOutcome::Bad(format!("connection failed while reading ({e})")),
        }
    }
    if buf.is_empty() {
        return ApiOutcome::Bad("the connection closed without an HTTP answer".to_string());
    }
    match parse_status_code(&buf) {
        Some(code) if (200..300).contains(&code) => ApiOutcome::Ok,
        Some(code) => ApiOutcome::Bad(format!("answered HTTP {code}")),
        None => ApiOutcome::Bad(format!(
            "answered without an HTTP status line ({:?})",
            String::from_utf8_lossy(&buf[..buf.len().min(80)])
        )),
    }
}

/// `HTTP/1.x NNN …` → `NNN` (100..=599).
fn parse_status_code(b: &[u8]) -> Option<u16> {
    let line = String::from_utf8_lossy(b);
    let line = line.lines().next()?;
    let mut parts = line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    let code: u16 = parts.next()?.parse().ok()?;
    (100..=599).contains(&code).then_some(code)
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

/// Rust-side ceiling: the script's own global timeout plus a margin. Also
/// the time limit `sitecraft status` judges a slow `pages` run against.
pub(crate) fn deadline_for(pages: usize) -> Duration {
    let secs = 30u64.saturating_add(45u64.saturating_mul(pages as u64));
    Duration::from_secs(secs.min(480) + 30)
}

/// Candidate paths of the bundled asset `name`, in priority order.
fn script_candidates(name: &str) -> Vec<PathBuf> {
    let mut c = Vec::new();
    if let Some(root) = std::env::var_os("CLAUDE_PLUGIN_ROOT").filter(|r| !r.is_empty()) {
        c.push(PathBuf::from(root).join("assets").join(name));
    }
    if let Ok(exe) = std::env::current_exe() {
        let exe = exe.canonicalize().unwrap_or(exe);
        for dir in exe.ancestors().skip(1).take(EXE_ANCESTOR_DEPTH) {
            c.push(dir.join("assets").join(name));
            c.push(
                dir.join("crates")
                    .join("sitecraft")
                    .join("assets")
                    .join(name),
            );
        }
    }
    c
}

fn locate_script() -> Result<PathBuf, String> {
    locate_asset(SCRIPT_NAME)
}

/// Locate the bundled asset `name` the same way as the check-pages script
/// (module docs). Not found → `Err` naming every path tried.
pub(crate) fn locate_asset(name: &str) -> Result<PathBuf, String> {
    let candidates = script_candidates(name);
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
        "cannot locate the bundled {name} (tried: {}); set CLAUDE_PLUGIN_ROOT to the \
         sitecraft plugin directory or reinstall the plugin",
        if tried.is_empty() { "nothing" } else { &tried }
    ))
}

/// Result of [`run_with_deadline`].
pub(crate) struct Output {
    /// `None` when the deadline passed and the process group was killed.
    pub(crate) status: Option<ExitStatus>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// Spawn `cmd` (stdout / stderr piped by the caller, own process group) and
/// wait at most `limit`; past it the whole group is SIGKILLed, the status is
/// `None` and the expiry is noted for the check history
/// ([`crate::history::note_timeout`]). A spawn or capture failure is `Err`.
pub(crate) fn run_with_deadline(mut cmd: Command, limit: Duration) -> std::io::Result<Output> {
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
                crate::history::note_timeout();
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
            "timeout: check-pages.mjs did not finish within the deadline and was killed; is the dev \
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
