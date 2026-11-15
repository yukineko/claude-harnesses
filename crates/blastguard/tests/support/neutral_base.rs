//! A fixture base whose LOCATION cannot decide a verdict, and which is never
//! under the workspace `target/` (backlog 37955e32).
//!
//! The fixture tests put a synthetic `<base>/src/proj`,
//! `<base>/src/.harness-worktrees`, `<base>/home`, git repositories and linked
//! worktrees somewhere and ask the hook binary about commands there. Four
//! properties of that "somewhere" leak into the verdict or into the workspace
//! (the first two: backlog bdd85e20, measured 2026-10-01):
//!
//!   * the payload `cwd` / `CLAUDE_PROJECT_DIR` SPELLING: blastguard derives a
//!     worktree storage root from the LAST `.harness-worktrees` component of
//!     that path (`src/scope.rs`, `worktree_storage_candidates`). A base under
//!     `<parent>/.harness-worktrees/<wt>` therefore makes the REAL
//!     `<parent>/.harness-worktrees` a root, and every fixture path — victims
//!     included — sits inside it: Allow.
//!   * the REAL path of the fixture files: a real path under `/tmp`,
//!     `/private/tmp`, `/var/tmp` or `/private/var/tmp` is inside a blastguard
//!     temp safe root, so a recursive `rm` there is a confined Ask instead of
//!     the Deny the negative controls pin. A real path under any other
//!     `/var` / `/private/var` directory — which includes macOS
//!     `std::env::temp_dir()` (`/private/var/folders/...`) — is a SYSTEM
//!     directory for the deletion principle (`src/deletion.rs`,
//!     `SYSTEM_PREFIXES`), so every recursive `rm` there is a refusal and the
//!     Allow controls fail (measured 2026-11-14:
//!     `cd_and_chain_equivalence` `c3_repo_absolute_form_allow` "expected
//!     Allow, got Deny" with the base in `$TMPDIR`).
//!   * a BUILD-OUTPUT component in the real path, when the base sits in a git
//!     checkout (observed 2026-11-03 in `~/.condukt/worktrees/<wt>`): the
//!     deletion principle's class 4 ("ignored build output in a git work
//!     tree", `BUILD_OUTPUT_NAMES`) makes a recursive `rm` of the victim an
//!     Allow whenever the operand's SPELLING reaches the work tree without a
//!     symlink.
//!   * the workspace `target/` itself (backlog 37955e32): the forced
//!     `git worktree remove` check answers Ask (subagent: Deny) for any nested
//!     `.git` inside an ignored build dir of the named worktree, so git
//!     fixtures left in `<worktree>/target/tmp` made every harness worktree
//!     that ran these tests un-removable without a prompt. The base used to be
//!     `CARGO_TARGET_TMPDIR`; it no longer is, and no candidate below is.
//!
//! LOCATION RULE. Physical candidates, in order: `$BLASTGUARD_TEST_FIXTURE_DIR`
//! when set (then the ONLY candidate — an explicit choice that fails is
//! reported, never silently replaced), else `$HOME/.blastguard-test-fixtures`
//! (the test process's own `$HOME`; the hook under test is normally handed a
//! fixture `HOME` instead). The run's directory is
//! `<candidate>/blastguard-worktree-root-fixtures/run-<pid>-<nanos>[-n]`; that
//! `blastguard-worktree-root-fixtures` directory is a scratch namespace owned
//! by this suite alone. `target/`, `/tmp`, `$TMPDIR` and `/var/tmp` are not
//! used: respectively the workspace build dir (the leak above), a temp safe
//! root, a temp safe root / system directory, and a temp safe root. A
//! `$HOME/.<name>/...` directory below home-level is none of blastguard's
//! roots (not a temp or cache root, not `~/.condukt/worktrees`, no
//! `.harness-worktrees` component) and not a system directory. Whether it is
//! also outside every git work tree depends on the machine, which is why the
//! calibration below decides rather than this paragraph.
//!
//! The base has a PHYSICAL directory (where the files live) and a LEXICAL
//! spelling (what the tests hand to the hook: payload `cwd`,
//! `CLAUDE_PROJECT_DIR`, `HOME`, operands). The lexical spelling is the
//! physical directory itself when its canonical path has no
//! `.harness-worktrees` component and no build-output component
//! (`blastguard::deletion::BUILD_OUTPUT_NAMES`) — always the case for the
//! default candidate — else a symlink to it in `std::env::temp_dir()` (whose
//! own path must not contain a `.harness-worktrees` component). blastguard
//! canonicalises the anchor and every operand, so the alias changes only the
//! spelling the root derivation and the class-4 symlink check read, not where
//! anything resolves.
//!
//! UNIQUENESS AND CLEAN-UP. Every test-binary RUN gets its own base, created
//! with `create_dir` (never reused), so concurrent runs and concurrent
//! sessions never share one: a shared base let one run delete another's
//! fixture mid-setup (backlog 84a90410, RED repro
//! `scripts/tests/blastguard-wtfix-escape-concurrent.sh`). Fixtures
//! `remove_dir_all` only their own `<base>/<test>/<name>`. Two deletions
//! reach the run directories themselves, both fail-closed (on any doubt the
//! directory is skipped, never removed through):
//!   * SELF-CLEAN: at process exit (a libc `atexit` hook registered once the
//!     run dir exists) the run deletes its OWN run dir — exactly the physical
//!     path it created, and only if `symlink_metadata` still shows a real
//!     directory (not a symlink) whose canonical path is that same path. A
//!     failure is written to stderr; it never panics and never deletes
//!     anything else. A run killed before exit leaves its dir behind for the
//!     prune.
//!   * PRUNE: before creating its own dir, a run removes `run-*` entries
//!     DIRECTLY in the dedicated `blastguard-worktree-root-fixtures` directory
//!     that are real directories (`symlink_metadata`; a symlink or non-dir is
//!     skipped) not modified for [`STALE_RUN`], and only after checking that
//!     the entry's canonical path is exactly `<canonical dedicated dir>/<name>`
//!     and the dedicated dir's name is `blastguard-worktree-root-fixtures`.
//!     `std::fs::remove_dir_all` does not follow symlinks inside the tree.
//!
//! CALIBRATION. Every candidate is calibrated against the real hook binary
//! before use, with two probes from cwd `<base>/calibration-<test>/src/proj`
//! (no git there):
//!   * `rm -rf <base>/calibration-<test>/outside/victim` must come back exactly
//!     Deny. Allow means an enclosing worktree root or class 4 (ignored build
//!     output in a git work tree); Ask means a temp safe root.
//!   * `rm -rf <base>/calibration-<test>/src/proj/inner` must come back exactly
//!     Ask (inside the cwd safe root, undetermined: the confined Ask). Deny
//!     means the location itself is refused (a system directory such as
//!     `/private/var/folders`), which would make every Deny assertion pass
//!     for the wrong reason and every Allow assertion fail.
//!
//! Calibration env: the probe runs the hook with `env_clear()` plus `PATH`
//! (for `git`), and sets exactly `HOME=<calib>/home`,
//! `CLAUDE_PROJECT_DIR=<cwd>`, `BLASTGUARD_APPROVALS_DIR=<calib>/store`,
//! `CLAUDE_CODE_ENTRYPOINT=cli` and `CLAUDECODE=1`. So `BLASTGUARD_ASK`,
//! `TMPDIR`, `CLAUDE_CODE_SESSION_ID`, `HARNESS_SANDBOX_HOME`, `GIT_*` and
//! every other caller variable are absent. The two mode variables are both
//! required: `interactive::ask_available()` needs `CLAUDECODE=1` AND
//! `CLAUDE_CODE_ENTRYPOINT=cli`, else the confined Ask of probe 2 is hardened
//! to Deny and every candidate is rejected. Observed 2026-11-14 under the env
//! `backlog done` gives its test command (INHERITED_ENV only, no `CLAUDECODE`):
//! RED `tests/neutral_base_calibration_env_20261114.rs`.
//!
//! A location that fails either probe is rejected with the hook's output. When
//! no candidate calibrates, this PANICS with every rejection — the tests fail
//! loudly rather than run somewhere their assertions stop observing the
//! property. (Measured on macOS only, 2026-11-14: the default candidate
//! calibrates; an override in `$TMPDIR` (`/private/var/folders/...`) is
//! rejected by the second probe.)

