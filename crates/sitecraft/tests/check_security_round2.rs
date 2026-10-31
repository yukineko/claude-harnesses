//! Round-2 black-box tests for `sitecraft check security` (exit 0 clean /
//! 1 violation / 2 undetermined), written by an independent test author from
//! defects a verifier observed in the first implementation:
//!
//! 1. a private-key block inside a `.map` `sourcesContent` string whose
//!    newline is double-escaped (bytes: backslash, backslash, `n`);
//! 2. directory symlinks silently skipped by both walks (build-output secret
//!    scan and source sink scan), plus a loop-termination guard;
//! 3. GitHub `ghu_` / `ghr_` tokens;
//! 4. bracket-notation and logical-assignment `innerHTML` sinks;
//! 5. `npm audit` whose grandchild keeps stdout open.
//!
//! Secret-shaped values are assembled by concatenation from a deterministic
//! generator so no secret-shaped literal lands in a tracked file.
//!
//! Every process spawned here runs under a wall-clock bound: a hang must be a
//! failing test, never a stuck test run.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// harness (mirrors tests/check_security.rs)
// ---------------------------------------------------------------------------

const CONFIG: &str = r#"command = ["npm", "run", "dev"]
port = 5173
pages = ["/"]
build_dir = "dist"
web_globs = ["src/**", "index.html"]
"#;

/// Bound for runs that are expected to be fast (no audit timeout involved).
const FAST_LIMIT: Duration = Duration::from_secs(60);

fn gen(alphabet: &str, n: usize, seed: u64) -> String {
    let chars: Vec<char> = alphabet.chars().collect();
    let mut s = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..n)
        .map(|_| {
            s = s
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            chars[((s >> 33) as usize) % chars.len()]
        })
        .collect()
}

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const B64: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn gh_token(prefix: &str, seed: u64) -> String {
    ["gh", prefix, "_"].concat() + &gen(ALNUM, 36, seed)
}
fn gh_classic() -> String {
    gh_token("p", 12)
}
fn pem_begin() -> String {
    ["-----BEGIN ", "RSA PRIV", "ATE KEY-----"].concat()
}
fn pem_end() -> String {
    ["-----END ", "RSA PRIV", "ATE KEY-----"].concat()
}
fn pem_body_lines() -> Vec<String> {
    (0..4).map(|i| gen(B64, 64, 200 + i)).collect()
}

struct Project {
    _dir: TempDir,
    root: PathBuf,
    _tools: TempDir,
    tools: PathBuf,
    state: PathBuf,
    /// A directory outside `root`, reachable only through symlinks.
    _outside: TempDir,
    outside: PathBuf,
}

