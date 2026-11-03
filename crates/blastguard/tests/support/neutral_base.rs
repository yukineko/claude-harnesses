//! A fixture base whose LOCATION cannot decide a worktree-root verdict.
//!
//! The `worktree_root_*` tests put a synthetic `<base>/src/proj`,
//! `<base>/src/.harness-worktrees` and `<base>/home` somewhere and ask the hook
//! binary about `rm -r` there. Two properties of that "somewhere" leak into the
//! verdict (backlog bdd85e20, measured 2026-10-01):
//!
//!   * the payload `cwd` / `CLAUDE_PROJECT_DIR` SPELLING: blastguard derives a
//!     worktree storage root from the LAST `.harness-worktrees` component of
//!     that path (`src/scope.rs`, `worktree_storage_candidates`). A checkout
//!     under `<parent>/.harness-worktrees/<wt>` (the layout CLAUDE.md §8
//!     mandates) therefore makes the REAL `<parent>/.harness-worktrees` a root,
//!     and every fixture path — victims included — sits inside it: Allow.
//!   * the REAL path of the fixture files: a real path under `/tmp`,
//!     `/private/tmp`, `/var/tmp` or `/private/var/tmp` is inside a blastguard
//!     temp safe root, so a recursive `rm` there is a confined Ask instead of
//!     the Deny the negative controls pin.
//!
//!   * a BUILD-OUTPUT component in the real path, when the fixture sits in a git
//!     checkout (observed 2026-11-03 in `~/.condukt/worktrees/<wt>`): the cargo
//!     target dir is `<checkout>/target`, which git ignores, so the deletion
//!     principle's class 4 (`src/deletion.rs`, `BUILD_OUTPUT_NAMES`: "ignored
//!     build output in a git work tree") makes a recursive `rm` of the victim
//!     an Allow whenever the operand's SPELLING reaches the work tree without a
//!     symlink. Measured with the real hook binary: a fixture under
//!     `<git checkout>/target/tmp` Allow even with `HOME=/nonexistent`; the same
//!     directory spelled through a symlink in `temp_dir()` Deny; a git repo
//!     whose ignored dir is named `ign` instead of `target` Deny ("ignored (not
//!     build output)"). The `.harness-worktrees` layout only passed because the
//!     alias it forced happened to put a symlink in the spelling.
//!
//! So the base has a PHYSICAL directory (where the files live) and a LEXICAL
//! spelling (what the tests hand to the hook: payload `cwd`,
//! `CLAUDE_PROJECT_DIR`, `HOME`, operands). The lexical spelling is the physical
//! directory itself when its canonical path has no `.harness-worktrees`
//! component and no build-output component
//! (`blastguard::deletion::BUILD_OUTPUT_NAMES`), else a symlink to it in
//! `std::env::temp_dir()` (whose own path must not contain a
//! `.harness-worktrees` component). blastguard canonicalises the anchor and
//! every operand, so the alias changes only the spelling the root derivation
//! and the class-4 symlink check read, not where anything resolves.
//!
//! Every test-binary RUN gets its own base: `<parent>/run-<pid>-<nanos>[-n]`
//! under the candidate's `blastguard-worktree-root-fixtures` directory, created
//! with `create_dir` (never reused). Fixtures `remove_dir_all` their own
//! `<base>/<test>/<name>` before rebuilding it, so a base shared by concurrent
//! runs let one run delete another's fixture mid-setup (backlog 84a90410, RED
//! repro `scripts/tests/blastguard-wtfix-escape-concurrent.sh`). The per-run
//! directory sits below the same physical parent and the same alias, so both
//! location properties above are unchanged: its real path is the parent's
//! plus one `run-*` component, and its lexical spelling is the alias plus that
//! component. `run-*` directories older than [`STALE_RUN`] are pruned.
//!
//! Physical candidates, in order: `$BLASTGUARD_TEST_FIXTURE_DIR` when set (then
//! the ONLY candidate — an explicit choice that fails is reported, never
//! silently replaced), `CARGO_TARGET_TMPDIR`, `<workspace>/target`.
//!
//! Every candidate is CALIBRATED against the real hook binary before use: an
//! `rm -rf <base>/calibration-<test>/outside/victim` from cwd
//! `<base>/calibration-<test>/src/proj` must come back exactly Deny. A location that turns it into Allow (an
//! enclosing worktree root) or Ask (a temp safe root), or anything else, is
//! rejected with the hook's output. When no candidate calibrates, this PANICS
//! with every rejection — the tests fail loudly rather than run somewhere their
//! assertions stop observing the property.

