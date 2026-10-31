//! Round-4 black-box tests for `sitecraft check security` (exit 0 clean /
//! 1 violation / 2 undetermined), written by an independent test author from
//! defects a verifier observed after round 3, under these binding user rulings:
//!
//! (a) The `.env` leak check EXCLUDES keys prefixed `VITE_`, `NEXT_PUBLIC_`,
//!     `PUBLIC_` and the key `NODE_ENV`: those are public by framework
//!     convention and are bundled on purpose.
//! (b) Sink detection must catch common code forms, including Prettier-style
//!     multi-line calls. Deliberately obfuscated forms are out of scope (no
//!     test here requires them).
//!
//! N1. `.env` parsing must match dotenv semantics: inline `# comment` after an
//!     unquoted or quoted value, lone-CR line endings, `KEY: value` form. A
//!     `#` inside a quoted value is part of the value.
//! N2. every `.env.*` at the project root is read (`.env.production.local`,
//!     `.env.development`, `.env.staging`, ...), except obvious templates
//!     (`.env.example` / `.env.sample` / `.env.template`).
//! N3. multi-line DOM sink calls / assignments with a non-literal argument.
//! N4. the source walk is bounded: a chain of directories each holding k
//!     symlinks to the next must not explode into k^depth paths.
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
// harness (mirrors tests/check_security_round3.rs)
// ---------------------------------------------------------------------------

const CONFIG: &str = r#"command = ["npm", "run", "dev"]
port = 5173
pages = ["/"]
build_dir = "dist"
web_globs = ["src/**", "index.html"]
"#;

