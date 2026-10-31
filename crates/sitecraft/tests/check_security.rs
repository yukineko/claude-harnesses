//! Black-box tests for `sitecraft check security` (exit 0 clean / 1 violation /
//! 2 undetermined), driven through the real binary with `--root` and a stub
//! `npm` named by `SITECRAFT_NPM`.
//!
//! Every fixture is generated in a tempdir at test time. Secret-shaped values
//! are assembled by concatenation from a deterministic generator so that no
//! secret-shaped literal ever lands in a tracked file (the repo's pre-commit
//! secret guard would block it, and rightly so).
//!
//! Undetermined (exit 2) cases are differential: the same project is first run
//! with a well-formed clean `npm audit` stub and must exit 0 (control), then
//! with the broken input and must exit 2. A bare "exit 2" assertion would also
//! be satisfied by a check that never runs at all, so it would prove nothing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

const CONFIG: &str = r#"command = ["npm", "run", "dev"]
port = 5173
pages = ["/"]
build_dir = "dist"
web_globs = ["src/**", "index.html"]
"#;

/// Deterministic pseudo-random string of `n` chars drawn from `alphabet`.
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

const UPPER_DIGITS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const B64: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const GOOGLE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";

fn aws_key() -> String {
    ["AK", "IA"].concat() + &gen(UPPER_DIGITS, 16, 11)
}
fn gh_classic() -> String {
    ["gh", "p_"].concat() + &gen(ALNUM, 36, 12)
}
fn gh_oauth() -> String {
    ["gh", "o_"].concat() + &gen(ALNUM, 36, 13)
}
fn gh_fine_grained() -> String {
    ["git", "hub_", "pat_"].concat() + &gen(ALNUM, 22, 14) + "_" + &gen(ALNUM, 59, 15)
}
fn stripe(mode: &str, seed: u64) -> String {
    ["sk", "_", mode, "_"].concat() + &gen(ALNUM, 24, seed)
}
fn stripe_publishable() -> String {
    ["pk", "_", "live", "_"].concat() + &gen(ALNUM, 24, 18)
}
fn google_key() -> String {
    ["AI", "za"].concat() + &gen(GOOGLE, 35, 19)
}
fn pem_begin() -> String {
    ["-----BEGIN ", "RSA PRIV", "ATE KEY-----"].concat()
}
fn pem_end() -> String {
    ["-----END ", "RSA PRIV", "ATE KEY-----"].concat()
}
/// Body lines of a private-key block (base64, 64 columns).
fn pem_body_lines() -> Vec<String> {
    (0..4).map(|i| gen(B64, 64, 100 + i)).collect()
}

struct Project {
    _dir: TempDir,
    root: PathBuf,
    _tools: TempDir,
    tools: PathBuf,
    state: PathBuf,
}

impl Project {
    /// A project that must be clean: config, benign sources, a non-empty
    /// build output without secrets, a package.json/lockfile.
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