// A test that includes this module must also include `support/bounded.rs` as
// `mod bounded` at its crate root (the calibration spawn below uses it).
use crate::bounded;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const OVERRIDE_ENV: &str = "BLASTGUARD_TEST_FIXTURE_DIR";
const WORKTREE_DIR_NAME: &str = ".harness-worktrees";
const RUN_PREFIX: &str = "run-";
/// A `run-*` base untouched this long belongs to a finished run.
const STALE_RUN: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// The calibrated base (lexical spelling). Computed once per test binary
/// process, and private to it (see the module docs).
pub fn neutral_base() -> &'static Path {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(pick)
}

fn candidates() -> Vec<(&'static str, PathBuf)> {
    if let Some(dir) = std::env::var_os(OVERRIDE_ENV) {
        return vec![(OVERRIDE_ENV, PathBuf::from(dir))];
    }
    vec![
        (
            "CARGO_TARGET_TMPDIR",
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")),
        ),
        (
            "<workspace>/target",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        ),
    ]
}

fn pick() -> PathBuf {
    let mut rejected: Vec<String> = Vec::new();
    for (label, dir) in candidates() {
        match prepare(&dir).and_then(|lexical| calibrate(&lexical).map(|()| lexical)) {
            Ok(lexical) => return lexical,
            Err(why) => rejected.push(format!("{label} ({}): {why}", dir.display())),
        }
    }
    panic!(
        "no location-neutral fixture base: every candidate lets its location decide the \
         verdict. Set {OVERRIDE_ENV} to a directory whose real path is outside the \
         blastguard temp roots.\n  {}",
        rejected.join("\n  ")
    );
}

/// Create this run's private physical directory and return its lexical
/// spelling.
fn prepare(dir: &Path) -> Result<PathBuf, String> {
    let physical_parent = dir.join("blastguard-worktree-root-fixtures");
    std::fs::create_dir_all(&physical_parent).map_err(|e| format!("create: {e}"))?;
    let physical = physical_parent
        .canonicalize()
        .map_err(|e| format!("canonicalize: {e}"))?;
    prune_stale_runs(&physical);
    let run = create_run_dir(&physical)?;
    Ok(alias_parent(&physical)?.join(run))
}

/// Create a fresh `run-<pid>-<nanos>[-n]` directory under `parent`; returns its
/// name. `create_dir` fails on an existing name, so no two runs share one.
fn create_run_dir(parent: &Path) -> Result<String, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("clock: {e}"))?
        .as_nanos();
    let stem = format!("{RUN_PREFIX}{}-{nanos}", std::process::id());
    for n in 0..100u32 {
        let name = if n == 0 {
            stem.clone()
        } else {
            format!("{stem}-{n}")
        };
        match std::fs::create_dir(parent.join(&name)) {
            Ok(()) => return Ok(name),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(format!(
                    "create run dir {}: {e}",
                    parent.join(&name).display()
                ))
            }
        }
    }
    Err(format!(
        "no free run dir name for {stem} under {}",
        parent.display()
    ))
}

