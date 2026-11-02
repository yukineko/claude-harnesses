//! Round-3 black-box tests for `sitecraft check security` (exit 0 clean /
//! 1 violation / 2 undetermined), written by an independent test author from
//! defects a verifier observed after round 2:
//!
//! F1. (regression) a directory symlink aliasing `src` (e.g. `zzz -> src`,
//!     `docs/code -> ../src`). The walk de-duplicates directories by canonical
//!     path, so whichever path is reached first "owns" the directory. If the
//!     alias is reached first its files are recorded under the alias path,
//!     which does not match `web_globs`, and the real `src` is then skipped as
//!     already visited: the sink in `src/x.js` is never scanned.
//! F2. DOM sinks other than `innerHTML`: `outerHTML`, `insertAdjacentHTML`,
//!     `document.write`, `document.writeln`.
//! F3. values from `.env` / `.env.local` / `.env.production` leaking into the
//!     build output.
//! F4. `innerHTML` reached via backtick bracket notation or `Object.assign`.
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
// harness (mirrors tests/check_security_round2.rs)
// ---------------------------------------------------------------------------

const CONFIG: &str = r#"command = ["npm", "run", "dev"]
port = 5173
pages = ["/"]
build_dir = "dist"
web_globs = ["src/**", "index.html"]
"#;

const FAST_LIMIT: Duration = Duration::from_secs(60);

struct Project {
    _dir: TempDir,
    root: PathBuf,
    _tools: TempDir,
    tools: PathBuf,
    state: PathBuf,
}

