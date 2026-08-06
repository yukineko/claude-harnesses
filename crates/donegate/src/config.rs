//! Configuration: project `donegate.toml` (preferred) layered over a home-level
//! `~/.donegate/config.toml`, over built-in defaults. Env overrides last.
//!
//! # Four answers, not two (backlog 3135ebb9)
//!
//! Loading used to return a bare [`Config`], so *every* reason for ending up with
//! zero checks collapsed into one output — `checks: 0`, which the gate reads as
//! "allow every stop". Measured, verbatim, in a session worktree of this repo:
//!
//! ```text
//! donegate: <wt>/donegate.toml provides commands but this project is not trusted; ignoring it.
//! checks:        0
//!   (none — the gate will allow every stop; run `donegate init`)
//! ```
//!
//! "There IS a declared check set and I am refusing to run it" was rendered
//! identically to "no check set was declared". That is a refusal to judge
//! emitted as a clean verdict — the empty-set fail-open CLAUDE.md 3 forbids, and
//! the same defect `condukt`'s `checks_verdict` had before commit `d30d9b00`
//! made it return `NoChecksDeclared` rather than `Passed`.
//!
//! [`Config::resolve`] therefore returns the config **and** a [`Declaration`]
//! saying what was actually observed about the declaration itself. The gate
//! branches on the `Declaration` before it branches on `checks.is_empty()`.
//!
//! Safe by default is preserved exactly where it is a real observation:
//! [`Declaration::Absent`] (no config file anywhere) still lets every stop
//! through, because a project that never opted in is not being refused
//! anything — see that variant's docs for the full argument.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;

// Re-exported so existing `crate::config::expand_tilde` call sites keep working.
pub use harness_core::config::expand_tilde;
use harness_core::trust;

/// One acceptance command, run as a subprocess on Stop.
#[derive(Debug, Clone, Deserialize)]
pub struct Check {
    /// Short label shown in the block reason (e.g. "test", "clippy").
    pub name: String,
    /// Shell command line; run via `sh -c` (Unix) / `cmd /C` (Windows).
    pub cmd: String,
    /// If set, the check runs only when a changed file (git diff vs HEAD +
    /// untracked) matches one of these globs. Absent ⇒ always run.
    #[serde(default)]
    pub when_changed: Option<Vec<String>>,
    /// Per-check timeout; falls back to `default_timeout_secs`.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// A failing optional check warns but never blocks the stop.
    #[serde(default)]
    pub optional: bool,
    /// Run the command in this subdir of the project root.
    #[serde(default)]
    pub workdir: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    /// After this many consecutive blocks in one session, give up and allow the
    /// stop (so a genuinely stuck agent isn't trapped forever).
    pub max_attempts: u32,
    pub default_timeout_secs: u64,
    /// How many trailing lines of a failing command's output to feed back.
    pub output_tail_lines: usize,
    /// A session's attempt counter resets if this many seconds pass between
    /// stops (a fresh turn after the user did other work).
    pub reset_after_secs: i64,
    pub state_dir: PathBuf,
    pub checks: Vec<Check>,
}

/// On-disk form; every field optional.
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    enabled: Option<bool>,
    max_attempts: Option<u32>,
    default_timeout_secs: Option<u64>,
    output_tail_lines: Option<usize>,
    reset_after_secs: Option<i64>,
    state_dir: Option<String>,
    #[serde(default)]
    check: Vec<Check>,
}

/// The `~/.donegate` base directory. Thin wrapper over the shared primitive.
pub fn base_dir() -> PathBuf {
    harness_core::config::base_dir("donegate")
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            max_attempts: 3,
            default_timeout_secs: 300,
            output_tail_lines: 40,
            reset_after_secs: 600,
            state_dir: base_dir().join("state"),
            checks: Vec::new(),
        }
    }
}

