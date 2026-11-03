//! U8 (user ruling): `sitecraft check security` runs `npm audit` in the same
//! package directory `check pages` resolves from the dev command's
//! `--prefix` (`npm_prefix` / `package_dir` in `src/check/pages.rs`):
//!
//! * `npm … --prefix <dir>` / `--prefix=<dir>` (before any `--`) -> `<root>/<dir>`;
//! * no `--prefix` -> the project root;
//! * that directory has no `package-lock.json` -> UNDETERMINED (exit 2);
//! * a `--prefix` that cannot be resolved, or resolves outside the root ->
//!   exit 2 and npm is never run.
//!
//! The fake npm (`$SITECRAFT_NPM`) records its cwd (`pwd -P`) on every call and
//! behaves like the real one about lockfiles: with no `package-lock.json` in
//! its cwd it prints an ENOLOCK error object and exits 1 (an undetermined
//! audit); with one it prints the configured audit JSON. So a test cannot go
//! green by the check merely not running npm, and the cwd record shows where
//! it ran.
//!
//! Expected at HEAD (audit always in the root): (a) and (b) RED, (d) RED,
//! (e) RED, (c) GREEN control.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// `command` is spliced in per test; build_dir / web_globs live under `app/`
/// when the app layout is used.
fn config(command: &str, app_layout: bool) -> String {
    let (build, globs) = if app_layout {
        ("app/dist", r#"["app/src/**", "app/index.html"]"#)
    } else {
        ("dist", r#"["src/**", "index.html"]"#)
    };
    format!(
        "command = {command}\nport = 5173\npages = [\"/\"]\nbuild_dir = \"{build}\"\nweb_globs = {globs}\n"
    )
}

const LOCK: &str = r#"{"name":"demo","version":"1.0.0","lockfileVersion":3,"packages":{}}"#;

fn audit_json(high: u64, critical: u64) -> String {
    let total = high + critical;
    format!(
        r#"{{"auditReportVersion":2,"vulnerabilities":{{}},"metadata":{{"vulnerabilities":{{"info":0,"low":0,"moderate":0,"high":{high},"critical":{critical},"total":{total}}},"dependencies":{{"prod":1,"dev":0,"optional":0,"peer":0,"peerOptional":0,"total":1}}}}}}"#
    )
}

struct Fixture {
    _tmp: TempDir,
    /// `<tmp>/proj`
    root: PathBuf,
    /// `<tmp>/outside` (sibling of the root)
    outside: PathBuf,
    /// `<tmp>/tools`: state dir, fake npm, its payload and cwd log
    tools: PathBuf,
}

impl Fixture {
    /// Root with benign sources + build output laid out under `base`
    /// (`""` for the flat layout, `"app/"` for the app layout). No lockfile
    /// is written anywhere; tests add the ones they need.
    fn new(base: &str, command: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path().canonicalize().unwrap();
        let root = t.join("proj");
        let outside = t.join("outside");
        let tools = t.join("tools");
        for d in [&root, &outside, &tools] {
            fs::create_dir_all(d).unwrap();
        }
        let f = Fixture {
            _tmp: tmp,
            root,
            outside,
            tools,
        };
        f.write("sitecraft.toml", &config(command, base == "app/"));
        f.write(
            &format!("{base}package.json"),
            r#"{"name":"demo","version":"1.0.0","private":true}"#,
        );
        f.write(
            &format!("{base}index.html"),
            "<!doctype html><html><body><div id=\"app\"></div></body></html>\n",
        );
        f.write(
            &format!("{base}src/main.js"),
            "const app = document.getElementById('app');\napp.textContent = 'hello';\n",
        );
        f.write(
            &format!("{base}dist/index.html"),
            "<!doctype html><html><body><div id=\"app\"></div></body></html>\n",
        );
        f.write(
            &format!("{base}dist/assets/app.js"),
            "(function(){var a=document.getElementById('app');a.textContent='hello';})();\n",
        );
        f
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn cwd_log(&self) -> PathBuf {
        self.tools.join("npm-cwd.log")
    }

    /// Fake npm: logs `pwd -P`; `audit --json` with a `package-lock.json` in
    /// the cwd prints `payload` and exits 0/1 like npm; without one it prints
    /// an ENOLOCK error and exits 1. Any other argv exits 97.
    fn npm(&self, payload: &str) -> PathBuf {
        let out = self.tools.join("payload.json");
        fs::write(&out, payload).unwrap();
        let script = self.tools.join("npm");
        let body = format!(
            "#!/bin/sh\n\
             pwd -P >> '{log}'\n\
             case \" $* \" in *\" audit \"*) ;; *) echo \"npm stub: unexpected args: $*\" >&2; exit 97;; esac\n\
             case \" $* \" in *\" --json \"*) ;; *) echo \"npm stub: --json missing: $*\" >&2; exit 97;; esac\n\
             if [ ! -f package-lock.json ]; then\n\
               echo '{{\"error\":{{\"code\":\"ENOLOCK\",\"summary\":\"This command requires an existing lockfile.\"}}}}'\n\
               exit 1\n\
             fi\n\
             cat '{payload}'\n\
             grep -q '\"high\":0,\"critical\":0' '{payload}' && exit 0\n\
             exit 1\n",
            log = self.cwd_log().display(),
            payload = out.display()
        );
        fs::write(&script, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// (exit code, stdout+stderr, directories npm ran in)
    fn run(&self, npm: &Path) -> (Option<i32>, String, Vec<PathBuf>) {
        let out = Command::new(env!("CARGO_BIN_EXE_sitecraft"))
            .arg("--root")
            .arg(&self.root)
            .args(["check", "security"])
            .current_dir(&self.root)
            .env(sitecraft::ENV_NPM, npm)
            .env(sitecraft::ENV_STATE_DIR, self.tools.join("state"))
            .env_remove(sitecraft::ENV_NODE)
            .env_remove(sitecraft::ENV_MARKETPLACE)
            .output()
            .expect("spawn sitecraft");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let cwds = fs::read_to_string(self.cwd_log())
            .map(|s| s.lines().map(PathBuf::from).collect())
            .unwrap_or_default();
        (out.status.code(), text, cwds)
    }
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap()
}

#[test]
fn a_app_layout_audits_app_dir_when_only_app_has_the_lockfile() {
    for command in [
        r#"["npm", "--prefix", "app", "run", "dev"]"#,
        r#"["npm", "--prefix=app", "run", "dev"]"#,
    ] {
        let f = Fixture::new("app/", command);
        f.write("app/package-lock.json", LOCK);
        assert!(!f.root.join("package-lock.json").exists());
        let npm = f.npm(&audit_json(0, 0));
        let (code, text, cwds) = f.run(&npm);
        assert_eq!(
            cwds,
            vec![canon(&f.root.join("app"))],
            "{command}: npm must run in <root>/app only; output:\n{text}"
        );
        assert_eq!(
            code,
            Some(0),
            "{command}: clean audit in app/; output:\n{text}"
        );
    }
}

#[test]
fn b_app_layout_high_vulnerability_in_app_is_a_violation() {
    let f = Fixture::new("app/", r#"["npm", "--prefix", "app", "run", "dev"]"#);
    f.write("app/package-lock.json", LOCK);
    let npm = f.npm(&audit_json(1, 0));
    let (code, text, cwds) = f.run(&npm);
    assert_eq!(
        cwds,
        vec![canon(&f.root.join("app"))],
        "npm must run in <root>/app; output:\n{text}"
    );
    assert_eq!(code, Some(1), "high vulnerability in app/; output:\n{text}");
}

#[test]
fn c_no_prefix_audits_the_root() {
    let f = Fixture::new("", r#"["npm", "run", "dev"]"#);
    f.write("package-lock.json", LOCK);
    // A lockfile in app/ must not matter without --prefix.
    f.write("app/package-lock.json", LOCK);
    let npm = f.npm(&audit_json(0, 0));
    let (code, text, cwds) = f.run(&npm);
    assert_eq!(cwds, vec![canon(&f.root)], "output:\n{text}");
    assert_eq!(code, Some(0), "output:\n{text}");

    // Differential: the same root with a high vulnerability is a violation,
    // so the 0 above came from a real audit of the root.
    let npm = f.npm(&audit_json(1, 0));
    let (code, text, _) = f.run(&npm);
    assert_eq!(code, Some(1), "output:\n{text}");
}

#[test]
fn d_prefix_dir_without_lockfile_is_undetermined_not_the_root() {
    let f = Fixture::new("app/", r#"["npm", "--prefix", "app", "run", "dev"]"#);
    // The root has a lockfile (a root audit would be clean), app/ has none.
    f.write("package-lock.json", LOCK);
    assert!(!f.root.join("app/package-lock.json").exists());
    let npm = f.npm(&audit_json(0, 0));
    let (code, text, cwds) = f.run(&npm);
    assert_eq!(code, Some(2), "no lockfile in app/; output:\n{text}");
    assert!(
        !cwds.contains(&canon(&f.root)),
        "npm must not audit the root when --prefix names app/: ran in {cwds:?}"
    );

    // Control, same root: give app/ its lockfile and the audit is clean, so
    // the exit 2 above is about the missing lockfile in app/.
    f.write("app/package-lock.json", LOCK);
    let (code, text, _) = f.run(&npm);
    assert_eq!(
        code,
        Some(0),
        "control with app/package-lock.json; output:\n{text}"
    );
}

#[test]
fn e_prefix_outside_or_unresolvable_is_undetermined_and_npm_never_runs() {
    for (label, command, make_outside) in [
        (
            "outside by ..",
            r#"["npm", "--prefix", "../outside", "run", "dev"]"#,
            true,
        ),
        (
            "outside by .. (equals form)",
            r#"["npm", "--prefix=../outside", "run", "dev"]"#,
            true,
        ),
        (
            "nonexistent dir",
            r#"["npm", "--prefix", "nope", "run", "dev"]"#,
            false,
        ),
    ] {
        let f = Fixture::new("", command);
        // Lockfiles everywhere an audit could land: a wrongly accepted prefix
        // (or a fallback to the root) would audit cleanly.
        f.write("package-lock.json", LOCK);
        if make_outside {
            fs::write(f.outside.join("package-lock.json"), LOCK).unwrap();
        }
        let npm = f.npm(&audit_json(0, 0));
        let (code, text, cwds) = f.run(&npm);
        assert_eq!(code, Some(2), "{label}; output:\n{text}");
        assert!(
            cwds.is_empty(),
            "{label}: npm must never run, ran in {cwds:?}"
        );
    }
}

#[test]
fn e_symlinked_prefix_escaping_the_root_is_undetermined_and_npm_never_runs() {
    let f = Fixture::new("", r#"["npm", "--prefix", "link", "run", "dev"]"#);
    f.write("package-lock.json", LOCK);
    fs::write(f.outside.join("package-lock.json"), LOCK).unwrap();
    std::os::unix::fs::symlink(&f.outside, f.root.join("link")).unwrap();
    let npm = f.npm(&audit_json(0, 0));
    let (code, text, cwds) = f.run(&npm);
    assert_eq!(code, Some(2), "output:\n{text}");
    assert!(cwds.is_empty(), "npm must never run, ran in {cwds:?}");
}
