//! Black-box tests for ctxrot's proposed side of the SHARED autonomy switch
//! (backlog 9976b845). Companion to
//! `crates/condukt/tests/autonomy_single_switch.rs`, which drives the
//! `condukt state autonomy-set|autonomy-path|autonomy-check` half of the
//! contract; this file drives the `ctxrot autonomy` half.
//!
//! Today `ctxrot` has NO `autonomy` subcommand at all — `auto_distill_on_band`
//! (default true, `crates/ctxrot/src/config.rs:131`) and `auto_compact_enabled`
//! (default false, `crates/ctxrot/src/config.rs:141`) are two independent
//! per-crate bools that never consult condukt's autonomy switch. The proposed
//! contract adds a new subcommand:
//!
//!   `ctxrot autonomy` -> JSON
//!   `{"autonomous":<bool>,"auto_distill_on_band":<bool>,
//!     "auto_compact_enabled":<bool>,"source":"<layer>"}`, exit 0.
//!
//! When the shared switch is ON and the user has not explicitly configured
//! either ctxrot bool, BOTH must read true. An explicit ctxrot config value or
//! ctxrot env var (e.g. `CTXROT_AUTO_COMPACT`) always wins over the shared
//! switch. A corrupt shared switch file is fail-closed (CLAUDE.md \u{a7}3):
//! `autonomous:false`, `source:"undetermined-switch-file"` — never silently
//! autonomous.
//!
//! None of this exists yet: `ctxrot autonomy` is not a registered subcommand
//! (see `crates/ctxrot/src/main.rs`'s `enum Command`, which has no `Autonomy`
//! variant), so every test here is expected to fail against today's binary
//! with a clap "unrecognized subcommand" error — a WEAKER form of RED than a
//! behavioural assertion failure, and reported as such.
//!
//! NOTE (contract revision): `ctxrot autonomy`'s own JSON shape is UNCHANGED
//! from the original brief — no frozen oracle pins ctxrot's output the way
//! `autonomy_invariant.rs` pins condukt's plain `autonomy-check`. Only the
//! condukt-side setup helpers below (`condukt_autonomy_path`) changed, because
//! `condukt state autonomy-path` now prints
//! `{"path":"...","source":"..."}` instead of a bare path string.
//!
//! I did not implement anything here. I only wrote tests and ran them.

use std::path::{Path, PathBuf};
use std::process::Command;

fn ctxrot_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ctxrot")
}

/// The condukt binary, used only to set up the shared switch file via
/// `condukt state autonomy-set`. ctxrot does NOT depend on condukt, so cargo
/// will not build it as a side effect of `cargo test -p ctxrot`; both crates'
/// binaries land in the same workspace target dir though (siblings of
/// `CARGO_BIN_EXE_ctxrot`), so we build it on demand into that same dir and
/// then resolve the sibling path.
fn condukt_bin() -> PathBuf {
    let ctxrot_path = PathBuf::from(ctxrot_bin());
    let dir = ctxrot_path
        .parent()
        .expect("ctxrot binary has a parent dir")
        .to_path_buf();
    let candidate = dir.join(if cfg!(windows) {
        "condukt.exe"
    } else {
        "condukt"
    });
    if !candidate.exists() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("crates/ctxrot has a repo root two levels up")
            .to_path_buf();
        let is_release = dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n == "release")
            .unwrap_or(false);
        let mut cmd = Command::new(env!("CARGO"));
        cmd.args(["build", "-p", "condukt"]);
        if is_release {
            cmd.arg("--release");
        }
        cmd.current_dir(&repo_root);
        let out = cmd.output().expect("spawn cargo build -p condukt");
        assert!(
            out.status.success(),
            "on-demand `cargo build -p condukt` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(
        candidate.exists(),
        "expected a `condukt` binary at {} even after building it on demand",
        candidate.display()
    );
    candidate
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Sandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    autonomy_dir: PathBuf,
    cwd: PathBuf,
}

fn init_git_repo(dir: &Path) {
    let run = |args: &[&str]| {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test.invalid")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test.invalid")
            .output()
            .expect("run git");
        assert!(
            st.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&st.stderr)
        );
    };
    run(&["init", "-q"]);
    std::fs::write(dir.join("README.md"), "sandbox\n").unwrap();
    run(&["add", "README.md"]);
    run(&["commit", "-q", "-m", "init"]);
}

fn sandbox() -> Sandbox {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let autonomy_dir = tmp.path().join("autonomy_dir");
    let cwd = tmp.path().join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&autonomy_dir).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    init_git_repo(&cwd);
    Sandbox {
        _tmp: tmp,
        home,
        autonomy_dir,
        cwd,
    }
}