/// What donegate observed about the **declaration** of a check set — kept apart
/// from the check set itself so that a refusal and an absence, which used to
/// produce the identical `checks: 0`, can never be confused again.
///
/// Only [`Declaration::Absent`] and a [`Declaration::Loaded`] with zero
/// `[[check]]` may let a stop through; the other two are the gate saying it
/// could not judge, and resolve to the restricted side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declaration {
    /// Neither `<root>/donegate.toml` nor `~/.donegate/config.toml` exists.
    ///
    /// **This deliberately still allows the stop, and here is the argument.**
    /// donegate is installed once into `~/.claude/settings.json` and then fires
    /// in *every* project on the machine. "No config file" is a determined
    /// observation (`Absent`, not `Undetermined`): nothing was declared, so
    /// nothing is being refused and there is no check whose result is unknown.
    /// Blocking here would trap every unrelated repo on the machine at once, for
    /// a gate they never opted into — and, unlike the refusal below, the operator
    /// gets no actionable remedy beyond "uninstall donegate". The
    /// `condukt::verify` precedent (`NoChecksDeclared` is not `Passed`) is
    /// honored by keeping this a *distinct answer* that the reporting paths name
    /// explicitly, not by pretending checks passed — but the action attached to
    /// that answer, for an opt-in Stop gate, is allow.
    Absent,
    /// A config donegate was entitled to read, and did read, from this path.
    /// Its `[[check]]` list — possibly empty, which is a real observation that
    /// the operator declared no checks — is in the returned [`Config`].
    Loaded(PathBuf),
    /// `<root>/donegate.toml` exists but the root is **not trusted**, so
    /// donegate will not run the commands it declares.
    ///
    /// This is a REFUSAL TO JUDGE, not a verdict about the project: donegate
    /// cannot say whether the project's own acceptance checks are green, because
    /// it declined to run them. It resolves to the restricted side.
    ///
    /// The home config is **not** substituted for the refused check set. Judging
    /// a project green against criteria it did not declare would be a different
    /// (and quieter) way of making the refusal invisible; only the knobs
    /// (`enabled`, `max_attempts`, `state_dir`, timeouts) are still taken from
    /// the home config, which needs no trust.
    RefusedUntrusted { project_path: PathBuf },
    /// A config file exists and donegate was entitled to read it, but could not
    /// (IO error, or invalid TOML). Also an inability to judge: the file may
    /// declare ten required checks or none, and donegate does not know which.
    /// Previously this fell through to `checks: 0` — the same fail-open as the
    /// refusal, reachable by shipping a `donegate.toml` with a typo in it.
    Unreadable { path: PathBuf, why: String },
}

impl Declaration {
    /// True when donegate could not judge and must therefore not let the stop
    /// through. Named, and matched exhaustively, so a fifth answer would have to
    /// say which side it is on.
    #[must_use]
    pub fn is_refusal(&self) -> bool {
        match self {
            Declaration::Absent | Declaration::Loaded(_) => false,
            Declaration::RefusedUntrusted { .. } | Declaration::Unreadable { .. } => true,
        }
    }
}

impl Config {
    /// The project config path (`<root>/donegate.toml`).
    pub fn project_path(root: &Path) -> PathBuf {
        root.join("donegate.toml")
    }

    /// The home config path (`~/.donegate/config.toml`).
    pub fn home_path() -> PathBuf {
        base_dir().join("config.toml")
    }

    /// Resolve the config for a project root, **and** what was observed about
    /// the declaration ([`Declaration`]). Callers must branch on the declaration
    /// before they branch on `checks.is_empty()`; that ordering is the whole
    /// point of the return type.
    ///
    /// A project `donegate.toml` wins outright — but only when the project root
    /// is trusted, because a project `[[check]].cmd` is later run via `sh -c` and
    /// a hostile repository could otherwise ship arbitrary commands. Trust is
    /// resolved with [`harness_core::trust::resolve`], i.e. **including worktree
    /// inheritance**: a linked git worktree of a trusted repository is trusted,
    /// because its `donegate.toml` is that repository's content and could be
    /// checked out in the repository's own working tree with identical
    /// privileges. Without that, CLAUDE.md 8's mandate that all work happen in
    /// session worktrees would put every session on the untrusted side.
    pub fn resolve(root: &Path) -> (Self, Declaration) {
        let project = Config::project_path(root);
        let home = Config::home_path();

        if project.exists() && !trust::resolve(root).is_trusted() {
            warn_untrusted_once(&project);
            // Knobs only — never the home check set (see `RefusedUntrusted`).
            let (mut cfg, _home_declaration) = load_from(home.exists().then_some(home));
            cfg.checks.clear();
            return (
                cfg,
                Declaration::RefusedUntrusted {
                    project_path: project,
                },
            );
        }

        let chosen = if project.exists() {
            Some(project)
        } else if home.exists() {
            Some(home)
        } else {
            None
        };
        load_from(chosen)
    }