// A test that includes this module must also include `support/bounded.rs` as
// `mod bounded` at its crate root (the calibration spawn below uses it).
use crate::bounded;
use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const OVERRIDE_ENV: &str = "BLASTGUARD_TEST_FIXTURE_DIR";
/// The default candidate, relative to the test process's `$HOME`.
const HOME_DIR_NAME: &str = ".blastguard-test-fixtures";
/// The dedicated namespace directory every run dir lives directly in.
const PARENT_NAME: &str = "blastguard-worktree-root-fixtures";
const WORKTREE_DIR_NAME: &str = ".harness-worktrees";
const RUN_PREFIX: &str = "run-";
/// A `run-*` base untouched this long belongs to a finished run.
const STALE_RUN: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// The physical run dir this process created (for the exit-time self-clean).
static OWN_RUN: OnceLock<PathBuf> = OnceLock::new();

/// The calibrated base (lexical spelling). Computed once per test binary
/// process, and private to it (see the module docs).
pub fn neutral_base() -> &'static Path {
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    BASE.get_or_init(pick)
}

fn candidates() -> Vec<(&'static str, Result<PathBuf, String>)> {
    if let Some(dir) = std::env::var_os(OVERRIDE_ENV) {
        return vec![(OVERRIDE_ENV, Ok(PathBuf::from(dir)))];
    }
    let home = match std::env::var_os("HOME") {
        Some(h) if Path::new(&h).is_absolute() => Ok(Path::new(&h).join(HOME_DIR_NAME)),
        Some(h) => Err(format!("HOME {h:?} is not absolute")),
        None => Err("HOME is unset".to_string()),
    };
    vec![("$HOME/.blastguard-test-fixtures", home)]
}

