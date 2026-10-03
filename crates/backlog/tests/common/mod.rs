//! Shared fixture for the close-evidence contract tests.
//!
//! Written by an independent test writer (CLAUDE.md §2(a)). Everything here
//! drives the REAL built binary (`CARGO_BIN_EXE_backlog`) against an isolated
//! HOME and an isolated, REAL git repo (`git init`) whose committed test
//! scripts live under `tests/`. No cargo-in-cargo: test commands are tiny bash
//! scripts (exit 0 = pass, exit 1 = behavioural failure), and the cargo-shaped
//! cases use a fake `cargo` shim placed first on PATH.
//!
//! CLI surface these tests assume (the spec fixes the semantics, not every
//! flag spelling; the implementer must match, or raise the spelling with the
//! test author rather than weaken a test):
//!   backlog done ID --test CMD [--red-rev REV] [--reason fixed|already-fixed|obsolete]
//!   backlog done ID --duplicate-of ID
//!   backlog done ID --doc-only COMMIT
//!   backlog ruling request ID --kind judgment --rationale T
//!   backlog ruling request ID --kind untestable --untestable-reason T
//!   backlog ruling approve ID | withdraw ID | list
//!   backlog add ... [--repro-test CMD]   (only a REPRODUCED outcome lands pending; else `unconfirmed`)
//!   backlog confirm ID --repro-test CMD
//!   backlog audit-closures [--json]
//! A closed row is read back with `list --all --json`; its recorded closure is
//! the JSON object under the key `closure`.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn both(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

pub fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-ce-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub struct Fixture {
    pub home: PathBuf,
    pub repo: PathBuf,
    pub shim: PathBuf,
}

/// Fake `cargo`: behaviour is decided by the `marker` file in the CWD (the
/// checkout the runner executes in), so the SAME command line is RED at the
/// broken rev and GREEN at HEAD.
///   marker == fixed   -> "test result: ok. 1 passed", exit 0
///   marker == broken  -> "test result: FAILED. 0 passed; 1 failed", exit 101 (behavioural)
///   marker == nobuild -> "error[E0432]: unresolved import" ... exit 101 (BUILD failure)
///   marker == empty   -> "test result: ok. 0 passed; 0 failed", exit 0 (0-passed)
///   marker == hang    -> sleeps
const FAKE_CARGO: &str = r#"#!/bin/bash
m="$(cat marker 2>/dev/null)"
case "$m" in
  fixed)   echo "running 1 test"; echo "test result: ok. 1 passed; 0 failed; 0 ignored"; exit 0;;
  broken)  echo "running 1 test"; echo "test t ... FAILED"; echo "test result: FAILED. 0 passed; 1 failed; 0 ignored"; exit 101;;
  nobuild) echo "error[E0432]: unresolved import \`crate::nope\`"; echo "error: could not compile \`x\` (lib test) due to 1 previous error"; exit 101;;
  empty)   echo "running 0 tests"; echo "test result: ok. 0 passed; 0 failed; 0 ignored"; exit 0;;
  hang)    sleep 60; exit 0;;
  *)       echo "no marker"; exit 2;;
esac
"#;

/// A `condukt` that answers "not claimed" the way the real one does:
/// `{"claimed":false}` on stdout and exit 1.
const NOT_CLAIMED_CONDUKT: &str = "#!/bin/sh\necho '{\"claimed\":false}'\nexit 1\n";

impl Fixture {
    pub fn new(tag: &str) -> Self {
        let home = unique_dir(&format!("{tag}-home"));
        let repo = unique_dir(&format!("{tag}-repo"));
        let shim = unique_dir(&format!("{tag}-shim"));
        let repo = repo.canonicalize().unwrap();
        let f = Fixture { home, repo, shim };
        f.git(&["init", "-q", "-b", "main"]);
        f.write("README.txt", "x\n");
        f.write_exec_shim("cargo", FAKE_CARGO);
        // `add` runs the cross-session claim check (`condukt state is-claimed`),
        // and an unusable condukt is UNDETERMINED, so `add` is refused (backlog
        // 420f1eec). Pin a deterministic "not claimed" answer for this fixture.
        f.write_exec_shim("condukt", NOT_CLAIMED_CONDUKT);
        // Repro scripts: a FAILING repro (exit 1) means the bug is REPRODUCED
        // (same polarity as a RED test); exit 0 means NOT reproduced.
        f.write_script("tests/repro_yes.sh", "echo 'bug present'; exit 1");
        f.write_script("tests/repro_no.sh", "echo 'no bug'; exit 0");
        f.commit_paths(
            &["README.txt", "tests/repro_yes.sh", "tests/repro_no.sh"],
            "init",
        );
        f
    }