    /// Write an executable `npm` stub that, for `npm audit --json`, prints
    /// `stdout` and exits `code`. Any other invocation fails loudly (exit 97,
    /// empty stdout) so a wrong argv cannot masquerade as a clean audit.
    fn npm_stub(&self, name: &str, stdout: &str, code: i32) -> PathBuf {
        let payload = self.tools.join(format!("{name}.out"));
        fs::write(&payload, stdout).unwrap();
        let script = self.tools.join(name);
        let body = format!(
            "#!/bin/sh\n\
             case \" $* \" in *\" audit \"*) ;; *) echo \"npm stub: unexpected args: $*\" >&2; exit 97;; esac\n\
             case \" $* \" in *\" --json \"*) ;; *) echo \"npm stub: --json missing: $*\" >&2; exit 97;; esac\n\
             cat '{}'\n\
             exit {code}\n",
            payload.display()
        );
        fs::write(&script, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        script
    }

    fn clean_npm(&self) -> PathBuf {
        self.npm_stub("npm-clean", &audit_json(0, 0, 0, 0, 0), 0)
    }

    fn run_with(&self, npm: &Path) -> Outcome {
        let out = Command::new(env!("CARGO_BIN_EXE_sitecraft"))
            .arg("--root")
            .arg(&self.root)
            .args(["check", "security"])
            .current_dir(&self.root)
            .env(sitecraft::ENV_NPM, npm)
            .env(sitecraft::ENV_STATE_DIR, &self.state)
            .env_remove(sitecraft::ENV_NODE)
            .env_remove(sitecraft::ENV_MARKETPLACE)
            .output()
            .expect("spawn sitecraft");
        Outcome {
            code: out.status.code(),
            text: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        }
    }

    fn run(&self) -> Outcome {
        let npm = self.clean_npm();
        self.run_with(&npm)
    }
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

/// `npm audit --json` (v7+) shape with all six `metadata.vulnerabilities` keys.
fn audit_json(low: u64, moderate: u64, high: u64, critical: u64, info: u64) -> String {
    let total = low + moderate + high + critical + info;
    format!(
        r#"{{"auditReportVersion":2,"vulnerabilities":{{}},"metadata":{{"vulnerabilities":{{"info":{info},"low":{low},"moderate":{moderate},"high":{high},"critical":{critical},"total":{total}}},"dependencies":{{"prod":1,"dev":0,"optional":0,"peer":0,"peerOptional":0,"total":1}}}}}}"#
    )
}

/// The control half of every undetermined case: the same project with a
/// well-formed clean audit must be clean. Proves the later exit 2 is caused by
/// the injected defect, not by a check that never runs.
fn control_clean(p: &Project, what: &str) {
    p.run().expect(0, &format!("control for [{what}]"));
}

// ---------------------------------------------------------------------------
// baseline
// ---------------------------------------------------------------------------

#[test]
fn clean_project_is_clean() {
    Project::clean().run().expect(0, "clean project");
}

// ---------------------------------------------------------------------------
// secrets in build output (.js and .map), redacted
// ---------------------------------------------------------------------------

/// Plant `secret` inside a `.js` and (separately) a `.map` in the build dir;
/// each must exit 1, name the file, and never print the secret in full.
fn assert_secret_detected(label: &str, secret: &str, must_not_print: &[String]) {
    let variants: [(&str, String); 2] = [
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
    ];
    for (file, body) in variants {
        let p = Project::clean();
        p.write(file, &body);
        let out = p.run();
        out.expect(1, &format!("{label} in {file}"));
        let base = Path::new(file).file_name().unwrap().to_str().unwrap();
        assert!(
            out.text.contains(base),
            "{label} in {file}: violation output must name the file ({base}); output:\n{}",
            out.text
        );
        assert!(
            !out.text.contains(secret),
            "{label} in {file}: the matched secret must be redacted, but it was printed in full; output:\n{}",
            out.text
        );
        for part in must_not_print {
            assert!(
                !out.text.contains(part.as_str()),
                "{label} in {file}: secret material leaked in output ({part}); output:\n{}",
                out.text
            );
        }
    }
}

#[test]
fn aws_access_key_in_build_is_violation_redacted() {
    assert_secret_detected("AWS access key", &aws_key(), &[]);
}

#[test]
fn github_classic_token_in_build_is_violation_redacted() {
    assert_secret_detected("GitHub ghp_ token", &gh_classic(), &[]);
}

#[test]
fn github_oauth_token_in_build_is_violation_redacted() {
    assert_secret_detected("GitHub gho_ token", &gh_oauth(), &[]);
}

#[test]
fn github_fine_grained_pat_in_build_is_violation_redacted() {
    assert_secret_detected("GitHub fine-grained PAT", &gh_fine_grained(), &[]);
}

#[test]
fn stripe_live_secret_key_in_build_is_violation_redacted() {
    assert_secret_detected("Stripe live secret key", &stripe("live", 16), &[]);
}

#[test]
fn stripe_test_secret_key_in_build_is_violation_redacted() {
    assert_secret_detected("Stripe test secret key", &stripe("test", 17), &[]);
}

#[test]
fn google_api_key_in_build_is_violation_redacted() {
    assert_secret_detected("Google API key", &google_key(), &[]);
}

#[test]
fn private_key_block_with_body_in_build_js_is_violation_redacted() {
    // Real newlines (template literal in a bundle).
    let body = pem_body_lines();
    let block = format!("{}\n{}\n{}", pem_begin(), body.join("\n"), pem_end());
    for file in ["dist/assets/app.js", "dist/assets/app.js.map"] {
        let p = Project::clean();
        let content = if file.ends_with(".map") {
            // JSON-escaped newlines: how a key embedded in sources leaks
            // through `sourcesContent`.
            let escaped = block.replace('\n', "\\n");
            format!(
                r#"{{"version":3,"sources":["../src/key.js"],"sourcesContent":["export const k = `{escaped}`;\n"],"names":[],"mappings":"AAAA"}}"#
            )
        } else {
            format!("var k=`{block}`;\n")
        };
        p.write(file, &content);
        let out = p.run();
        out.expect(1, &format!("private key block in {file}"));
        for line in &body {
            assert!(
                !out.text.contains(line.as_str()),
                "private key in {file}: key body must be redacted; output:\n{}",
                out.text
            );
        }
    }
}

#[test]
fn stripe_publishable_key_is_not_a_finding() {
    let p = Project::clean();
    p.write(
        "dist/assets/app.js",
        &format!("(function(){{var pub=\"{}\";}})();\n", stripe_publishable()),
    );
    p.run()
        .expect(0, "Stripe pk_ (publishable) key is public by design");
}

#[test]
fn private_key_begin_line_without_body_is_not_a_finding() {
    let p = Project::clean();
    // A PEM parser in a bundle mentions the header but carries no key.
    p.write(
        "dist/assets/app.js",
        &format!(
            "function isPem(s){{return s.startsWith(\"{}\");}}\n",
            pem_begin()
        ),
    );
    p.run()
        .expect(0, "BEGIN PRIVATE KEY header without a body is not a key");
}

// ---------------------------------------------------------------------------
// dangerous HTML sinks in sources
// ---------------------------------------------------------------------------

fn assert_sink_detected(label: &str, rel: &str, body: &str) {
    let p = Project::clean();
    p.write(rel, body);
    let out = p.run();
    out.expect(1, label);
    let base = Path::new(rel).file_name().unwrap().to_str().unwrap();
    assert!(
        out.text.contains(base),
        "{label}: violation output must name the file ({base}); output:\n{}",
        out.text
    );
}

#[test]
fn inner_html_assignment_of_variable_is_violation() {
    assert_sink_detected(
        "innerHTML = <variable>",
        "src/render.js",
        "export function render(el, html) {\n  el.innerHTML = html;\n}\n",
    );
}

#[test]
fn dangerously_set_inner_html_is_violation() {
    assert_sink_detected(
        "dangerouslySetInnerHTML",
        "src/App.jsx",
        "export default function App({ html }) {\n  return <div dangerouslySetInnerHTML={{ __html: html }} />;\n}\n",
    );
}

#[test]
fn v_html_is_violation() {
    assert_sink_detected(
        "v-html",
        "src/App.vue",
        "<template>\n  <div v-html=\"raw\"></div>\n</template>\n<script>\nexport default { props: ['raw'] };\n</script>\n",
    );
}

#[test]
fn svelte_at_html_is_violation() {
    assert_sink_detected(
        "{@html}",
        "src/App.svelte",
        "<script>\n  export let raw;\n</script>\n<div>{@html raw}</div>\n",
    );
}

#[test]
fn eval_is_violation() {
    assert_sink_detected(
        "eval",
        "src/run.js",
        "export function run(code) {\n  return eval(code);\n}\n",
    );
}

#[test]
fn new_function_is_violation() {
    assert_sink_detected(
        "new Function",
        "src/run.js",
        "export function compile(src) {\n  return new Function('x', src);\n}\n",
    );
}

#[test]
fn inner_html_constant_string_assignment_is_not_a_finding() {
    let p = Project::clean();
    p.write(
        "src/render.js",
        "export function reset(el) {\n  el.innerHTML = \"<p>loading</p>\";\n  el.innerHTML = '';\n}\n",
    );
    p.run()
        .expect(0, "innerHTML assigned a constant string literal");
}

#[test]
fn inner_html_read_is_not_a_finding() {
    let p = Project::clean();
    p.write(
        "src/read.js",
        "export function snapshot(el) {\n  const before = el.innerHTML;\n  console.log(el.innerHTML);\n  return before;\n}\n",
    );
    p.run().expect(0, "reading innerHTML is not a sink");
}

#[test]
fn node_modules_and_dist_are_not_scanned_for_sinks() {
    let p = Project::clean();
    let sinks = "module.exports=function(e,h){e.innerHTML=h;return eval(h)||new Function(h);};\n";
    p.write("node_modules/lib/index.js", sinks);
    p.write("src/node_modules/lib/index.js", sinks);
    // Bundled output routinely contains sink code; it is not a source.
    p.write("dist/assets/vendor.js", sinks);
    p.run()
        .expect(0, "sinks under node_modules/ and dist/ are excluded");
}

// ---------------------------------------------------------------------------
// npm audit (judged from the JSON shape, not the exit code)
// ---------------------------------------------------------------------------

#[test]
fn audit_with_only_low_and_moderate_is_clean_despite_nonzero_exit() {
    let p = Project::clean();
    // npm audit exits non-zero whenever any vulnerability exists.
    let npm = p.npm_stub("npm-lowmod", &audit_json(2, 3, 0, 0, 1), 1);
    p.run_with(&npm)
        .expect(0, "high=0 critical=0 is clean regardless of npm exit code");
}

#[test]
fn audit_with_high_is_violation() {
    let p = Project::clean();
    let npm = p.npm_stub("npm-high", &audit_json(0, 0, 1, 0, 0), 1);
    p.run_with(&npm).expect(1, "high=1");
}

#[test]
fn audit_with_critical_is_violation() {
    let p = Project::clean();
    let npm = p.npm_stub("npm-critical", &audit_json(0, 0, 0, 1, 0), 1);
    p.run_with(&npm).expect(1, "critical=1");
}

#[test]
fn audit_missing_any_of_six_keys_is_undetermined() {
    for key in ["info", "low", "moderate", "high", "critical", "total"] {
        let p = Project::clean();
        control_clean(&p, &format!("missing {key}"));
        let mut v: serde_json::Value = serde_json::from_str(&audit_json(0, 0, 0, 0, 0)).unwrap();
        v["metadata"]["vulnerabilities"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        let npm = p.npm_stub(&format!("npm-no-{key}"), &v.to_string(), 0);
        p.run_with(&npm).expect(
            2,
            &format!("metadata.vulnerabilities without `{key}` cannot be judged"),
        );
    }
}

#[test]
fn audit_without_metadata_is_undetermined() {
    let p = Project::clean();
    control_clean(&p, "no metadata");
    let npm = p.npm_stub(
        "npm-nometa",
        r#"{"auditReportVersion":2,"vulnerabilities":{}}"#,
        0,
    );
    p.run_with(&npm)
        .expect(2, "audit JSON without metadata.vulnerabilities");
}

#[test]
fn audit_error_object_enolock_is_undetermined() {
    let p = Project::clean();
    control_clean(&p, "ENOLOCK");
    let npm = p.npm_stub(
        "npm-enolock",
        r#"{"error":{"code":"ENOLOCK","summary":"This command requires an existing lockfile.","detail":"Try creating one first with: npm i --package-lock-only"}}"#,
        1,
    );
    p.run_with(&npm).expect(2, "npm audit error ENOLOCK");
}

#[test]
fn audit_error_object_other_code_is_undetermined() {
    let p = Project::clean();
    control_clean(&p, "ENOTFOUND");
    let npm = p.npm_stub(
        "npm-enotfound",
        r#"{"error":{"code":"ENOTFOUND","summary":"request to https://registry.npmjs.org/-/npm/v1/security/advisories/bulk failed","detail":""}}"#,
        1,
    );
    p.run_with(&npm).expect(2, "npm audit error ENOTFOUND");
}

#[test]
fn audit_error_object_alongside_zero_metadata_is_undetermined() {
    let p = Project::clean();
    control_clean(&p, "error + zero metadata");
    let mut v: serde_json::Value = serde_json::from_str(&audit_json(0, 0, 0, 0, 0)).unwrap();
    v["error"] = serde_json::json!({"code": "EAUDITNOPJSON", "summary": "No package.json found"});
    let npm = p.npm_stub("npm-err-meta", &v.to_string(), 0);
    p.run_with(&npm)
        .expect(2, "an error object is undetermined even beside zero counts");
}

#[test]
fn audit_empty_stdout_is_undetermined() {
    let p = Project::clean();
    control_clean(&p, "empty stdout");
    let npm = p.npm_stub("npm-empty", "", 0);
    p.run_with(&npm).expect(2, "empty npm audit stdout");
}

#[test]
fn audit_invalid_json_is_undetermined() {
    let p = Project::clean();
    control_clean(&p, "invalid json");
    let npm = p.npm_stub("npm-garbage", "npm WARN something went wrong\n{not json", 0);
    p.run_with(&npm).expect(2, "invalid npm audit JSON");
}

#[test]
fn missing_npm_executable_is_undetermined() {
    let p = Project::clean();
    control_clean(&p, "missing npm");
    let missing = p.tools.join("no-such-npm");
    assert!(!missing.exists());
    p.run_with(&missing).expect(2, "npm executable not found");
}

// ---------------------------------------------------------------------------
// empty scan sets
// ---------------------------------------------------------------------------

#[test]
fn empty_build_dir_is_undetermined() {
    let p = Project::clean();
    control_clean(&p, "empty build dir");
    fs::remove_dir_all(p.root.join("dist")).unwrap();
    fs::create_dir_all(p.root.join("dist")).unwrap();
    p.run().expect(
        2,
        "zero files in build_dir: nothing was scanned for secrets",
    );
}

#[test]
fn missing_build_dir_is_undetermined() {
    let p = Project::clean();
    control_clean(&p, "missing build dir");
    fs::remove_dir_all(p.root.join("dist")).unwrap();
    p.run().expect(2, "build_dir does not exist");
}

#[test]
fn no_source_files_is_undetermined() {
    let p = Project::clean();
    control_clean(&p, "no sources");
    fs::remove_dir_all(p.root.join("src")).unwrap();
    fs::remove_file(p.root.join("index.html")).unwrap();
    p.run()
        .expect(2, "zero source files: nothing was scanned for sinks");
}
