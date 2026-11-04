// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Spike (user ruling 2026-11-03): can fde reuse `specforge draft` for a
//! FOREIGN target while keeping that target untouched?
//!
//! Setup, all in temp dirs outside the harness repo:
//!   - `target/`  a throwaway git repo with a commit, tracked + untracked +
//!     ignored files and hidden dirs (the foreign repo fde would work on);
//!   - `state/`   a separate dir outside the target holding `specforge.toml`
//!     with `project.root = "."` (= the state dir), so every relative output
//!     path of specforge (spec_dir, sentinel, `.scratch/specforge`) resolves
//!     into the state dir;
//!   - `material/req.md`  the gathered material, passed via `--req`, also
//!     outside the target.
//!
//! The `[agent]` is a deterministic stub (`bash -c`, drains stdin, prints a
//! fixed Spec IR draft from `tests/fixtures/spike/agent_draft.txt`); no real
//! LLM is called. specforge is spawned with cwd = the target (the worst case
//! for fde: it would usually run from inside the target).
//!
//! Asserted:
//!   (1) the target is unchanged: `git status --porcelain --ignored` output is
//!       identical, a recursive listing of every path (hidden dirs and `.git`
//!       included) with each file's sha256 is identical, and no entry has an
//!       mtime newer than a marker taken right before the run;
//!   (2) `<state>/specs/<id>.toml` exists and parses as TOML with `[spec] id`
//!       equal to the requested id and at least one `[[requirement]]`.
//!
//! The specforge binary is resolved in this order:
//!   1. `$FDE_SPECFORGE_BIN` if set (it must name a file, else the test FAILS
//!      without trying anything else);
//!   2. the workspace's own `specforge` (`[[bin]]` of `crates/specguard`),
//!      built by `$CARGO build -p specguard --bin specforge
//!      --message-format=json` against this workspace's `Cargo.toml`, with a
//!      dedicated `--target-dir` (`<workspace>/target/fde-spike-specforge`) so it
//!      never waits on the build-directory lock held by the outer
//!      `cargo test`; the path is the `executable` of the `compiler-artifact`
//!      message for target `specforge`, and the build must exit 0;
//!   3. `specforge` on `PATH` (only if step 2 failed, e.g. `$CARGO` unset).
//!
//! If none works the test FAILS (never skips), listing why each step failed.
//! If the chosen binary cannot be spawned the test FAILS too. The chosen path
//! is printed to stderr (`specforge=<path>`).
//!
//! What this test does NOT prove: anything about a real LLM agent (which runs
//! with cwd = repo_root = the state dir and could still be told to read or
//! write elsewhere), or any specforge subcommand other than `draft`.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

const GIT_LEAK_VARS: &[&str] = &["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"];
const SPEC_ID: &str = "spike-foreign-target";

fn scrubbed(program: &Path) -> Command {
    let mut c = Command::new(program);
    for v in GIT_LEAK_VARS {
        c.env_remove(v);
    }
    c
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = scrubbed(Path::new("git"))
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git spawns");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("git output is utf-8")
}

/// `git status --porcelain --ignored` without taking optional locks, so the
/// observation itself cannot rewrite `.git/index`.
fn status(target: &Path) -> String {
    git(
        target,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--ignored",
            "--untracked-files=all",
        ],
    )
}

fn locate_specforge() -> PathBuf {
    if let Some(p) = std::env::var_os("FDE_SPECFORGE_BIN") {
        let p = PathBuf::from(p);
        assert!(
            p.is_file(),
            "FDE_SPECFORGE_BIN={} does not name a file — cannot run the spike",
            p.display()
        );
        return p;
    }
    let build_err = match build_workspace_specforge() {
        Ok(p) => return p,
        Err(e) => e,
    };
    let path_err = match specforge_on_path() {
        Ok(p) => return p,
        Err(e) => e,
    };
    panic!(
        "specforge binary not found (the spike cannot be decided without it; this is a FAIL, \
         not a skip):\n  1. FDE_SPECFORGE_BIN: unset\n  2. workspace build: {build_err}\n  \
         3. PATH lookup: {path_err}"
    );
}