const FAST_LIMIT: Duration = Duration::from_secs(60);
const WALK_LIMIT: Duration = Duration::from_secs(20);

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
        self.run_within(FAST_LIMIT)
    }

    fn run_within(&self, limit: Duration) -> Outcome {
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
            if start.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                panic!("sitecraft check security did not finish within {limit:?}");
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

// ---------------------------------------------------------------------------
// baseline
// ---------------------------------------------------------------------------

#[test]
fn round4_clean_project_is_clean() {
    Project::clean().run().expect(0, "clean project");
}

// ---------------------------------------------------------------------------
// N1. dotenv-compatible parsing
// ---------------------------------------------------------------------------

fn secret() -> String {
    ["Sup3rS3cret", "Value_9fj2kd"].concat()
}

fn bundle_with(value: &str) -> String {
    format!("(function(){{var c={{db:\"{value}\"}};window.cfg=c;}})();\n")
}

/// `env_text` is written to `env_file`, the build contains `in_build`; the
/// check must exit 1, name the build file, and never print `in_build` in full.
fn assert_env_leak(label: &str, env_file: &str, env_text: &str, in_build: &str) {
    let p = Project::clean();
    p.write(env_file, env_text);
    p.write("dist/assets/app.js", &bundle_with(in_build));
    let out = p.run();
    out.expect(
        1,
        &format!("{label}: dotenv-parsed value of {env_file} appears in dist/assets/app.js"),
    );
    assert!(
        !out.text.contains(in_build),
        "{label}: the full .env value must not be printed; output:\n{}",
        out.text
    );
    assert!(
        out.text.contains("app.js"),
        "{label}: violation output must name the build file; output:\n{}",
        out.text
    );
}

/// Control (passes already in round 3): the plain form is detected.
#[test]
fn env_plain_form_control_is_violation() {
    let s = secret();
    assert_env_leak("plain DB=value", ".env", &format!("DB={s}\n"), &s);
}

/// dotenv: an unquoted value ends before ` #`; the value is `<secret>`.
#[test]
fn env_inline_comment_after_unquoted_value_is_stripped() {
    let s = secret();
    assert_env_leak(
        "DB=<secret> # prod pw",
        ".env",
        &format!("DB={s} # prod pw\n"),
        &s,
    );
}

/// dotenv: a quoted value followed by an inline comment; the value is the
/// quoted content.
#[test]
fn env_inline_comment_after_quoted_value_is_stripped() {
    let s = secret();
    assert_env_leak(
        "DB=\"<secret>\" # prod pw",
        ".env",
        &format!("DB=\"{s}\" # prod pw\n"),
        &s,
    );
}

/// dotenv splits lines on `\r\n`, `\n` and lone `\r`.
#[test]
fn env_lone_cr_line_endings_are_line_breaks() {
    let s = secret();
    assert_env_leak(
        "A=x<CR>DB=<secret><CR>",
        ".env",
        &format!("A=x\rDB={s}\r"),
        &s,
    );
}

/// dotenv accepts `KEY: value` as well as `KEY=value`.
#[test]
fn env_colon_form_is_parsed() {
    let s = secret();
    assert_env_leak("DB: <secret>", ".env", &format!("DB: {s}\n"), &s);
}

/// Control: inside quotes `#` is part of the value. The value is
/// `<secret>#tail`, and that full value in the build is a violation.
#[test]
fn env_hash_inside_quotes_is_part_of_value_control() {
    let full = format!("{}#tail_xyz", secret());
    assert_env_leak(
        "DB=\"<secret>#tail\"",
        ".env",
        &format!("DB=\"{full}\"\n"),
        &full,
    );
}

/// Control: since `#tail` belongs to the quoted value, only the prefix in the
/// build is not the value and is not a finding.
#[test]
fn env_hash_inside_quotes_prefix_only_in_build_is_not_a_finding() {
    let s = secret();
    let p = Project::clean();
    p.write(".env", &format!("DB=\"{s}#tail_xyz\"\n"));
    p.write("dist/assets/app.js", &bundle_with(&s));
    p.run().expect(
        0,
        "DB=\"<secret>#tail_xyz\": the value includes #tail_xyz, only the prefix is bundled",
    );
}

// ---------------------------------------------------------------------------
// N2. every .env.* at the project root, except templates
// ---------------------------------------------------------------------------

#[test]
fn env_production_local_is_read() {
    let s = ["Pr0dL0cal", "Value_q81zxa"].concat();
    assert_env_leak(
        ".env.production.local",
        ".env.production.local",
        &format!("DB={s}\n"),
        &s,
    );
}

#[test]
fn env_development_is_read() {
    let s = ["D3vT0ken", "Value_m29vbn"].concat();
    assert_env_leak(
        ".env.development",
        ".env.development",
        &format!("DB={s}\n"),
        &s,
    );
}

#[test]
fn env_staging_is_read() {
    let s = ["St4g1ngT0ken", "Value_p03lkj"].concat();
    assert_env_leak(".env.staging", ".env.staging", &format!("DB={s}\n"), &s);
}

/// Control: template files hold placeholders that are committed on purpose;
/// a value from them appearing in the build is not a leak.
#[test]
fn env_template_files_are_not_read() {
    for name in [".env.example", ".env.sample", ".env.template"] {
        let v = ["Placeh0lder", "Value_template1"].concat();
        let p = Project::clean();
        p.write(name, &format!("DB={v}\n"));
        p.write("dist/assets/app.js", &bundle_with(&v));
        p.run().expect(
            0,
            &format!("{name} is a template; its value in the build is not a leak"),
        );
    }
}

// ---------------------------------------------------------------------------
// ruling (a): public-by-convention keys are excluded
// ---------------------------------------------------------------------------

const PUBLIC_ENV: &str = "NODE_ENV=production\nVITE_API_URL=https://api.example.com\nNEXT_PUBLIC_X=pk_live_publicKeyValue123\nPUBLIC_Y=public-site-identifier-42\n";

fn public_bundle() -> String {
    "(function(){var e=\"production\",u=\"https://api.example.com\",x=\"pk_live_publicKeyValue123\",y=\"public-site-identifier-42\";window.c=[e,u,x,y];})();\n".to_string()
}

/// Ruling (a): NODE_ENV / VITE_ / NEXT_PUBLIC_ / PUBLIC_ values are bundled on
/// purpose and must not be findings.
#[test]
fn public_prefixed_env_values_in_build_are_not_findings() {
    let p = Project::clean();
    p.write(".env.production", PUBLIC_ENV);
    p.write("dist/assets/app.js", &public_bundle());
    p.run().expect(
        0,
        "ruling (a): NODE_ENV, VITE_*, NEXT_PUBLIC_*, PUBLIC_* values in the bundle",
    );
}

/// Each public key on its own, to localise which exclusion is missing.
#[test]
fn each_public_prefixed_env_value_is_not_a_finding() {
    let cases = [
        ("NODE_ENV", "production"),
        ("VITE_API_URL", "https://api.example.com"),
        ("NEXT_PUBLIC_X", "pk_live_publicKeyValue123"),
        ("PUBLIC_Y", "public-site-identifier-42"),
    ];
    let mut failures = Vec::new();
    for (k, v) in cases {
        let p = Project::clean();
        p.write(".env.production", &format!("{k}={v}\n"));
        p.write("dist/assets/app.js", &bundle_with(v));
        let out = p.run();
        if out.code != Some(0) {
            failures.push(format!(
                "{k}: expected exit 0, got {:?}; output:\n{}",
                out.code, out.text
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "ruling (a): public-by-convention keys were flagged:\n{}",
        failures.join("\n---\n")
    );
}

/// Control: the exclusion is by key, not by file. A non-public key alongside
/// the public ones is still a leak.
#[test]
fn non_public_env_value_alongside_public_ones_is_violation() {
    let secret = ["ApiS3cret", "Value_long_8hd72k"].concat();
    let p = Project::clean();
    p.write(
        ".env.production",
        &format!("{PUBLIC_ENV}API_SECRET={secret}\n"),
    );
    p.write(
        "dist/assets/app.js",
        &format!("{}var s=\"{secret}\";\n", public_bundle()),
    );
    let out = p.run();
    out.expect(
        1,
        "API_SECRET (not public) appears in the bundle next to public keys",
    );
    assert!(
        !out.text.contains(&secret),
        "the full API_SECRET value must not be printed; output:\n{}",
        out.text
    );
}

// ---------------------------------------------------------------------------
// N3. multi-line sinks (ruling (b))
// ---------------------------------------------------------------------------

fn assert_sink_body_detected(label: &str, body: &str) {
    let p = Project::clean();
    p.write(
        "src/render.js",
        &format!("export function render(el, userInput) {{\n{body}}}\n"),
    );
    let out = p.run();
    out.expect(1, label);
    assert!(
        out.text.contains("render.js"),
        "{label}: violation output must name the file; output:\n{}",
        out.text
    );
}

fn assert_sink_body_clean(label: &str, body: &str) {
    let p = Project::clean();
    p.write(
        "src/render.js",
        &format!("export function render(el) {{\n{body}}}\n"),
    );
    p.run().expect(0, label);
}

#[test]
fn multiline_insert_adjacent_html_is_violation() {
    assert_sink_body_detected(
        "Prettier-style el.insertAdjacentHTML(\\n 'beforeend',\\n userInput\\n)",
        "  el.insertAdjacentHTML(\n    'beforeend',\n    userInput\n  );\n",
    );
}

#[test]
fn insert_adjacent_html_with_call_position_is_violation() {
    assert_sink_body_detected(
        "el.insertAdjacentHTML(pos(), userInput)",
        "  el.insertAdjacentHTML(pos(), userInput);\n",
    );
}

#[test]
fn multiline_object_assign_inner_html_is_violation() {
    assert_sink_body_detected(
        "Object.assign(el, {\\n innerHTML: userInput,\\n})",
        "  Object.assign(el, {\n    innerHTML: userInput,\n  });\n",
    );
}

#[test]
fn multiline_inner_html_assignment_is_violation() {
    assert_sink_body_detected(
        "el.innerHTML =\\n userInput",
        "  el.innerHTML =\n    userInput;\n",
    );
}

#[test]
fn multiline_document_write_is_violation() {
    assert_sink_body_detected(
        "document.write(\\n userInput\\n)",
        "  document.write(\n    userInput\n  );\n",
    );
}

/// Controls: the same multi-line shapes with string literal arguments.
#[test]
fn multiline_insert_adjacent_html_literal_is_not_a_finding() {
    assert_sink_body_clean(
        "multi-line insertAdjacentHTML with '<hr>'",
        "  el.insertAdjacentHTML(\n    'beforeend',\n    '<hr>'\n  );\n",
    );
}

#[test]
fn multiline_object_assign_literal_is_not_a_finding() {
    assert_sink_body_clean(
        "multi-line Object.assign innerHTML: '<b>x</b>'",
        "  Object.assign(el, {\n    innerHTML: '<b>x</b>',\n  });\n",
    );
}

#[test]
fn multiline_inner_html_literal_is_not_a_finding() {
    assert_sink_body_clean(
        "el.innerHTML =\\n '<b>x</b>'",
        "  el.innerHTML =\n    '<b>x</b>';\n",
    );
}

#[test]
fn multiline_document_write_literal_is_not_a_finding() {
    assert_sink_body_clean(
        "document.write(\\n '<p>x</p>'\\n)",
        "  document.write(\n    '<p>x</p>'\n  );\n",
    );
}

// ---------------------------------------------------------------------------
// N4. bounded walk
// ---------------------------------------------------------------------------

/// `src/d0 .. src/d{depth}`, each `d{i}` holding `k` symlinks `l{j} -> ../d{i+1}`.
/// Naively following links visits k^depth paths.
#[cfg(unix)]
fn symlink_chain(p: &Project, depth: usize, k: usize, bottom: &str) {
    for i in 0..depth {
        fs::create_dir_all(p.root.join(format!("src/d{i}"))).unwrap();
        for j in 0..k {
            symlink(
                Path::new(&format!("../d{}", i + 1)),
                &p.root.join(format!("src/d{i}/l{j}")),
            );
        }
    }
    p.write(&format!("src/d{depth}/x.js"), bottom);
}

/// depth 20, k = 2: ~1M paths if every link is followed. Must finish within
/// 20 s with exit 1 (the sink at the bottom was scanned) or exit 2 (the walk
/// gave up with an explicit undetermined). A timeout or exit 0 fails.
#[cfg(unix)]
#[test]
fn symlink_chain_walk_is_bounded() {
    let p = Project::clean();
    symlink_chain(&p, 20, 2, "el.innerHTML = userInput;\n");
    let out = p.run_within(WALK_LIMIT);
    assert!(
        matches!(out.code, Some(1) | Some(2)),
        "depth-20 k=2 symlink chain with a sink at the bottom: expected exit 1 (found) or 2 (explicit undetermined), got {:?}; output:\n{}",
        out.code,
        out.text
    );
    if out.code == Some(2) {
        let lower = out.text.to_lowercase();
        assert!(
            lower.contains("too many") || lower.contains("limit"),
            "exit 2 must explain that the walk gave up (too many paths); output:\n{}",
            out.text
        );
    }
}

/// Control: a small chain is fully walked and the sink found.
#[cfg(unix)]
#[test]
fn small_symlink_chain_finds_sink_control() {
    let p = Project::clean();
    symlink_chain(&p, 3, 2, "el.innerHTML = userInput;\n");
    p.run_within(WALK_LIMIT)
        .expect(1, "depth-3 k=2 chain with a sink at the bottom");
}