/// Set the shared switch via the real `condukt` binary, sandboxed identically
/// to `ctxrot`'s sandbox (same HOME + HARNESS_AUTONOMY_DIR + cwd) so both
/// crates resolve the same durable file.
fn condukt_autonomy_set(sb: &Sandbox, mode: &str) {
    let out = Command::new(condukt_bin())
        .args(["state", "autonomy-set", mode])
        .current_dir(&sb.cwd)
        .env("HOME", &sb.home)
        .env("HARNESS_AUTONOMY_DIR", &sb.autonomy_dir)
        .env_remove("CONDUKT_AUTONOMOUS")
        .env_remove("HARNESS_AUTONOMOUS")
        .output()
        .expect("run condukt state autonomy-set");
    assert!(
        out.status.success(),
        "condukt state autonomy-set {mode} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The resolved switch-file path, via the real `condukt` binary (kept as the
/// single source of truth for path resolution rather than re-deriving it).
///
/// REVISED CONTRACT: `autonomy-path` prints JSON
/// `{"path":"<absolute path>","source":"<layer>"}`, not a bare path string
/// (the second Claude session's ruling, to keep the FROZEN
/// `autonomy_invariant.rs` oracle on the plain `autonomy-check` command
/// untouched while still giving `--explain`/`autonomy-path` a place to name
/// which layer decided).
fn condukt_autonomy_path(sb: &Sandbox) -> PathBuf {
    let out = Command::new(condukt_bin())
        .args(["state", "autonomy-path"])
        .current_dir(&sb.cwd)
        .env("HOME", &sb.home)
        .env("HARNESS_AUTONOMY_DIR", &sb.autonomy_dir)
        .env_remove("CONDUKT_AUTONOMOUS")
        .env_remove("HARNESS_AUTONOMOUS")
        .output()
        .expect("run condukt state autonomy-path");
    assert!(
        out.status.success(),
        "condukt state autonomy-path failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("condukt state autonomy-path stdout was not valid JSON: {e}; stdout={stdout:?}")
    });
    let path = v
        .get("path")
        .and_then(|p| p.as_str())
        .unwrap_or_else(|| panic!("condukt state autonomy-path JSON missing \"path\": {stdout:?}"));
    PathBuf::from(path)
}

/// Run `ctxrot autonomy`, sandboxed: HOME + HARNESS_AUTONOMY_DIR pinned,
/// ctxrot's own autonomy-relevant env vars scrubbed unless explicitly passed.
fn run_ctxrot_autonomy(sb: &Sandbox, env: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new(ctxrot_bin());
    cmd.arg("autonomy")
        .current_dir(&sb.cwd)
        .env("HOME", &sb.home)
        .env("HARNESS_AUTONOMY_DIR", &sb.autonomy_dir)
        .env_remove("CONDUKT_AUTONOMOUS")
        .env_remove("HARNESS_AUTONOMOUS")
        .env_remove("CTXROT_AUTO_COMPACT")
        .env_remove("CTXROT_AUTO_DISTILL_ON_BAND");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run ctxrot autonomy");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    }
}

fn json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout).unwrap_or_else(|e| {
        panic!("`ctxrot autonomy` stdout was not valid JSON: {e}; stdout={stdout:?}")
    })
}

// ---------------------------------------------------------------------------
// A6 — ctxrot follows the shared switch when the user set nothing explicitly
// ---------------------------------------------------------------------------
#[test]
fn a6_ctxrot_follows_shared_switch_when_on_and_unconfigured() {
    let sb = sandbox();
    condukt_autonomy_set(&sb, "on");

    let run = run_ctxrot_autonomy(&sb, &[]);
    assert_eq!(
        run.code, 0,
        "A6: `ctxrot autonomy` must exit 0; stdout={:?} stderr={:?}",
        run.stdout, run.stderr
    );
    let v = json(&run.stdout);
    assert_eq!(
        v.get("auto_distill_on_band").and_then(|b| b.as_bool()),
        Some(true),
        "A6: with the shared switch ON and no explicit ctxrot config/env, \
         auto_distill_on_band must be true; got {}",
        run.stdout
    );
    assert_eq!(
        v.get("auto_compact_enabled").and_then(|b| b.as_bool()),
        Some(true),
        "A6: with the shared switch ON and no explicit ctxrot config/env, \
         auto_compact_enabled must be true (today it defaults false and never \
         consults condukt's switch); got {}",
        run.stdout
    );
}

// ---------------------------------------------------------------------------
// A7 — an explicit ctxrot env var still wins over the shared switch
// ---------------------------------------------------------------------------
#[test]
fn a7_explicit_ctxrot_env_beats_shared_switch() {
    let sb = sandbox();
    condukt_autonomy_set(&sb, "on");

    let run = run_ctxrot_autonomy(&sb, &[("CTXROT_AUTO_COMPACT", "0")]);
    assert_eq!(
        run.code, 0,
        "A7: `ctxrot autonomy` must exit 0; stdout={:?} stderr={:?}",
        run.stdout, run.stderr
    );
    let v = json(&run.stdout);
    assert_eq!(
        v.get("auto_compact_enabled").and_then(|b| b.as_bool()),
        Some(false),
        "A7: CTXROT_AUTO_COMPACT=0 is an explicit user decision and must NOT \
         be overridden by the shared switch being ON; got {}",
        run.stdout
    );
}

// ---------------------------------------------------------------------------
// A8 — ctxrot fails closed on a corrupt shared switch file
// ---------------------------------------------------------------------------
#[test]
fn a8_ctxrot_fails_closed_on_corrupt_shared_switch_file() {
    let sb = sandbox();
    let path = condukt_autonomy_path(&sb);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, b"not json at all {{{").expect("write garbage switch file");

    let run = run_ctxrot_autonomy(&sb, &[]);
    assert_eq!(
        run.code, 0,
        "A8: `ctxrot autonomy` itself is a plain report, not a gate, so it \
         should still exit 0 even when the answer is \"undetermined -> off\"; \
         stdout={:?} stderr={:?}",
        run.stdout, run.stderr
    );
    let v = json(&run.stdout);
    assert_eq!(
        v.get("autonomous").and_then(|b| b.as_bool()),
        Some(false),
        "A8 (CLAUDE.md \u{a7}3 fail-closed): a corrupt shared switch file must \
         resolve to NOT autonomous, never silently autonomous; got {}",
        run.stdout
    );
    assert_eq!(
        v.get("source").and_then(|s| s.as_str()),
        Some("undetermined-switch-file"),
        "A8: the JSON `source` must name the corrupt-file case explicitly \
         (\"undetermined-switch-file\"), distinguishable from a plain off; \
         got {}",
        run.stdout
    );
}