/// Step 2 of [`locate_specforge`]: build the workspace's `specforge` and
/// return the produced executable, or why that was impossible.
fn build_workspace_specforge() -> Result<PathBuf, String> {
    let cargo = std::env::var_os("CARGO").ok_or("CARGO is not set")?;
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = workspace.join("Cargo.toml");
    if !manifest.is_file() {
        return Err(format!("{} is not a file", manifest.display()));
    }
    // A target dir separate from the outer `cargo test`'s, so this build never
    // waits on its build-directory lock.
    let target_dir = workspace.join("target").join("fde-spike-specforge");
    let out = Command::new(&cargo)
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target_dir)
        .args([
            "-p",
            "specguard",
            "--bin",
            "specforge",
            "--message-format=json",
        ])
        .output()
        .map_err(|e| format!("spawning {} failed: {e}", Path::new(&cargo).display()))?;
    if !out.status.success() {
        return Err(format!(
            "cargo build exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let stdout = String::from_utf8(out.stdout).map_err(|e| format!("non-utf8 stdout: {e}"))?;
    let mut found = None;
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if msg.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        if msg.pointer("/target/name").and_then(|n| n.as_str()) != Some("specforge") {
            continue;
        }
        if let Some(exe) = msg.get("executable").and_then(|e| e.as_str()) {
            found = Some(PathBuf::from(exe));
        }
    }
    let exe = found.ok_or("cargo build succeeded but reported no specforge executable")?;
    if !exe.is_file() {
        return Err(format!(
            "reported executable {} is not a file",
            exe.display()
        ));
    }
    Ok(exe)
}

/// Step 3 of [`locate_specforge`]: an executable `specforge` on `PATH`.
fn specforge_on_path() -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH").ok_or("PATH is not set")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join("specforge");
        if let Ok(md) = fs::metadata(&cand) {
            if md.is_file() && md.permissions().mode() & 0o111 != 0 {
                return Ok(cand);
            }
        }
    }
    Err(format!(
        "no executable `specforge` in PATH={}",
        path.to_string_lossy()
    ))
}

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Dir,
    File(String),
    Symlink(PathBuf),
}

/// Every path under `root` (hidden entries and `.git` included), with the
/// sha256 of each regular file, plus the newest mtime seen.
fn snapshot(root: &Path) -> (BTreeMap<PathBuf, Entry>, Vec<(PathBuf, SystemTime)>) {
    let mut map = BTreeMap::new();
    let mut mtimes = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for ent in fs::read_dir(&dir).expect("read_dir") {
            let ent = ent.expect("dir entry");
            let path = ent.path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            let md = fs::symlink_metadata(&path).expect("lstat");
            mtimes.push((rel.clone(), md.modified().expect("mtime")));
            let ft = md.file_type();
            if ft.is_symlink() {
                map.insert(rel, Entry::Symlink(fs::read_link(&path).unwrap()));
            } else if ft.is_dir() {
                map.insert(rel, Entry::Dir);
                stack.push(path);
            } else {
                let bytes = fs::read(&path).expect("read file");
                let digest = Sha256::digest(&bytes);
                let hex = digest.iter().map(|b| format!("{b:02x}")).collect();
                map.insert(rel, Entry::File(hex));
            }
        }
    }
    (map, mtimes)
}