impl Project {
    fn clean() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let tools = tempfile::tempdir().unwrap();
        let tools_path = tools.path().to_path_buf();
        let state = tools_path.join("state");
        fs::create_dir_all(&state).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().to_path_buf();
        let p = Project {
            _dir: dir,
            root,
            _tools: tools,
            tools: tools_path,
            state,
            _outside: outside,
            outside: outside_path,
        };
        p.write("sitecraft.toml", CONFIG);
        p.write(
            "package.json",
            r#"{"name":"demo","version":"1.0.0","private":true}"#,
        );
        p.write(
            "package-lock.json",
            r#"{"name":"demo","version":"1.0.0","lockfileVersion":3,"packages":{}}"#,
        );
        p.write(
            "index.html",
            "<!doctype html><html><body><div id=\"app\"></div><script type=\"module\" src=\"/src/main.js\"></script></body></html>\n",
        );
        p.write(
            "src/main.js",
            "const app = document.getElementById('app');\napp.textContent = 'hello';\n",
        );
        p.write(
            "dist/index.html",
            "<!doctype html><html><body><div id=\"app\"></div><script src=\"/assets/app.js\"></script></body></html>\n",
        );
        p.write(
            "dist/assets/app.js",
            "(function(){var a=document.getElementById('app');a.textContent='hello';})();\n",
        );
        p.write(
            "dist/assets/app.js.map",
            r#"{"version":3,"sources":["../src/main.js"],"names":[],"mappings":"AAAA"}"#,
        );
        p
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_outside(&self, rel: &str, body: &str) {
        let path = self.outside.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn script(&self, name: &str, body: &str) -> PathBuf {
        let script = self.tools.join(name);
        fs::write(&script, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        script
    }

    /// `npm audit --json` stub printing `stdout` and exiting `code`; any other
    /// argv fails loudly (exit 97).
    fn npm_stub(&self, name: &str, stdout: &str, code: i32) -> PathBuf {
        let payload = self.tools.join(format!("{name}.out"));
        fs::write(&payload, stdout).unwrap();
        self.script(
            name,
            &format!(
                "#!/bin/sh\n{}cat '{}'\nexit {code}\n",
                ARGV_GUARD,
                payload.display()
            ),
        )
    }

    fn clean_npm(&self) -> PathBuf {
        self.npm_stub("npm-clean", &audit_json(0, 0, 0, 0, 0), 0)
    }

    /// Run `sitecraft check security` with a wall-clock `limit`. Output goes
    /// to files (not pipes) so a leaked grandchild holding an inherited
    /// descriptor cannot stall this harness. `None` = still running at the
    /// deadline (the process is killed).
    fn run_bounded(&self, npm: &Path, limit: Duration) -> Option<Outcome> {
        let out_path = self.tools.join("sitecraft.stdout");
        let err_path = self.tools.join("sitecraft.stderr");
        let mut child = Command::new(env!("CARGO_BIN_EXE_sitecraft"))
            .arg("--root")
            .arg(&self.root)
            .args(["check", "security"])
            .current_dir(&self.root)
            .env(sitecraft::ENV_NPM, npm)
            .env(sitecraft::ENV_STATE_DIR, &self.state)
            .env_remove(sitecraft::ENV_NODE)
            .env_remove(sitecraft::ENV_MARKETPLACE)
            .stdin(Stdio::null())
            .stdout(Stdio::from(fs::File::create(&out_path).unwrap()))
            .stderr(Stdio::from(fs::File::create(&err_path).unwrap()))
            .spawn()
            .expect("spawn sitecraft");
        let start = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if start.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Some(Outcome {
            code: status.code(),
            elapsed: start.elapsed(),
            text: format!(
                "{}{}",
                fs::read_to_string(&out_path).unwrap_or_default(),
                fs::read_to_string(&err_path).unwrap_or_default()
            ),
        })
    }

    fn run(&self) -> Outcome {
        let npm = self.clean_npm();
        self.run_bounded(&npm, FAST_LIMIT).unwrap_or_else(|| {
            panic!("sitecraft check security did not finish within {FAST_LIMIT:?}")
        })
    }
}

const ARGV_GUARD: &str = "case \" $* \" in *\" audit \"*) ;; *) echo \"npm stub: unexpected args: $*\" >&2; exit 97;; esac\n\
case \" $* \" in *\" --json \"*) ;; *) echo \"npm stub: --json missing: $*\" >&2; exit 97;; esac\n";

struct Outcome {
    code: Option<i32>,
    elapsed: Duration,
    text: String,
}

impl Outcome {
    fn expect(&self, code: i32, what: &str) {
        assert_eq!(
            self.code,
            Some(code),
            "{what}: expected exit {code}, got {:?}; output:\n{}",
            self.code,
            self.text
        );
    }

    /// Violation or undetermined, but never clean.
    fn expect_not_clean(&self, what: &str) {
        assert!(
            matches!(self.code, Some(1 | 2)),
            "{what}: expected exit 1 (violation) or 2 (undetermined), got {:?}; output:\n{}",
            self.code,
            self.text
        );
    }
}

fn audit_json(low: u64, moderate: u64, high: u64, critical: u64, info: u64) -> String {
    let total = low + moderate + high + critical + info;
    format!(
        r#"{{"auditReportVersion":2,"vulnerabilities":{{}},"metadata":{{"vulnerabilities":{{"info":{info},"low":{low},"moderate":{moderate},"high":{high},"critical":{critical},"total":{total}}},"dependencies":{{"prod":1,"dev":0,"optional":0,"peer":0,"peerOptional":0,"total":1}}}}}}"#
    )
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// Kill a process recorded in a pid file (best-effort test cleanup).
fn kill_pidfile(pidfile: &Path) {
    if let Ok(s) = fs::read_to_string(pidfile) {
        let pid = s.trim();
        if !pid.is_empty() {
            let _ = Command::new("kill").args(["-9", pid]).status();
        }
    }
}

// ---------------------------------------------------------------------------
// baseline
// ---------------------------------------------------------------------------

#[test]
fn round2_clean_project_is_clean() {
    Project::clean().run().expect(0, "clean project");
}

// ---------------------------------------------------------------------------
// 1. private key in .map sourcesContent with escaped newlines
// ---------------------------------------------------------------------------

/// Plant a PEM block in `dist/assets/app.js.map` `sourcesContent` with each
/// real newline replaced by `sep` (raw file bytes).
fn pem_in_map(sep: &str) -> (Project, Vec<String>) {
    let body = pem_body_lines();
    let block = format!("{}\n{}\n{}", pem_begin(), body.join("\n"), pem_end()).replace('\n', sep);
    let p = Project::clean();
    p.write(
        "dist/assets/app.js.map",
        &format!(
            r#"{{"version":3,"sources":["../src/key.js"],"sourcesContent":["const k = \"{block}\";\n"],"names":[],"mappings":"AAAA"}}"#
        ),
    );
    (p, body)
}

fn assert_pem_in_map_detected(sep: &str, label: &str) {
    let (p, body) = pem_in_map(sep);
    let out = p.run();
    out.expect(1, label);
    assert!(
        out.text.contains("app.js.map"),
        "{label}: violation output must name the file; output:\n{}",
        out.text
    );
    for line in &body {
        assert!(
            !out.text.contains(line.as_str()),
            "{label}: key body must be redacted; output:\n{}",
            out.text
        );
    }
}

#[test]
fn private_key_in_map_with_single_escaped_newlines_is_violation() {
    // Bytes between header and body: backslash, n.
    assert_pem_in_map_detected("\\n", "PEM in .map, newline written as backslash-n");
}

#[test]
fn private_key_in_map_with_double_escaped_newlines_is_violation() {
    // Bytes between header and body: backslash, backslash, n (a source that
    // itself contained an escaped newline, re-escaped by the map's JSON).
    assert_pem_in_map_detected(
        "\\\\n",
        "PEM in .map, newline written as backslash-backslash-n",
    );
}

// ---------------------------------------------------------------------------
// 2. directory symlinks
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn secret_behind_directory_symlink_in_build_dir_is_not_clean() {
    let p = Project::clean();
    p.write_outside(
        "chunks/vendor.js",
        &format!("(function(){{var k=\"{}\";}})();\n", gh_classic()),
    );
    symlink(&p.outside.join("chunks"), &p.root.join("dist/chunks"));
    p.run().expect_not_clean(
        "build_dir/chunks is a directory symlink whose target holds a GitHub token; skipping it silently reports a clean scan of files never read",
    );
}

#[cfg(unix)]
#[test]
fn sink_behind_directory_symlink_in_sources_is_not_clean() {
    let p = Project::clean();
    p.write_outside(
        "shared/render.js",
        "export function render(el, userInput) {\n  el.innerHTML = userInput;\n}\n",
    );
    symlink(&p.outside.join("shared"), &p.root.join("src/shared"));
    p.run().expect_not_clean(
        "src/shared is a directory symlink whose target holds el.innerHTML = userInput; skipping it silently reports a clean scan of files never read",
    );
}

#[cfg(unix)]
#[test]
fn directory_symlink_loops_terminate() {
    let p = Project::clean();
    // Each link points at an ancestor of itself.
    symlink(Path::new(".."), &p.root.join("dist/assets/loop"));
    symlink(Path::new(".."), &p.root.join("src/loop"));
    let npm = p.clean_npm();
    let out = p
        .run_bounded(&npm, Duration::from_secs(20))
        .unwrap_or_else(|| panic!("check security did not terminate within 20s on a symlink loop"));
    assert!(
        matches!(out.code, Some(0 | 2)),
        "symlink loop with no findings: expected exit 0 (loop-safe follow) or 2 (undetermined), got {:?}; output:\n{}",
        out.code,
        out.text
    );
}

// ---------------------------------------------------------------------------
// 3. GitHub ghu_ / ghr_ tokens
// ---------------------------------------------------------------------------

fn assert_build_secret_detected(label: &str, secret: &str) {
    for (file, body) in [
        (
            "dist/assets/app.js",
            format!("(function(){{var k=\"{secret}\";window.cfg={{k:k}};}})();\n"),
        ),
        (
            "dist/assets/app.js.map",
            format!(
                r#"{{"version":3,"sources":["../src/main.js"],"sourcesContent":["export const k = '{secret}';\n"],"names":[],"mappings":"AAAA"}}"#
            ),
        ),
    ] {
        let p = Project::clean();
        p.write(file, &body);
        let out = p.run();
        out.expect(1, &format!("{label} in {file}"));
        assert!(
            !out.text.contains(secret),
            "{label} in {file}: the matched secret must be redacted; output:\n{}",
            out.text
        );
    }
}

#[test]
fn github_user_to_server_token_ghu_in_build_is_violation() {
    assert_build_secret_detected("GitHub ghu_ token", &gh_token("u", 31));
}

#[test]
fn github_refresh_token_ghr_in_build_is_violation() {
    assert_build_secret_detected("GitHub ghr_ token", &gh_token("r", 32));
}

// ---------------------------------------------------------------------------
// 4. innerHTML sink variants
// ---------------------------------------------------------------------------

fn assert_sink_detected(label: &str, body: &str) {
    let p = Project::clean();
    p.write("src/render.js", body);
    let out = p.run();
    out.expect(1, label);
    assert!(
        out.text.contains("render.js"),
        "{label}: violation output must name the file; output:\n{}",
        out.text
    );
}

#[test]
fn inner_html_bracket_single_quote_assignment_is_violation() {
    assert_sink_detected(
        "el['innerHTML'] = x",
        "export function render(el, x) {\n  el['innerHTML'] = x;\n}\n",
    );
}

#[test]
fn inner_html_bracket_double_quote_assignment_is_violation() {
    assert_sink_detected(
        "el[\"innerHTML\"] = x",
        "export function render(el, x) {\n  el[\"innerHTML\"] = x;\n}\n",
    );
}

#[test]
fn inner_html_logical_or_assignment_is_violation() {
    assert_sink_detected(
        "el.innerHTML ||= x",
        "export function render(el, x) {\n  el.innerHTML ||= x;\n}\n",
    );
}

#[test]
fn inner_html_plus_assignment_is_violation() {
    assert_sink_detected(
        "el.innerHTML += x",
        "export function render(el, x) {\n  el.innerHTML += x;\n}\n",
    );
}

/// Negative control mirroring `inner_html_constant_string_assignment_is_not_a_finding`
/// in check_security.rs: a constant string literal RHS is not a finding, in
/// bracket notation too.
#[test]
fn inner_html_bracket_constant_string_assignment_is_not_a_finding() {
    let p = Project::clean();
    p.write(
        "src/render.js",
        "export function reset(el) {\n  el['innerHTML'] = \"<b>static</b>\";\n}\n",
    );
    p.run()
        .expect(0, "el['innerHTML'] assigned a constant string literal");
}

// ---------------------------------------------------------------------------
// 5. npm audit: grandchild holding stdout open
// ---------------------------------------------------------------------------

/// npm prints a well-formed clean audit, leaves a background grandchild
/// holding the inherited stdout pipe, and exits 0. The pipe never reaches EOF
/// while the grandchild lives, so the output is not known to be complete:
/// the check must not block on it, and must return undetermined (exit 2)
/// promptly.
#[cfg(unix)]
#[test]
fn audit_grandchild_holding_stdout_after_npm_exits_is_undetermined_promptly() {
    let p = Project::clean();
    p.run()
        .expect(0, "control: same project with a clean audit");
    let payload = p.tools.join("npm-gc.out");
    fs::write(&payload, audit_json(0, 0, 0, 0, 0)).unwrap();
    let pidfile = p.tools.join("grandchild.pid");
    let npm = p.script(
        "npm-gc",
        &format!(
            "#!/bin/sh\n{ARGV_GUARD}cat '{}'\nsleep 600 &\necho $! > '{}'\nexit 0\n",
            payload.display(),
            pidfile.display()
        ),
    );
    let res = p.run_bounded(&npm, Duration::from_secs(30));
    kill_pidfile(&pidfile);
    let out = res.unwrap_or_else(|| {
        panic!("check security blocked for 30s+ on npm stdout held open by a grandchild after npm exited")
    });
    out.expect(
        2,
        "npm exited but a grandchild still holds its stdout: output not known complete",
    );
    assert!(
        out.elapsed < Duration::from_secs(30),
        "returned only after {:?}",
        out.elapsed
    );
}

// Not covered here: npm itself hanging past the audit timeout. The timeout is
// a hard-coded 300 s constant (`AUDIT_TIMEOUT`, not configurable by env or
// config), so such a test would cost over 5 minutes per run.