/// Remove `run-*` bases not modified for [`STALE_RUN`]. Best effort: a base
/// that cannot be inspected or removed is left alone (it only costs disk), and
/// a live run's base is never older than that.
fn prune_stale_runs(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(RUN_PREFIX) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > STALE_RUN);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The lexical spelling of the physical parent: itself, or its alias.
fn alias_parent(physical: &Path) -> Result<PathBuf, String> {
    if !has_worktree_component(physical) && !has_build_output_component(physical) {
        return Ok(physical.to_path_buf());
    }
    let tmp = std::env::temp_dir();
    if has_worktree_component(&tmp) {
        return Err(format!(
            "real path needs an alias spelling (a `{WORKTREE_DIR_NAME}` or build-output \
             component) and temp_dir() {} has a `{WORKTREE_DIR_NAME}` component too, \
             so no alias spelling is available",
            tmp.display()
        ));
    }
    // One alias per physical parent: concurrent runs from different checkouts
    // get different aliases; runs sharing a target dir share the alias but
    // each lives in its own `run-*` directory below it.
    let alias = tmp.join(format!(
        "blastguard-wtfix-{:016x}",
        fnv1a(physical.to_string_lossy().as_bytes())
    ));
    if std::fs::read_link(&alias).ok().as_deref() != Some(physical) {
        // Build the link under a private name, then rename over the alias:
        // atomic, so a concurrent test binary never sees a half-made alias.
        let staging = tmp.join(format!(
            "blastguard-wtfix-{:016x}.{}",
            fnv1a(physical.to_string_lossy().as_bytes()),
            std::process::id()
        ));
        let _ = std::fs::remove_file(&staging);
        std::os::unix::fs::symlink(physical, &staging)
            .map_err(|e| format!("alias symlink {}: {e}", staging.display()))?;
        std::fs::rename(&staging, &alias)
            .map_err(|e| format!("alias rename {}: {e}", alias.display()))?;
    }
    Ok(alias)
}

fn has_worktree_component(p: &Path) -> bool {
    p.components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new(WORKTREE_DIR_NAME))
}

fn has_build_output_component(p: &Path) -> bool {
    p.components().any(|c| {
        blastguard::deletion::BUILD_OUTPUT_NAMES
            .iter()
            .any(|n| c.as_os_str() == std::ffi::OsStr::new(n))
    })
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// `rm -rf <outside>/victim` from a plain project cwd must be exactly Deny.
fn calibrate(lexical: &Path) -> Result<(), String> {
    // Per test binary, so two binaries calibrating the same base never share it.
    let calib = lexical.join(concat!("calibration-", env!("CARGO_CRATE_NAME")));
    let _ = std::fs::remove_dir_all(&calib);
    let proj = calib.join("src/proj");
    let home = calib.join("home");
    let victim = calib.join("outside/victim");
    for d in [&proj, &home, &victim] {
        std::fs::create_dir_all(d).map_err(|e| format!("create {}: {e}", d.display()))?;
    }
    std::fs::write(victim.join("f"), "x").map_err(|e| format!("write: {e}"))?;
    let payload = serde_json::json!({
        "session_id": "worktree-root-calibration",
        "cwd": proj.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": format!("rm -rf {}", victim.display()) },
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    hook.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CLAUDE_PROJECT_DIR", &proj)
        .env("BLASTGUARD_APPROVALS_DIR", calib.join("store"))
        .env("HOME", &home)
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&calib);
    // Bounded (support/bounded.rs): a spawn failure, a `try_wait` failure or
    // a hook that does not exit within the deadline PANICS here rather than
    // returning Err, so it is never mistaken for "this candidate base leaks".
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.code() != Some(0) {
        return Err(format!(
            "calibration hook exited {:?}; stderr: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    if stdout.contains(r#""permissionDecision":"deny""#) {
        Ok(())
    } else if stdout.trim().is_empty() {
        Err("calibration `rm -rf <outside>/victim` was Allow (an enclosing worktree root?)".into())
    } else {
        Err(format!(
            "calibration `rm -rf <outside>/victim` was not Deny: {}",
            stdout.trim()
        ))
    }
}