fn pick() -> PathBuf {
    let mut rejected: Vec<String> = Vec::new();
    for (label, dir) in candidates() {
        let dir = match dir {
            Ok(d) => d,
            Err(why) => {
                rejected.push(format!("{label}: {why}"));
                continue;
            }
        };
        match prepare(&dir).and_then(|lexical| calibrate(&lexical).map(|()| lexical)) {
            Ok(lexical) => return lexical,
            Err(why) => rejected.push(format!("{label} ({}): {why}", dir.display())),
        }
    }
    panic!(
        "no location-neutral fixture base: every candidate lets its location decide the \
         verdict. Set {OVERRIDE_ENV} to a directory whose real path is outside the \
         blastguard temp roots and system directories (`/var`, `/private/var`, ...), \
         outside every worktree storage root, outside the workspace `target/`, and not \
         inside a git work tree under a build-output directory (ignored build output); \
         the calibration must come back Deny (outside) and Ask (inside the cwd).\n  {}",
        rejected.join("\n  ")
    );
}

/// Create this run's private physical directory and return its lexical
/// spelling.
fn prepare(dir: &Path) -> Result<PathBuf, String> {
    let physical_parent = dir.join(PARENT_NAME);
    std::fs::create_dir_all(&physical_parent).map_err(|e| format!("create: {e}"))?;
    let physical = physical_parent
        .canonicalize()
        .map_err(|e| format!("canonicalize: {e}"))?;
    prune_stale_runs(&physical);
    let run = create_run_dir(&physical)?;
    register_self_clean(physical.join(&run));
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

/// Is `path` a `run-*` directory that may be deleted: a real directory (not a
/// symlink, per `symlink_metadata`), named `run-*`, sitting DIRECTLY in a
/// canonical dedicated parent named [`PARENT_NAME`], whose own canonical path
/// is exactly `path`? Any doubt (an IO error included) is `None` = skip.
fn deletable_run_dir(path: &Path) -> Option<std::fs::Metadata> {
    let name = path.file_name()?.to_str()?;
    if !name.starts_with(RUN_PREFIX) {
        return None;
    }
    let parent = path.parent()?;
    if parent.file_name() != Some(OsStr::new(PARENT_NAME)) {
        return None;
    }
    if parent.canonicalize().ok()? != parent {
        return None;
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_dir() {
        return None;
    }
    if path.canonicalize().ok()? != path {
        return None;
    }
    Some(meta)
}

/// Remove stale `run-*` bases (module docs, PRUNE). `parent` is the canonical
/// dedicated directory. A base that cannot be inspected is left alone (it only
/// costs disk); a removal failure is reported on stderr.
pub fn prune_stale_runs(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = parent.join(entry.file_name());
        let Some(meta) = deletable_run_dir(&path) else {
            continue;
        };
        let stale = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > STALE_RUN);
        if stale {
            if let Err(e) = std::fs::remove_dir_all(&path) {
                report(&format!("prune of stale {} failed: {e}", path.display()));
            }
        }
    }
}

extern "C" {
    fn atexit(cb: extern "C" fn()) -> std::os::raw::c_int;
}

/// Record this run's physical dir and register the exit-time self-clean once.
fn register_self_clean(physical_run: PathBuf) {
    if OWN_RUN.set(physical_run).is_err() {
        return;
    }
    // SAFETY: `atexit` is the C library function with exactly this signature;
    // `self_clean` is a plain `extern "C" fn()` that never unwinds (it neither
    // panics nor calls anything that does: every IO result is matched).
    let rc = unsafe { atexit(self_clean) };
    if rc != 0 {
        report("atexit registration failed: this run's fixture dir will be left for the prune");
    }
}