fn make_target(target: &Path) {
    git(target, &["init", "-q"]);
    git(target, &["config", "user.email", "t@t.t"]);
    git(target, &["config", "user.name", "t"]);
    git(target, &["config", "commit.gpgsign", "false"]);
    fs::create_dir_all(target.join("src")).unwrap();
    fs::create_dir_all(target.join("docs")).unwrap();
    fs::write(target.join("README.md"), "foreign target\n").unwrap();
    fs::write(target.join("src/lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();
    fs::write(target.join("docs/fde.md"), "# target-untouched\n").unwrap();
    // Ignore the paths a leaking specforge would write, so they would surface
    // in `--ignored` as well as in the listing.
    fs::write(
        target.join(".gitignore"),
        ".scratch/\nspecs/\n.specforge-pending\nignored.log\n",
    )
    .unwrap();
    git(target, &["add", "-A"]);
    git(target, &["commit", "-q", "-m", "seed"]);
    // Untracked, ignored and hidden-dir content after the commit.
    fs::write(target.join("untracked.txt"), "wip\n").unwrap();
    fs::write(target.join("ignored.log"), "log\n").unwrap();
    fs::create_dir_all(target.join(".hidden/inner")).unwrap();
    fs::write(target.join(".hidden/inner/x"), "x\n").unwrap();
    // Settle the index stat cache once, so later lock-free observations are
    // comparing a quiescent tree.
    git(target, &["status", "--porcelain"]);
}

#[test]
fn specforge_draft_leaves_foreign_target_untouched() {
    let specforge = locate_specforge();

    let target_tmp = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    let material_tmp = tempfile::tempdir().unwrap();
    let target = target_tmp.path().canonicalize().unwrap();
    let state = state_tmp.path().canonicalize().unwrap();
    let material = material_tmp.path().canonicalize().unwrap();
    assert!(!state.starts_with(&target) && !target.starts_with(&state));
    assert!(!material.starts_with(&target));

    make_target(&target);

    let draft_fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/spike/agent_draft.txt")
        .canonicalize()
        .expect("fixture exists");
    let script = format!(
        "cat >/dev/null; cat '{}'",
        draft_fixture.display().to_string().replace('\'', r"'\''")
    );
    let cfg = format!(
        r#"[project]
name = "SpikeForeign"
root = "."

[agent]
command = "bash"
args = ["-c", {script:?}]

[output]
spec_dir = "specs"
sentinel = ".specforge-pending"
"#
    );
    let cfg_path = state.join("specforge.toml");
    fs::write(&cfg_path, cfg).unwrap();
    let req = material.join("req.md");
    fs::write(
        &req,
        "# gathered material\nfde must not write into the foreign target.\n\
         target src/lib.rs: pub fn f() -> u8 { 1 }\n",
    )
    .unwrap();

    let status_before = status(&target);
    let (tree_before, _) = snapshot(&target);
    std::thread::sleep(Duration::from_millis(50));
    let marker = SystemTime::now();
    std::thread::sleep(Duration::from_millis(50));

    let out = match scrubbed(&specforge)
        .current_dir(&target)
        .arg("--config")
        .arg(&cfg_path)
        .args(["--date", "2026-11-03", "draft", "--id", SPEC_ID, "--req"])
        .arg(&req)
        .output()
    {
        Ok(o) => o,
        Err(e) => panic!("failed to spawn specforge at {}: {e}", specforge.display()),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!(
        "specforge={} exit={:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        specforge.display(),
        out.status.code()
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "specforge draft did not exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );

    // (1) target untouched.
    let status_after = status(&target);
    assert_eq!(
        status_before, status_after,
        "git status --porcelain --ignored changed"
    );
    let (tree_after, mtimes_after) = snapshot(&target);
    assert_eq!(
        tree_before, tree_after,
        "target tree listing/content hashes changed"
    );
    let newer: Vec<_> = mtimes_after
        .iter()
        .filter(|(_, t)| *t > marker)
        .map(|(p, _)| p.clone())
        .collect();
    assert!(
        newer.is_empty(),
        "entries under the target modified after the marker: {newer:?}"
    );

    // (2) draft spec landed under the state dir's spec_dir.
    let spec_path = state.join("specs").join(format!("{SPEC_ID}.toml"));
    let text = fs::read_to_string(&spec_path).unwrap_or_else(|e| {
        panic!("draft spec {} not readable: {e}", spec_path.display());
    });
    let v: toml::Value = toml::from_str(&text).expect("draft spec parses as TOML");
    assert_eq!(
        v.get("spec")
            .and_then(|s| s.get("id"))
            .and_then(|i| i.as_str()),
        Some(SPEC_ID),
        "[spec] id missing or wrong:\n{text}"
    );
    let reqs = v
        .get("requirement")
        .and_then(|r| r.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    assert!(reqs >= 1, "no [[requirement]] in draft:\n{text}");
    assert!(
        !state.join(".specforge-pending").exists(),
        "clean rigor must not raise the sentinel"
    );
}