    /// Globally disabled via env.
    pub fn disabled_env() -> bool {
        std::env::var("DONEGATE_DISABLE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    }
}

/// Layer `path` (when there is one) over the built-in defaults, then sanitize.
///
/// A read or parse failure yields [`Declaration::Unreadable`] rather than the
/// old silent fall-through to a check-less default: a config we cannot read is
/// not a config that declares nothing.
fn load_from(path: Option<PathBuf>) -> (Config, Declaration) {
    let mut cfg = Config::default();
    let declaration = match path {
        None => Declaration::Absent,
        Some(path) => match std::fs::read_to_string(&path) {
            Err(e) => Declaration::Unreadable {
                path,
                why: e.to_string(),
            },
            Ok(text) => match toml::from_str::<FileConfig>(&text) {
                Err(e) => Declaration::Unreadable {
                    path,
                    why: e.to_string(),
                },
                Ok(fc) => {
                    apply(&mut cfg, fc);
                    Declaration::Loaded(path)
                }
            },
        },
    };
    sanitize(&mut cfg);
    (cfg, declaration)
}

/// Overlay the on-disk fields that were actually present.
fn apply(cfg: &mut Config, fc: FileConfig) {
    if let Some(v) = fc.enabled {
        cfg.enabled = v;
    }
    if let Some(v) = fc.max_attempts {
        cfg.max_attempts = v;
    }
    if let Some(v) = fc.default_timeout_secs {
        cfg.default_timeout_secs = v;
    }
    if let Some(v) = fc.output_tail_lines {
        cfg.output_tail_lines = v;
    }
    if let Some(v) = fc.reset_after_secs {
        cfg.reset_after_secs = v;
    }
    if let Some(v) = fc.state_dir {
        cfg.state_dir = expand_tilde(&v);
    }
    cfg.checks = fc.check;
}

/// Clamp nonsensical knobs, and drop `[[check]]` entries with no name or no
/// command. The clamps all have a floor that keeps the gate working (a
/// `max_attempts` of 0 would mean "give up before the first attempt", i.e.
/// always allow).
fn sanitize(cfg: &mut Config) {
    if cfg.max_attempts == 0 {
        cfg.max_attempts = 1;
    }
    if cfg.default_timeout_secs == 0 {
        cfg.default_timeout_secs = 300;
    }
    if cfg.output_tail_lines == 0 {
        cfg.output_tail_lines = 40;
    }
    cfg.checks
        .retain(|c| !c.name.trim().is_empty() && !c.cmd.trim().is_empty());
}

/// Emit a one-shot notice (per process) that a project config is being refused
/// because the project isn't trusted. Best effort — never panics. The *verdict*
/// no longer rides on this line: [`Declaration::RefusedUntrusted`] carries it,
/// and the gate blocks. This stays because a manual `donegate status` reader
/// benefits from seeing it on stderr too.
fn warn_untrusted_once(project_path: &Path) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "donegate: {} provides commands but this project is not trusted; refusing to run them \
         (the stop will be BLOCKED, not allowed). Run 'donegate trust' to enable.",
        project_path.display()
    );
}

/// Serializes every test in this crate that mutates the process-global
/// `$HOME` env var (workspace-trust lookups here, and — since main.rs's
/// `violation_emission_tests` also point `$HOME` at a scratch dir to isolate
/// `overwatch::store`'s home-relative storage root — the violation-emission
/// tests too). `cargo test` runs a crate's unit tests on multiple threads by
/// default; two such tests racing without a shared lock corrupts each
/// other's view of "which project is trusted" / "where does the store
/// live" (observed directly: adding the violation-emission tests without
/// wiring them to this lock made `project_config_is_gated_behind_workspace_trust`
/// fail intermittently under the default multi-threaded test runner).
#[cfg(test)]
pub(crate) static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    const PROJECT_TOML: &str = r#"
[[check]]
name = "proj"
cmd = "echo project"
"#;

    const HOME_TOML: &str = r#"
