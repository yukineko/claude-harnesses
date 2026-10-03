#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Contract of `backlog add`'s cross-session claim check (backlog 420f1eec,
//! round 2). The real `condukt state is-claimed` exits 1 both for "not claimed"
//! and (before the fix) for "claim registry unreadable", so exit 1 alone cannot
//! be trusted: backlog must also see a stdout JSON whose `claimed` agrees with
//! the exit code. Anything else is UNDETERMINED and `add` is refused.
//!
//! Each test drives the BUILT `backlog` binary with a PATH shim named
//! `condukt`, from a LINKED git worktree, and asserts on the stored file.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TITLE: &str = "claim contract probe";

struct Fx {
    home: PathBuf,
    linked: PathBuf,
    shim: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?} failed: {out:?}");
}

fn fx(tag: &str) -> Fx {
    let root = std::env::temp_dir().join(format!(
        "backlog-claimcontract-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let home = root.join("home");
    let main = root.join("main");
    let linked = root.join("linked");
    let shim = root.join("shim");
    for d in [&home, &main, &shim] {
        std::fs::create_dir_all(d).unwrap();
    }
    git(&main, &["init", "-q"]);
    git(&main, &["config", "user.email", "t@t.t"]);
    git(&main, &["config", "user.name", "t"]);
    std::fs::write(main.join("README"), "x").unwrap();
    git(&main, &["add", "README"]);
    git(&main, &["commit", "-q", "-m", "init"]);
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "wt",
        ],
    );
    Fx { home, linked, shim }
}

impl Fx {
    fn write_shim(&self, body: &str) {
        let p = self.shim.join("condukt");
        let _ = std::fs::remove_file(&p);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Warm the freshly written script: the first exec of a new file can be
        // slow (OS scanning) and would blow a tight claim-check bound, turning
        // every case into a "timed out" refusal that proves nothing.
        let _ = Command::new(&p)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn write_nonexec_shim(&self) {
        let p = self.shim.join("condukt");
        std::fs::write(&p, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    fn add(&self, extra: &[&str]) -> (i32, String, String) {
        let proj = self.linked.to_str().unwrap();
        let mut args = vec!["add", "--title", TITLE, "--project", proj];
        args.extend_from_slice(extra);
        let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
            .args(&args)
            .env("HOME", &self.home)
            // Shim + minimal system dirs only: an inherited PATH could hold a
            // real `condukt` that answers in the shim's place.
            .env("PATH", common::isolated_path_with(&self.shim))
            .current_dir(&self.linked)
            .stdin(Stdio::null())
            .output()
            .expect("binary runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Concatenated text of every `.toml` under the linked checkout's store.
    fn stored(&self) -> String {
        let mut s = String::new();
        let dir = self.linked.join(".backlog");
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.path().extension().and_then(|x| x.to_str()) == Some("toml") {
                    s.push_str(&std::fs::read_to_string(e.path()).unwrap_or_default());
                }
            }
        }
        s
    }
}

fn assert_refused(f: &Fx, r: (i32, String, String), why: &str) {
    let (rc, out, err) = r;
    assert_ne!(rc, 0, "{why}: add must be refused; out={out:?} err={err:?}");
    assert!(
        !f.stored().contains(TITLE),
        "{why}: the task must NOT be stored; store={:?}",
        f.stored()
    );
    assert!(
        !err.contains("timed out"),
        "{why}: refused only because the shim timed out, which proves nothing; err={err:?}"
    );
    assert!(
        err.contains("--force"),
        "{why}: stderr must tell the operator about --force; err={err:?}"
    );
}

const NOT_CLAIMED_JSON: &str = r#"echo '{"hashkey":"x","claimed":false,"holder_run":null}'"#;

#[test]
fn control_exit1_with_claimed_false_json_adds() {
    let f = fx("ctl");
    f.write_shim(&format!("{NOT_CLAIMED_JSON}\nexit 1"));
    let (rc, out, err) = f.add(&[]);
    assert_eq!(rc, 0, "control must add; out={out:?} err={err:?}");
    assert!(f.stored().contains(TITLE), "task must be stored");
}

#[test]
fn exit1_empty_stdout_registry_error_is_refused() {
    let f = fx("empty");
    f.write_shim("echo 'refusing to list active claims' >&2\nexit 1");
    let r = f.add(&[]);
    assert_refused(&f, r, "exit 1 + empty stdout (corrupt registry shape)");
}

#[test]
fn exit1_garbage_stdout_is_refused() {
    let f = fx("garbage");
    f.write_shim("echo '{garbage'\nexit 1");
    let r = f.add(&[]);
    assert_refused(&f, r, "exit 1 + unparseable stdout");
}

#[test]
fn exit1_with_claimed_true_contradiction_is_refused() {
    let f = fx("contra1");
    f.write_shim(r#"echo '{"hashkey":"x","claimed":true,"holder_run":"r"}'; exit 1"#);
    let r = f.add(&[]);
    assert_refused(&f, r, "exit 1 but stdout says claimed:true");
}

#[test]
fn exit0_with_claimed_false_contradiction_is_refused() {
    let f = fx("contra0");
    f.write_shim(&format!("{NOT_CLAIMED_JSON}\nexit 0"));
    let r = f.add(&[]);
    assert_refused(&f, r, "exit 0 but stdout says claimed:false");
}

#[test]
fn exit0_claimed_true_is_refused_as_claimed() {
    let f = fx("claimed");
    f.write_shim(r#"echo '{"hashkey":"x","claimed":true,"holder_run":"r"}'; exit 0"#);
    let (rc, out, err) = f.add(&[]);
    assert_ne!(rc, 0, "claimed must refuse; out={out:?} err={err:?}");
    assert!(!f.stored().contains(TITLE), "task must not be stored");
    assert!(
        !err.contains("timed out"),
        "timeout proves nothing; err={err:?}"
    );
    assert!(
        err.contains("claimed by a live cross-session run"),
        "must use the duplicate-claimed message, not the undetermined one; err={err:?}"
    );
}

#[test]
fn signal_killed_condukt_is_refused() {
    let f = fx("sig");
    f.write_shim("kill -9 $$");
    let r = f.add(&[]);
    assert_refused(&f, r, "condukt killed by signal");
}

#[test]
fn non_executable_condukt_is_refused() {
    let f = fx("nonexec");
    f.write_nonexec_shim();
    let r = f.add(&[]);
    assert_refused(&f, r, "non-executable condukt (spawn error)");
}

#[test]
fn force_overrides_an_undetermined_claim_check() {
    let f = fx("force");
    f.write_shim("echo 'refusing to list active claims' >&2\nexit 1");
    let (rc, out, err) = f.add(&["--force"]);
    assert_eq!(rc, 0, "--force must add; out={out:?} err={err:?}");
    assert!(f.stored().contains(TITLE), "task must be stored");
}