    pub fn write_exec_shim(&self, name: &str, body: &str) {
        let p = self.shim.join(name);
        std::fs::write(&p, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    pub fn project(&self) -> String {
        self.repo.to_string_lossy().into_owned()
    }

    pub fn tasks_path(&self) -> PathBuf {
        self.repo.join(".backlog").join("tasks.toml")
    }

    pub fn done_path(&self) -> PathBuf {
        self.repo.join(".backlog").join("tasks.done.toml")
    }

    pub fn write(&self, rel: &str, body: &str) {
        let p = self.repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }

    /// Write an executable bash script (committed test scripts live in tests/).
    pub fn write_script(&self, rel: &str, body: &str) {
        self.write(rel, &format!("#!/bin/bash\n{body}\n"));
        use std::os::unix::fs::PermissionsExt;
        let p = self.repo.join(rel);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn git_cmd(&self) -> Command {
        let mut c = Command::new("git");
        c.current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com");
        c
    }

    pub fn git(&self, args: &[&str]) -> String {
        let out = self.git_cmd().args(args).output().expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Commit exactly these paths (never `.backlog`); returns the new full rev.
    pub fn commit_paths(&self, paths: &[&str], msg: &str) -> String {
        let mut a = vec!["add", "-f", "--"];
        a.extend_from_slice(paths);
        self.git(&a);
        self.git(&["commit", "-q", "--no-verify", "-m", msg]);
        self.head()
    }

    /// Remove a path and commit the removal.
    pub fn commit_rm(&self, path: &str, msg: &str) -> String {
        self.git(&["rm", "-q", "--", path]);
        self.git(&["commit", "-q", "--no-verify", "-m", msg]);
        self.head()
    }

    pub fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }

    /// Standard scenario: rev A has the bug (src/impl.txt=broken, marker=broken),
    /// rev B adds the committed tests and the fix. Returns (red_rev, head).
    /// Tests: tests/check.sh (bash; exit 1 while impl is not `fixed`).
    pub fn bug_then_fix(&self) -> (String, String) {
        self.write("src/impl.txt", "broken\n");
        self.write("marker", "broken");
        let a = self.commit_paths(&["src/impl.txt", "marker"], "buggy impl");
        self.write("src/impl.txt", "fixed\n");
        self.write("marker", "fixed");
        self.write_script(
            "tests/check.sh",
            "grep -q '^fixed$' src/impl.txt || { echo 'FAIL: impl not fixed'; exit 1; }\necho '1 passed'",
        );
        let b = self.commit_paths(&["src/impl.txt", "marker", "tests/check.sh"], "fix + test");
        (a, b)
    }

    pub fn run(&self, args: &[&str]) -> Out {
        self.run_env(args, &[])
    }

    pub fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        self.run_stdin(args, env, b"")
    }

    pub fn run_stdin(&self, args: &[&str], env: &[(&str, &str)], input: &[u8]) -> Out {
        let bin = env!("CARGO_BIN_EXE_backlog");
        let path = format!(
            "{}:{}",
            self.shim.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env_remove("BACKLOG_TEST_TIMEOUT_SECS")
            .current_dir(&self.repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("binary spawns");
        if let Some(mut s) = child.stdin.take() {
            let _ = s.write_all(input);
        }
        let out = child.wait_with_output().expect("binary runs");
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Add a finding that lands as `pending` (repro test exits 1 = reproduced).
    pub fn add(&self, title: &str) -> String {
        let p = self.project();
        let o = self.run(&[
            "add",
            "--title",
            title,
            "--project",
            &p,
            "--repro-test",
            "bash tests/repro_yes.sh",
        ]);
        assert_eq!(o.code, 0, "add failed: {}", o.both());
        parse_added(&o.stdout)
    }

    pub fn add_extra(&self, title: &str, extra: &[&str]) -> (Out, Option<String>) {
        let p = self.project();
        let mut a = vec!["add", "--title", title, "--project", &p];
        a.extend_from_slice(extra);
        let o = self.run(&a);
        let id = o
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("added: "))
            .map(|s| s.trim().to_string());
        (o, id)
    }

    pub fn list_all(&self) -> Vec<serde_json::Value> {
        let o = self.run(&["list", "--all", "--json"]);
        assert_eq!(o.code, 0, "list failed: {}", o.both());
        serde_json::from_str(o.stdout.trim())
            .unwrap_or_else(|e| panic!("list --json not an array ({e}): {:?}", o.stdout))
    }

    pub fn row(&self, id: &str) -> serde_json::Value {
        self.list_all()
            .into_iter()
            .find(|t| t["id"] == id)
            .unwrap_or_else(|| panic!("row {id} not in list --all --json"))
    }

    pub fn status(&self, id: &str) -> String {
        self.row(id)["status"].as_str().unwrap().to_string()
    }

    /// Both store files, concatenated (for byte-identity assertions).
    pub fn store_bytes(&self) -> Vec<u8> {
        let mut v = std::fs::read(self.tasks_path()).unwrap_or_default();
        v.extend(std::fs::read(self.done_path()).unwrap_or_default());
        v
    }

    /// Append a legacy terminal row (no closure evidence) straight to the done file.
    pub fn plant_legacy_done(&self, id: &str, title: &str, status: &str) {
        let body = format!(
            "[[task]]\nid = \"{id}\"\ntitle = \"{title}\"\nproject = \"{}\"\nproject_unresolved = false\ntags = []\ntouched_files = []\nstatus = \"{status}\"\nnotes = \"\"\ncreated_at = 1\nupdated_at = 1\nweight = 0.0\n",
            self.project()
        );
        let p = self.done_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut cur = std::fs::read_to_string(&p).unwrap_or_default();
        cur.push_str(&body);
        std::fs::write(&p, cur).unwrap();
    }
}

pub const REPRO_YES: &str = "bash tests/repro_yes.sh";
pub const REPRO_NO: &str = "bash tests/repro_no.sh";

pub fn parse_added(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("added: "))
        .unwrap_or_else(|| panic!("no `added: <id>` line in {stdout:?}"))
        .trim()
        .to_string()
}

pub fn assert_refused_unchanged(f: &Fixture, id: &str, before_status: &str, out: &Out, why: &str) {
    assert_ne!(
        out.code,
        0,
        "{why}: must be refused, got exit 0: {}",
        out.both()
    );
    assert_eq!(
        f.status(id),
        before_status,
        "{why}: a refused close must leave the status unchanged: {}",
        out.both()
    );
}

pub fn is_path(_p: &Path) -> bool {
    true
}

// ---- Shared condukt claim-check seam (backlog 420f1eec) ----

use std::sync::OnceLock;

/// Directory holding a `condukt` that reports "not claimed" the way the real
/// one does: `{"claimed":false}` on stdout and exit 1 (backlog trusts exit 1
/// only together with that field). One stable dir shared by every test process
/// (nothing leaks per pid); the script is published by atomic rename so a
/// concurrent reader never sees a half-written file.
pub fn condukt_shim_dir() -> &'static std::path::Path {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        use std::os::unix::fs::PermissionsExt;
        const BODY: &str = "#!/bin/sh\necho '{\"claimed\":false}'\nexit 1\n";
        let dir = std::env::temp_dir().join("backlog-test-condukt-shim-v2");
        std::fs::create_dir_all(&dir).expect("create shim dir");
        let sh = dir.join("condukt");
        if std::fs::read_to_string(&sh).ok().as_deref() != Some(BODY) {
            let tmp = dir.join(format!("condukt.{}.tmp", std::process::id()));
            std::fs::write(&tmp, BODY).expect("write shim");
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                .expect("chmod shim");
            std::fs::rename(&tmp, &sh).expect("publish shim");
        }
        dir
    })
}

/// `PATH` value with the shim first, then the inherited PATH.
pub fn path_with_condukt_shim() -> String {
    let old = std::env::var("PATH").unwrap_or_default();
    format!("{}:{}", condukt_shim_dir().display(), old)
}