[[check]]
name = "homecheck"
cmd = "echo home"
"#;

    fn names(cfg: &Config) -> Vec<&str> {
        cfg.checks.iter().map(|c| c.name.as_str()).collect()
    }

    // Mutates process-global HOME / HARNESS_TRUST_ALL, so the whole trust-gate
    // scenario runs in a single serialized #[test].
    //
    // DELIBERATE CONTRACT CHANGE (backlog 3135ebb9), recorded here rather than
    // quietly edited: steps 2 and 4 used to assert that an untrusted project
    // "falls back to the home config" and runs *those* checks. That is no longer
    // the contract. Judging a project green against criteria it never declared
    // is a quieter version of the fail-open this change exists to remove, so an
    // untrusted project now yields `Declaration::RefusedUntrusted` with an empty
    // check set, and the gate blocks on it. The assertions below are stronger,
    // not weaker: they now pin *which* answer was produced, where the old ones
    // could not tell "refused" from "nothing declared".
    #[test]
    fn project_config_is_gated_behind_workspace_trust() {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::remove_var("HARNESS_TRUST_ALL");

        let root = proj.path();
        write(&Config::project_path(root), PROJECT_TOML);

        // 1. Untrusted project + no home config ⇒ a REFUSAL, distinguishable
        //    from "nothing was declared".
        let (cfg, decl) = Config::resolve(root);
        assert!(cfg.checks.is_empty());
        assert!(
            matches!(decl, Declaration::RefusedUntrusted { .. }),
            "an untrusted project config must be a refusal, not an absence; got {decl:?}"
        );
        assert!(decl.is_refusal());

        // 2. Untrusted project + a home config ⇒ still a refusal, and the home
        //    check set is NOT substituted for the refused one.
        write(&Config::home_path(), HOME_TOML);
        let (cfg, decl) = Config::resolve(root);
        assert!(
            matches!(decl, Declaration::RefusedUntrusted { .. }),
            "a home config must not convert a refusal into a verdict; got {decl:?}"
        );
        assert!(
            cfg.checks.is_empty(),
            "the home check set must not stand in for the project's refused one; got {:?}",
            names(&cfg)
        );

        // 3. Trust the project ⇒ project config wins outright (as before).
        trust::add(root).unwrap();
        let (cfg, decl) = Config::resolve(root);
        assert_eq!(names(&cfg), vec!["proj"]);
        assert!(matches!(decl, Declaration::Loaded(_)), "got {decl:?}");
        assert!(!decl.is_refusal());

        // 4. Removing trust reverts to the refusal.
        assert!(trust::remove(root).unwrap());
        let (cfg, decl) = Config::resolve(root);
        assert!(matches!(decl, Declaration::RefusedUntrusted { .. }));
        assert!(cfg.checks.is_empty());

        // 5. HARNESS_TRUST_ALL is the global escape hatch: project wins without
        //    an explicit trust entry.
        std::env::set_var("HARNESS_TRUST_ALL", "1");
        let (cfg, decl) = Config::resolve(root);
        assert_eq!(names(&cfg), vec!["proj"]);
        assert!(matches!(decl, Declaration::Loaded(_)));
        std::env::remove_var("HARNESS_TRUST_ALL");

        // 6. Home config needs no trust at all: a project with NO donegate.toml
        //    loads the home checks directly.
        let bare = tempfile::tempdir().unwrap();
        let (cfg, decl) = Config::resolve(bare.path());
        assert_eq!(names(&cfg), vec!["homecheck"]);
        assert!(matches!(decl, Declaration::Loaded(_)));

        // 7. No config file anywhere ⇒ Absent, which is an observation of
        //    absence and NOT a refusal (the one empty that may allow a stop).
        std::fs::remove_file(Config::home_path()).unwrap();
        let bare2 = tempfile::tempdir().unwrap();
        let (cfg, decl) = Config::resolve(bare2.path());
        assert!(cfg.checks.is_empty());
        assert_eq!(decl, Declaration::Absent);
        assert!(!decl.is_refusal());
    }

    /// A config we are entitled to read but cannot parse is an inability to
    /// judge, not a declaration of zero checks — the same fail-open as the
    /// untrust refusal, reachable with a one-character typo.
    #[test]
    fn unparseable_config_is_undetermined_not_an_empty_check_set() {
        let _guard = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::set_var("HARNESS_TRUST_ALL", "1"); // isolate the parse axis

        let root = proj.path();
        write(&Config::project_path(root), "[[check\nname = \"broken\"\n");
        let (cfg, decl) = Config::resolve(root);
        std::env::remove_var("HARNESS_TRUST_ALL");

        assert!(cfg.checks.is_empty());
        // `#![deny(clippy::panic)]` covers tests too, so assert the shape first and
        // then destructure -- an `if let` alone would pass vacuously on a mismatch.
        assert!(
            matches!(&decl, Declaration::Unreadable { .. }),
            "an invalid TOML config must be Unreadable; got {decl:?}"
        );
        if let Declaration::Unreadable { path, why } = &decl {
            assert!(path.ends_with("donegate.toml"), "got {path:?}");
            assert!(!why.is_empty(), "the reason must say why");
        }
        assert!(decl.is_refusal());
    }
}