/// Exit-time removal of this process's own run dir (module docs, SELF-CLEAN).
extern "C" fn self_clean() {
    let Some(run) = OWN_RUN.get() else {
        return;
    };
    clean_own_run(run);
}

/// The body of [`self_clean`], parameterised on the run dir so a regression
/// test can drive it against a seam dir (`fixture_base_prune_20261114.rs`).
pub fn clean_own_run(run: &Path) {
    if deletable_run_dir(run).is_none() {
        report(&format!(
            "self-clean skipped {}: no longer a plain run dir in the dedicated parent",
            run.display()
        ));
        return;
    }
    if let Err(e) = std::fs::remove_dir_all(run) {
        report(&format!("self-clean of {} failed: {e}", run.display()));
    }
}

/// Best-effort stderr note; never panics.
fn report(msg: &str) {
    let _ = writeln!(std::io::stderr(), "neutral_base: {msg}");
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
    // get different aliases; runs sharing a parent share the alias but each
    // lives in its own `run-*` directory below it.
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
        .any(|c| c.as_os_str() == OsStr::new(WORKTREE_DIR_NAME))
}

fn has_build_output_component(p: &Path) -> bool {
    p.components().any(|c| {
        blastguard::deletion::BUILD_OUTPUT_NAMES
            .iter()
            .any(|n| c.as_os_str() == OsStr::new(n))
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

/// The two calibration probes (module docs): outside the cwd exactly Deny,
/// inside the cwd exactly Ask.
fn calibrate(lexical: &Path) -> Result<(), String> {
    // Per test binary, so two binaries calibrating the same base never share it.
    let calib = lexical.join(concat!("calibration-", env!("CARGO_CRATE_NAME")));
    let _ = std::fs::remove_dir_all(&calib);
    let proj = calib.join("src/proj");
    let home = calib.join("home");
    let victim = calib.join("outside/victim");
    let inner = proj.join("inner");
    for d in [&proj, &home, &victim, &inner] {
        std::fs::create_dir_all(d).map_err(|e| format!("create {}: {e}", d.display()))?;
    }
    std::fs::write(victim.join("f"), "x").map_err(|e| format!("write: {e}"))?;
    std::fs::write(inner.join("f"), "x").map_err(|e| format!("write: {e}"))?;

    let outside = run_probe(&calib, &proj, &home, &victim)?;
    if !outside.contains(r#""permissionDecision":"deny""#) {
        return Err(if outside.trim().is_empty() {
            "calibration `rm -rf <outside>/victim` was Allow (an enclosing worktree root, or ignored build output in a git work tree?)".into()
        } else {
            format!(
                "calibration `rm -rf <outside>/victim` was not Deny (a temp safe root?): {}",
                outside.trim()
            )
        });
    }
    let inside = run_probe(&calib, &proj, &home, &inner)?;
    if !inside.contains(r#""permissionDecision":"ask""#) {
        return Err(if inside.trim().is_empty() {
            "calibration `rm -rf <cwd>/inner` was Allow, expected the confined Ask (an enclosing worktree root or allow class?)".into()
        } else {
            format!(
                "calibration `rm -rf <cwd>/inner` was not Ask (the location itself is refused, e.g. a system directory?): {}",
                inside.trim()
            )
        });
    }
    Ok(())
}

/// Run the hook binary on `rm -rf <target>` from cwd `proj`; returns stdout.
fn run_probe(calib: &Path, proj: &Path, home: &Path, target: &Path) -> Result<String, String> {
    let payload = serde_json::json!({
        "session_id": "worktree-root-calibration",
        "cwd": proj.display().to_string(),
        "tool_name": "Bash",
        "tool_input": { "command": format!("rm -rf {}", target.display()) },
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    // The probe's environment is PINNED, not inherited (module docs,
    // "Calibration env"): only PATH passes through; everything the hook reads
    // to pick its mode or its roots is set or absent here.
    hook.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        hook.env("PATH", path);
    }
    hook.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CLAUDE_PROJECT_DIR", proj)
        .env("BLASTGUARD_APPROVALS_DIR", calib.join("store"))
        .env("HOME", home)
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDECODE", "1")
        .current_dir(calib);
    // Bounded (support/bounded.rs): a spawn failure, a `try_wait` failure or
    // a hook that does not exit within the deadline PANICS here rather than
    // returning Err, so it is never mistaken for "this candidate base leaks".
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    if out.status.code() != Some(0) {
        return Err(format!(
            "calibration hook exited {:?}; stderr: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