impl Project {
    fn clean() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let tools = tempfile::tempdir().unwrap();
        let tools_path = tools.path().to_path_buf();
        let state = tools_path.join("state");
        fs::create_dir_all(&state).unwrap();
        let p = Project {
            _dir: dir,
            root,
            _tools: tools,
            tools: tools_path,
            state,
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

    /// `npm audit --json` stub printing a clean audit; any other argv fails
    /// loudly (exit 97). Never the real npm.
    fn clean_npm(&self) -> PathBuf {
        let payload = self.tools.join("npm-clean.out");
        fs::write(&payload, audit_json_clean()).unwrap();
        self.script(
            "npm-clean",
            &format!(
                "#!/bin/sh\n{ARGV_GUARD}cat '{}'\nexit 0\n",
                payload.display()
            ),
        )
    }

    fn run(&self) -> Outcome {
        let npm = self.clean_npm();
        let out_path = self.tools.join("sitecraft.stdout");
        let err_path = self.tools.join("sitecraft.stderr");
        let mut child = Command::new(env!("CARGO_BIN_EXE_sitecraft"))
            .arg("--root")
            .arg(&self.root)
            .args(["check", "security"])
            .current_dir(&self.root)
            .env(sitecraft::ENV_NPM, &npm)
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
            if start.elapsed() > FAST_LIMIT {
                let _ = child.kill();
                let _ = child.wait();
                panic!("sitecraft check security did not finish within {FAST_LIMIT:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Outcome {
            code: status.code(),
            text: format!(
                "{}{}",
                fs::read_to_string(&out_path).unwrap_or_default(),
                fs::read_to_string(&err_path).unwrap_or_default()
            ),
        }
    }
}

const ARGV_GUARD: &str = "case \" $* \" in *\" audit \"*) ;; *) echo \"npm stub: unexpected args: $*\" >&2; exit 97;; esac\n\
case \" $* \" in *\" --json \"*) ;; *) echo \"npm stub: --json missing: $*\" >&2; exit 97;; esac\n";

fn audit_json_clean() -> String {
    r#"{"auditReportVersion":2,"vulnerabilities":{},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":0,"critical":0,"total":0},"dependencies":{"prod":1,"dev":0,"optional":0,"peer":0,"peerOptional":0,"total":1}}}"#.to_string()
}

struct Outcome {
    code: Option<i32>,
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
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, link).unwrap();
}

const SINK_JS: &str = "export function render(el, userInput) {\n  el.innerHTML = userInput;\n}\n";

// ---------------------------------------------------------------------------
// baseline
// ---------------------------------------------------------------------------

#[test]
fn round3_clean_project_is_clean() {
    Project::clean().run().expect(0, "clean project");
}

// ---------------------------------------------------------------------------
// F1. directory symlink aliasing src must not hide src
// ---------------------------------------------------------------------------

/// Control: the sink in `src/x.js` without any alias is a violation.
#[test]
fn sink_in_src_without_alias_is_violation() {
    let p = Project::clean();
    p.write("src/x.js", SINK_JS);
    let out = p.run();
    out.expect(1, "control: src/x.js sink, no alias");
    assert!(
        out.text.contains("x.js"),
        "control: output must name x.js; output:\n{}",
        out.text
    );
}

/// Several alias names are planted at once so the test does not depend on
/// `read_dir` order: whatever order the walk visits root entries in, with
/// this many aliases at least one is very likely to be reached before `src`.
/// The real `src/x.js` must still be scanned (exit 1).
#[cfg(unix)]
#[test]
fn directory_symlinks_aliasing_src_do_not_hide_src_sink() {
    let p = Project::clean();
    p.write("src/x.js", SINK_JS);
    for name in ["a", "aa", "b", "m", "s", "zzz", "0alias", "_src", "Src2"] {
        symlink(Path::new("src"), &p.root.join(name));
    }
    symlink(Path::new("../src"), &p.root.join("docs/code"));
    symlink(Path::new("../../src"), &p.root.join("lib/deep/code"));
    let out = p.run();
    out.expect(
        1,
        "src/x.js has el.innerHTML = userInput and several root-level directory symlinks alias src; the real src must still be scanned",
    );
}

/// Each single-alias shape on its own, to localise which one hides src.
#[cfg(unix)]
#[test]
fn each_single_alias_of_src_does_not_hide_src_sink() {
    let cases: [(&str, &str); 4] = [
        ("zzz", "src"),
        ("a", "src"),
        ("docs/code", "../src"),
        ("0", "src"),
    ];
    let mut failures = Vec::new();
    for (link, target) in cases {
        let p = Project::clean();
        p.write("src/x.js", SINK_JS);
        symlink(Path::new(target), &p.root.join(link));
        let out = p.run();
        if out.code != Some(1) {
            failures.push(format!(
                "alias {link} -> {target}: expected exit 1, got {:?}; output:\n{}",
                out.code, out.text
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "a directory symlink aliasing src hid the real src sink:\n{}",
        failures.join("\n---\n")
    );
}

// ---------------------------------------------------------------------------
// F2. other DOM sinks with non-literal arguments
// ---------------------------------------------------------------------------

fn assert_sink_detected(label: &str, line: &str) {
    let p = Project::clean();
    p.write(
        "src/render.js",
        &format!("export function render(el, userInput) {{\n  {line}\n}}\n"),
    );
    let out = p.run();
    out.expect(1, label);
    assert!(
        out.text.contains("render.js"),
        "{label}: violation output must name the file; output:\n{}",
        out.text
    );
}

fn assert_sink_clean(label: &str, line: &str) {
    let p = Project::clean();
    p.write(
        "src/render.js",
        &format!("export function render(el) {{\n  {line}\n}}\n"),
    );
    p.run().expect(0, label);
}

#[test]
fn outer_html_assignment_of_variable_is_violation() {
    assert_sink_detected("el.outerHTML = userInput", "el.outerHTML = userInput;");
}

#[test]
fn outer_html_bracket_assignment_of_variable_is_violation() {
    assert_sink_detected(
        "el['outerHTML'] = userInput",
        "el['outerHTML'] = userInput;",
    );
}

#[test]
fn outer_html_logical_or_assignment_is_violation() {
    assert_sink_detected("el.outerHTML ||= userInput", "el.outerHTML ||= userInput;");
}

#[test]
fn insert_adjacent_html_with_variable_is_violation() {
    assert_sink_detected(
        "el.insertAdjacentHTML('beforeend', userInput)",
        "el.insertAdjacentHTML('beforeend', userInput);",
    );
}

#[test]
fn document_write_with_variable_is_violation() {
    assert_sink_detected("document.write(userInput)", "document.write(userInput);");
}

#[test]
fn document_writeln_with_variable_is_violation() {
    assert_sink_detected(
        "document.writeln(userInput)",
        "document.writeln(userInput);",
    );
}

/// Same policy as `inner_html_constant_string_assignment_is_not_a_finding`:
/// a constant string literal argument is not a finding.
#[test]
fn document_write_with_constant_string_is_not_a_finding() {
    assert_sink_clean(
        "document.write(\"<p>hi</p>\")",
        "document.write(\"<p>hi</p>\");",
    );
}

#[test]
fn outer_html_constant_string_assignment_is_not_a_finding() {
    assert_sink_clean(
        "el.outerHTML = \"<b>x</b>\"",
        "el.outerHTML = \"<b>x</b>\";",
    );
}

#[test]
fn insert_adjacent_html_constant_string_is_not_a_finding() {
    assert_sink_clean(
        "el.insertAdjacentHTML('beforeend', '<hr>')",
        "el.insertAdjacentHTML('beforeend', '<hr>');",
    );
}

// ---------------------------------------------------------------------------
// F3. .env values leaking into the build
// ---------------------------------------------------------------------------
//
// Pinned spec (chosen by this test author; the implementation must match):
//
// * The project root files `.env`, `.env.local` and `.env.production` are
//   read as `KEY=VALUE` lines (blank lines and `#` comments ignored; a value
//   wrapped in matching single or double quotes is unquoted).
// * A value is a *candidate* only if it is at least 8 characters long and is
//   not purely boolean (`true`/`false`/`yes`/`no`/`on`/`off`, any case) and
//   not purely numeric. Shorter / boolean / numeric values are ignored, since
//   they appear in bundles by coincidence (`PORT=5173`, `DEBUG=true`).
// * A candidate value appearing verbatim in any build output file is a
//   violation (exit 1). The full value is never printed.
// * An `.env*` file that exists but cannot be read is undetermined (exit 2):
//   the values it holds were never compared.

fn db_password_value() -> String {
    ["Sup3rS3cret", "Value_9fj2kd"].concat()
}

fn bundle_with(value: &str) -> String {
    format!("(function(){{var c={{db:\"{value}\"}};window.cfg=c;}})();\n")
}

fn assert_env_value_leak_detected(env_file: &str, key: &str, value: &str) {
    // Control: same .env, value not in the bundle -> clean.
    let p = Project::clean();
    p.write(env_file, &format!("# local config\n{key}={value}\n"));
    p.run().expect(
        0,
        &format!("control: {env_file} present, value not in the bundle"),
    );

    let p = Project::clean();
    p.write(env_file, &format!("# local config\n{key}={value}\n"));
    p.write("dist/assets/app.js", &bundle_with(value));
    let out = p.run();
    out.expect(
        1,
        &format!("{env_file} value of {key} appears verbatim in dist/assets/app.js"),
    );
    assert!(
        !out.text.contains(value),
        "{env_file}: the full .env value must not be printed; output:\n{}",
        out.text
    );
    assert!(
        out.text.contains("app.js"),
        "{env_file}: violation output must name the build file; output:\n{}",
        out.text
    );
}

#[test]
fn dot_env_value_in_build_is_violation_redacted() {
    assert_env_value_leak_detected(".env", &["DB_PASS", "WORD"].concat(), &db_password_value());
}

#[test]
fn dot_env_local_value_in_build_is_violation_redacted() {
    assert_env_value_leak_detected(
        ".env.local",
        "SESSION_SIGNING_SALT",
        &["Lc4lS4lt", "Value_x81qpz"].concat(),
    );
}

#[test]
fn dot_env_production_value_in_build_is_violation_redacted() {
    assert_env_value_leak_detected(
        ".env.production",
        "INTERNAL_API_TOKEN",
        &["Pr0dT0ken", "Value_k27wmb"].concat(),
    );
}

#[test]
fn dot_env_quoted_value_in_build_is_violation() {
    let value = db_password_value();
    let p = Project::clean();
    p.write(
        ".env",
        &format!("{}=\"{value}\"\n", ["DB_PASS", "WORD"].concat()),
    );
    p.write("dist/assets/app.js", &bundle_with(&value));
    let out = p.run();
    out.expect(1, ".env double-quoted value appears in the bundle");
    assert!(
        !out.text.contains(&value),
        "value printed; output:\n{}",
        out.text
    );
}

/// Short, boolean and numeric values are ignored by the pinned spec even
/// when they appear in the bundle.
#[test]
fn trivial_dot_env_values_in_build_are_not_findings() {
    let p = Project::clean();
    p.write(
        ".env",
        "DEBUG=true\nPORT=5173\nVERBOSE=False\nTIMEOUT_MS=1234567890\nNAME=demo\n",
    );
    p.write(
        "dist/assets/app.js",
        "(function(){var d=true,p=5173,v=false,t=1234567890,n='demo';window.c=[d,p,v,t,n];})();\n",
    );
    p.run()
        .expect(0, "short / boolean / numeric .env values in the bundle");
}

#[cfg(unix)]
#[test]
fn unreadable_dot_env_is_undetermined() {
    use std::os::unix::fs::PermissionsExt;
    let p = Project::clean();
    p.write(
        ".env",
        &format!("{}={}\n", ["DB_PASS", "WORD"].concat(), db_password_value()),
    );
    p.run()
        .expect(0, "control: readable .env, value not in bundle");
    let env = p.root.join(".env");
    fs::set_permissions(&env, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&env).is_ok() {
        fs::set_permissions(&env, fs::Permissions::from_mode(0o644)).unwrap();
        panic!("cannot construct an unreadable .env here (running as root?): the test would observe nothing");
    }
    let out = p.run();
    fs::set_permissions(&env, fs::Permissions::from_mode(0o644)).unwrap();
    out.expect(2, ".env exists but is unreadable (mode 000)");
}

// ---------------------------------------------------------------------------
// F4. innerHTML via backtick bracket / Object.assign
// ---------------------------------------------------------------------------

#[test]
fn inner_html_backtick_bracket_assignment_is_violation() {
    assert_sink_detected(
        "el[`innerHTML`] = userInput",
        "el[`innerHTML`] = userInput;",
    );
}

#[test]
fn inner_html_via_object_assign_is_violation() {
    assert_sink_detected(
        "Object.assign(el, {innerHTML: userInput})",
        "Object.assign(el, {innerHTML: userInput});",
    );
}
