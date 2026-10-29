//! CLI / hook contract tests for runbook. Each `claim_<id>` drives the real
//! binary (or reads a packaged repo file) and asserts the specific outcome.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

fn fresh_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("runbook-prose-{}-{tag}-{id}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// Fresh repo (marked as a git root) and a separate fresh HOME.
struct Env {
    repo: PathBuf,
    home: PathBuf,
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Env {
    fn new() -> Env {
        let repo = fresh_dir("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        Env {
            repo,
            home: fresh_dir("home"),
        }
    }

    fn project_dir(&self) -> PathBuf {
        self.repo.join(".runbook")
    }

    fn global_dir(&self) -> PathBuf {
        self.home.join(".runbook").join("runbooks")
    }

    fn write_project(&self, file: &str, content: &str) {
        let d = self.project_dir();
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(file), content).unwrap();
    }

    fn write_global(&self, file: &str, content: &str) {
        let d = self.global_dir();
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(file), content).unwrap();
    }

    fn write_project_config(&self, toml: &str) {
        std::fs::write(self.repo.join("runbook.toml"), toml).unwrap();
    }

    fn write_global_config(&self, toml: &str) {
        let d = self.home.join(".runbook");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.toml"), toml).unwrap();
    }

    fn run(&self, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Out {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_runbook"));
        harness_core::git_env::scrub(&mut cmd);
        cmd.args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("RUNBOOK_DISABLE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let o = child.wait_with_output().unwrap();
        Out {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }

    fn cli(&self, args: &[&str]) -> Out {
        self.run(args, "", &[])
    }

    fn inject_env(&self, prompt: &str, env: &[(&str, &str)]) -> Out {
        let payload = serde_json::json!({
            "cwd": self.repo.to_string_lossy(),
            "session_id": "sess-prose",
            "hook_event_name": "UserPromptSubmit",
            "prompt": prompt,
        })
        .to_string();
        self.run(&["inject"], &payload, env)
    }

    fn inject(&self, prompt: &str) -> Out {
        self.inject_env(prompt, &[])
    }

    fn settings_path(&self) -> PathBuf {
        self.home.join(".claude").join("settings.json")
    }
}

fn status_value(stdout: &str, key: &str) -> String {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix(key))
        .unwrap_or_else(|| panic!("no `{key}` line in status:\n{stdout}"))
        .trim()
        .to_string()
}

fn read_json(p: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn runbook_groups(settings: &serde_json::Value) -> Vec<&serde_json::Value> {
    settings["hooks"]["UserPromptSubmit"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|g| g.to_string().contains("runbook inject"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn claim_inject_expands_macro() {
    let e = Env::new();
    e.write_project(
        "release-plugin.md",
        "# release-plugin\n\n## Overview\nOVERVIEW-TEXT\n\n## Procedure\n1. PROCEDURE-STEP\n\n## Specifications\nSPEC-TEXT\n\n## Forbidden Actions\n- FORBIDDEN-TEXT\n",
    );
    let o = e.inject("follow !release-plugin and cut a new version");
    assert_eq!(o.code, 0, "{}", o.stderr);
    for needle in [
        "## Overview",
        "OVERVIEW-TEXT",
        "## Procedure",
        "PROCEDURE-STEP",
        "## Specifications",
        "SPEC-TEXT",
        "## Forbidden Actions",
        "FORBIDDEN-TEXT",
    ] {
        assert!(o.stdout.contains(needle), "missing {needle}:\n{}", o.stdout);
    }
}

#[test]
fn claim_inject_only_requested_under_budget() {
    // Only what was asked for.
    let e = Env::new();
    e.write_project("a.md", "BODY-OF-A");
    e.write_project("b.md", "BODY-OF-B");
    let o = e.inject("please !a");
    assert!(o.stdout.contains("BODY-OF-A"), "{}", o.stdout);
    assert!(!o.stdout.contains("BODY-OF-B"), "{}", o.stdout);

    // Budget: max_chars=1000; four large runbooks all requested.
    let e = Env::new();
    e.write_project_config("max_chars = 1000\n");
    for n in ["a", "b", "c", "d"] {
        e.write_project(
            &format!("{n}.md"),
            &format!("{}\n", n.to_uppercase().repeat(5000)),
        );
    }
    let o = e.inject("!a !b !c !d");
    let total = o.stdout.trim_end_matches('\n').chars().count();
    let header_end = o.stdout.find("\n\n## ").expect("first block");
    let header = o.stdout[..header_end].chars().count();
    let note_start = o.stdout.find("\n\n…（残り").expect("omission note present");
    let note = o.stdout[note_start..].trim_end().chars().count();
    let blocks = total - header - note;
    eprintln!("MEASURE total={total} header={header} note={note} blocks={blocks}");
    // ACTUAL (claim "hard char budget" is FALSE): the first block is always
    // admitted and counted at heading + per_runbook_chars(=1000 after clamp) +
    // truncation ellipsis = 1017 > max_chars; header and the omission note are
    // not counted at all (stdout is 1182 chars for max_chars=1000).
    assert!(!o.stdout.contains(&"B".repeat(50)), "b leaked");
    assert!(
        o.stdout.contains("残り 3 件"),
        "{}",
        &o.stdout[note_start..]
    );
    assert_eq!(
        blocks, 1017,
        "first block = 9 heading + 1000 body + 8 ellipsis"
    );
    assert!(blocks > 1000);
    assert_eq!(total, 1182);
}

#[test]
fn claim_unreadable_store_reported() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    e.write_project("x.md", "BODY-X");
    let dir = e.project_dir();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = std::fs::read_dir(&dir).is_err();
    let status = e.cli(&["status"]);
    let list = e.cli(&["list"]);
    let show = e.cli(&["show", "x"]);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        unreadable,
        "precondition failed: chmod 000 did not make the store unreadable (running as root?)"
    );
    let line = status_value(&status.stdout, "runbooks visible:");
    assert!(line.starts_with("unknown —"), "{line}");
    assert_eq!(list.code, 1);
    assert!(list.stderr.contains("unknown —"), "{}", list.stderr);
    assert_eq!(show.code, 1);
    assert!(show.stderr.contains("unknown —"), "{}", show.stderr);
}

#[test]
fn claim_inject_metrics_ledger() {
    let e = Env::new();
    e.write_project("a.md", "BODY-A");
    let ledger = e.home.join(".harness/state/inject-metrics.jsonl");
    let o = e.inject("no macro here, just prose");
    assert_eq!(o.code, 0);
    assert_eq!(o.stdout, "");
    assert!(!ledger.exists(), "ledger written for a non-injection");
    let o = e.inject("run !nonexistent");
    assert_eq!(o.stdout, "");
    assert!(!ledger.exists(), "ledger written for unresolved macro");

    let o = e.inject("run !a");
    assert!(o.stdout.contains("BODY-A"));
    let text = std::fs::read_to_string(&ledger).expect("ledger exists after injection");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(v["plugin"], "runbook");
    assert!(v["chars"].as_u64().unwrap() > 0);
}

#[test]
fn claim_store_dirs_project_and_global() {
    let e = Env::new();
    e.write_project("aproj.md", "PROJ-BODY");
    e.write_global("bglob.md", "GLOB-BODY");
    let o = e.cli(&["list"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("[project] !aproj"), "{}", o.stdout);
    assert!(o.stdout.contains("[global ] !bglob"), "{}", o.stdout);
}

#[test]
fn claim_file_stem_is_macro() {
    let e = Env::new();
    e.write_project("deploy.md", "DEPLOY-BODY");
    let o = e.inject("go !deploy");
    assert!(o.stdout.contains("DEPLOY-BODY"), "{}", o.stdout);
    assert!(o.stdout.contains("## 📓 deploy"), "{}", o.stdout);
}

#[test]
fn claim_hooks_json_wires_userpromptsubmit() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let v = read_json(&p);
    let groups = v["hooks"]["UserPromptSubmit"].as_array().expect("event");
    let found = groups.iter().any(|g| {
        g["hooks"].as_array().is_some_and(|hs| {
            hs.iter()
                .any(|h| h["command"] == "${CLAUDE_PLUGIN_ROOT}/bin/runbook inject")
        })
    });
    assert!(found, "{v}");
}

#[test]
fn claim_init_creates_sample() {
    let e = Env::new();
    let o = e.cli(&["init"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(e.project_dir().is_dir());
    let mds: Vec<_> = std::fs::read_dir(e.project_dir())
        .unwrap()
        .flatten()
        .filter(|f| f.path().extension().is_some_and(|x| x == "md"))
        .collect();
    assert!(!mds.is_empty());
}

#[test]
fn claim_list_shows_macros() {
    let e = Env::new();
    e.write_project("alpha.md", "A");
    e.write_project("beta.md", "B");
    let o = e.cli(&["list"]);
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains("!alpha"), "{}", o.stdout);
    assert!(o.stdout.contains("!beta"), "{}", o.stdout);
}

#[test]
fn claim_show_prints_one() {
    let e = Env::new();
    e.write_project("deploy.md", "DEPLOY-BODY");
    e.write_project("other.md", "OTHER-BODY");
    let o = e.cli(&["show", "deploy"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("DEPLOY-BODY"));
    assert!(!o.stdout.contains("OTHER-BODY"));
}

#[test]
fn claim_install_merges_hook() {
    let e = Env::new();
    let o = e.cli(&["install"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v = read_json(&e.settings_path());
    let groups = runbook_groups(&v);
    assert_eq!(groups.len(), 1, "{v}");
    let cmd = groups[0]["hooks"][0]["command"].as_str().unwrap();
    assert!(cmd.ends_with("runbook inject"), "{cmd}");
}

#[test]
fn claim_status_shows_config_dirs_count() {
    let e = Env::new();
    e.write_project_config("max_chars = 5555\n");
    e.write_project("a.md", "A");
    e.write_project("b.md", "B");
    let o = e.cli(&["status"]);
    assert_eq!(o.code, 0);
    assert_eq!(status_value(&o.stdout, "max_chars:"), "5555");
    assert_eq!(
        status_value(&o.stdout, "project dir:"),
        e.project_dir().display().to_string()
    );
    assert_eq!(
        status_value(&o.stdout, "global dir:"),
        e.global_dir().display().to_string()
    );
    assert_eq!(status_value(&o.stdout, "runbooks visible:"), "2");
    assert!(
        status_value(&o.stdout, "config:").ends_with("runbook.toml"),
        "{}",
        o.stdout
    );
}

#[test]
fn claim_new_global_force_normalize() {
    let e = Env::new();
    // --global
    let o = e.cli(&["new", "--global", "x"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(e.global_dir().join("x.md").is_file());
    assert!(!e.project_dir().join("x.md").exists());

    // no --force: second run fails and leaves the file unchanged
    assert_eq!(e.cli(&["new", "y"]).code, 0);
    let y = e.project_dir().join("y.md");
    std::fs::write(&y, "HAND-EDITED").unwrap();
    let o = e.cli(&["new", "y"]);
    assert_ne!(o.code, 0);
    assert_eq!(std::fs::read_to_string(&y).unwrap(), "HAND-EDITED");
    // --force overwrites
    let o = e.cli(&["new", "y", "--force"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_ne!(std::fs::read_to_string(&y).unwrap(), "HAND-EDITED");

    // normalization
    assert_eq!(e.cli(&["new", "My Name!"]).code, 0);
    assert!(e.project_dir().join("my-name.md").is_file());
    let long = "Ab".repeat(30);
    assert_eq!(long.len(), 60);
    assert_eq!(e.cli(&["new", &long]).code, 0);
    let expected: String = "ab".repeat(30).chars().take(48).collect();
    assert!(e.project_dir().join(format!("{expected}.md")).is_file());
    assert_eq!(expected.len(), 48);
}

#[test]
fn claim_install_dryrun_idempotent_backup_foreign() {
    let e = Env::new();
    let sp = e.settings_path();
    std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
    let foreign = r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"/x/playbook inject"}]}]}}"#;
    std::fs::write(&sp, foreign).unwrap();

    // install --dry-run: byte-identical
    assert_eq!(e.cli(&["install", "--dry-run"]).code, 0);
    assert_eq!(std::fs::read_to_string(&sp).unwrap(), foreign);

    // first install backs up the original (backup name is per-second, so check
    // before the second install can overwrite it)
    assert_eq!(e.cli(&["install"]).code, 0);
    let backups: Vec<_> = std::fs::read_dir(sp.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|f| {
            f.file_name()
                .to_string_lossy()
                .contains("settings.json.bak")
        })
        .collect();
    assert!(!backups.is_empty(), "no backup file");
    assert!(backups
        .iter()
        .any(|b| std::fs::read_to_string(b.path()).unwrap() == foreign));

    // install twice: exactly one runbook group, foreign preserved
    assert_eq!(e.cli(&["install"]).code, 0);
    let v = read_json(&sp);
    assert_eq!(runbook_groups(&v).len(), 1, "{v}");
    assert!(v.to_string().contains("playbook inject"), "{v}");

    // uninstall --dry-run: unchanged
    let before = std::fs::read_to_string(&sp).unwrap();
    assert_eq!(e.cli(&["uninstall", "--dry-run"]).code, 0);
    assert_eq!(std::fs::read_to_string(&sp).unwrap(), before);

    // uninstall: runbook gone, foreign kept
    assert_eq!(e.cli(&["uninstall"]).code, 0);
    let v = read_json(&sp);
    assert!(runbook_groups(&v).is_empty(), "{v}");
    assert!(v.to_string().contains("playbook inject"), "{v}");
}

#[test]
fn claim_project_shadows_global() {
    let e = Env::new();
    e.write_project("deploy.md", "BODY-P");
    e.write_global("deploy.md", "BODY-G");
    let o = e.inject("!deploy");
    assert!(o.stdout.contains("BODY-P"), "{}", o.stdout);
    assert!(!o.stdout.contains("BODY-G"), "{}", o.stdout);
}

#[test]
fn claim_case_insensitive_names_aliases() {
    let e = Env::new();
    e.write_project(
        "deploy.md",
        "+++\naliases = [\"ship\"]\n+++\n\nDEPLOY-BODY\n",
    );
    for p in ["!DEPLOY", "!SHIP", "!Deploy", "!sHiP"] {
        let o = e.inject(p);
        assert!(o.stdout.contains("DEPLOY-BODY"), "{p}: {}", o.stdout);
    }
}

#[test]
fn claim_real_index_token_runbook_wins() {
    let e = Env::new();
    e.write_project("runbooks.md", "REAL-RUNBOOKS-BODY");
    e.write_project("deploy.md", "DEPLOY-BODY");
    let o = e.inject("!runbooks");
    assert!(o.stdout.contains("REAL-RUNBOOKS-BODY"), "{}", o.stdout);
    assert!(!o.stdout.contains("利用可能な runbook"), "{}", o.stdout);
    assert!(!o.stdout.contains("DEPLOY-BODY"), "{}", o.stdout);

    // control: without the real runbook the index is what is injected
    let e = Env::new();
    e.write_project("deploy.md", "DEPLOY-BODY");
    let o = e.inject("!runbooks");
    assert!(o.stdout.contains("利用可能な runbook"), "{}", o.stdout);
}

#[test]
fn claim_example_toml_keys() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("runbook.example.toml");
    let t: toml::Table = toml::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    for k in [
        "enabled",
        "project_dir",
        "global_dir",
        "include_global",
        "prefix",
        "index_token",
        "max_chars",
        "per_runbook_chars",
    ] {
        assert!(t.contains_key(k), "missing uncommented key {k}: {t:?}");
    }
}

#[test]
fn claim_runbook_disable_env() {
    let e = Env::new();
    e.write_project("a.md", "BODY-A");
    let o = e.inject_env("!a", &[("RUNBOOK_DISABLE", "1")]);
    assert_eq!(o.code, 0);
    assert_eq!(o.stdout, "");
    let o = e.inject_env("!a", &[("RUNBOOK_DISABLE", "yes")]);
    assert_eq!(o.stdout, "");
    let o = e.inject_env("!a", &[("RUNBOOK_DISABLE", "0")]);
    assert!(o.stdout.contains("BODY-A"), "{}", o.stdout);
    let o = e.inject_env("!a", &[("RUNBOOK_DISABLE", "")]);
    assert!(o.stdout.contains("BODY-A"), "{}", o.stdout);
}

#[test]
fn claim_config_first_file_wins() {
    let e = Env::new();
    e.write_project_config("max_chars = 5000\n");
    e.write_global_config("prefix = \"%\"\nmax_chars = 7000\n");
    let o = e.cli(&["status"]);
    assert_eq!(status_value(&o.stdout, "max_chars:"), "5000");
    assert_eq!(status_value(&o.stdout, "prefix:"), "!");

    let e = Env::new();
    e.write_global_config("prefix = \"%\"\nmax_chars = 7000\n");
    let o = e.cli(&["status"]);
    assert_eq!(status_value(&o.stdout, "max_chars:"), "7000");
    assert_eq!(status_value(&o.stdout, "prefix:"), "%");
}

#[test]
fn claim_budget_floor_clamp() {
    let max_of = |toml: &str| {
        let e = Env::new();
        e.write_project_config(toml);
        let o = e.cli(&["status"]);
        (
            status_value(&o.stdout, "max_chars:"),
            status_value(&o.stdout, "per_runbook_chars:"),
        )
    };
    assert_eq!(max_of("max_chars = 10\n").0, "200");
    assert_eq!(
        max_of("max_chars = 1000\nper_runbook_chars = 999999\n"),
        ("1000".into(), "1000".into())
    );
    assert_eq!(
        max_of("max_chars = 1000\nper_runbook_chars = 5\n"),
        ("1000".into(), "200".into())
    );
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn claim_platform_bins_not_committed() {
    let root = repo_root();
    let ls = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["-C"])
        .arg(&root)
        .args(["ls-files", "crates/runbook/bin"])
        .output()
        .unwrap();
    assert!(ls.status.success(), "git ls-files failed");
    assert_eq!(
        String::from_utf8_lossy(&ls.stdout).trim(),
        "crates/runbook/bin/runbook"
    );
    for name in [
        "runbook-darwin-arm64",
        "runbook-darwin-x86_64",
        "runbook-linux-arm64",
        "runbook-linux-x86_64",
    ] {
        let st = harness_core::git_env::scrub(&mut Command::new("git"))
            .args(["-C"])
            .arg(&root)
            .args(["check-ignore", "-q"])
            .arg(format!("crates/runbook/bin/{name}"))
            .status()
            .unwrap();
        assert_eq!(st.code(), Some(0), "{name} not ignored");
    }
}

#[test]
fn claim_makefile_crate_local_target() {
    let mk =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Makefile")).unwrap();
    assert!(mk.contains("cp target/release/runbook"), "{mk}");
    let out = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let td = PathBuf::from(v["target_directory"].as_str().unwrap());
    let crate_local = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    assert_ne!(td, crate_local, "workspace build uses crate-local target/");
    assert_eq!(td.file_name().unwrap(), "target");
    assert_eq!(
        td.parent().unwrap().canonicalize().unwrap(),
        repo_root(),
        "target dir is the workspace root's"
    );
}
