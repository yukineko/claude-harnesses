//! `sitecraft.toml` — the project's declaration of how to run its dev server,
//! which pages to check, where the build output lives, and which files are
//! web sources.
//!
//! Example (all keys flat at the top level):
//!
//! ```toml
//! command = ["npm", "run", "dev", "--", "--port", "5173", "--strictPort"]
//! port = 5173
//! ready_path = "/"            # default "/"
//! ready_timeout_secs = 60     # default 60, at most MAX_READY_TIMEOUT_SECS
//! pages = ["/", "/about"]
//! build_dir = "dist"
//! web_globs = ["src/**", "index.html", "public/**"]
//! max_attempts = 3            # default 3 (Stop-gate give-up budget)
//! reset_after_secs = 1800     # default 1800
//! ```
//!
//! Three answers, not two ([`Loaded`]): `Absent` (no file: the project never
//! opted in), `Config`, and `Undetermined` (the file exists but cannot be
//! read, is not UTF-8, does not parse, or is semantically unusable).
//! `Undetermined` is NOT `Absent`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The config file name, at the project root.
pub const FILE_NAME: &str = "sitecraft.toml";

/// Ceiling for `ready_timeout_secs` (keeps the dev-server wait well inside the
/// 600 s Stop-hook timeout).
pub const MAX_READY_TIMEOUT_SECS: u64 = 300;

/// The parsed `sitecraft.toml`. Unknown keys are a parse error.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Dev-server argv, e.g. `["npm", "run", "dev"]`. Spawned only after the
    /// config is trusted (see [`crate::trust`]). Must be non-empty with a
    /// non-blank program.
    pub command: Vec<String>,
    /// Fixed port the dev server listens on (non-zero). `serve start` refuses
    /// to spawn if it is already taken.
    pub port: u16,
    /// Path requested on `http://127.0.0.1:<port>` to decide the server is ready.
    #[serde(default = "default_ready_path")]
    pub ready_path: String,
    /// Deadline for the ready probe, in seconds (1..=[`MAX_READY_TIMEOUT_SECS`]).
    #[serde(default = "default_ready_timeout_secs")]
    pub ready_timeout_secs: u64,
    /// Same-origin paths `check pages` opens. Must be non-empty.
    pub pages: Vec<String>,
    /// Build output directory (relative to the root) scanned by
    /// `check security` for leaked secrets. Must be non-empty.
    pub build_dir: String,
    /// Globs (relative to the root) of web source files. The Stop gate runs
    /// `check pages` only when the session wrote a file matching one of them
    /// (or when that cannot be determined). Must be non-empty and every glob
    /// must compile.
    pub web_globs: Vec<String>,
    /// Consecutive blocked stops before the gate's bounded give-up.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Idle seconds after which the gate's attempt counter restarts.
    #[serde(default = "default_reset_after_secs")]
    pub reset_after_secs: i64,
}

fn default_ready_path() -> String {
    "/".to_string()
}
fn default_ready_timeout_secs() -> u64 {
    60
}
fn default_max_attempts() -> u32 {
    3
}
fn default_reset_after_secs() -> i64 {
    1800
}

/// Result of [`load`].
#[derive(Debug)]
pub enum Loaded {
    /// No `sitecraft.toml` at the root (`NotFound`). The project never opted in.
    Absent,
    /// The parsed, validated config and the exact bytes it was parsed from
    /// (the trust check hashes these same bytes: one read).
    Config(Box<Config>, Vec<u8>),
    /// The file exists but cannot be used. Carries why. Blocks.
    Undetermined(String),
}

/// `<root>/sitecraft.toml`.
#[must_use]
pub fn path_for(root: &Path) -> PathBuf {
    root.join(FILE_NAME)
}

/// Load and validate `<root>/sitecraft.toml`.
///
/// Contract: `NotFound` → [`Loaded::Absent`]; any other read error, non-UTF-8,
/// TOML syntax error, unknown key, or a failed validation (empty `command` or
/// blank program, `port` 0, empty `pages`, empty `build_dir`, empty or
/// uncompilable `web_globs`, `ready_timeout_secs` outside
/// 1..=[`MAX_READY_TIMEOUT_SECS`], `max_attempts` 0) → [`Loaded::Undetermined`];
/// otherwise [`Loaded::Config`]. No error maps to `Absent` or `Config`.
#[must_use]
pub fn load(root: &Path) -> Loaded {
    let p = path_for(root);
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Absent,
        Err(e) => return Loaded::Undetermined(format!("cannot read {}: {e}", p.display())),
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(t) => t,
        Err(e) => return Loaded::Undetermined(format!("{} is not valid UTF-8: {e}", p.display())),
    };
    let cfg: Config = match toml::from_str(text) {
        Ok(c) => c,
        Err(e) => return Loaded::Undetermined(format!("cannot parse {}: {e}", p.display())),
    };
    match validate(&cfg) {
        Ok(()) => Loaded::Config(Box::new(cfg), bytes),
        Err(why) => Loaded::Undetermined(format!("{} is unusable: {why}", p.display())),
    }
}

/// Semantic validation; `Err` carries why the config cannot be used.
fn validate(c: &Config) -> Result<(), String> {
    match c.command.first() {
        None => return Err("`command` is empty".into()),
        Some(prog) if prog.trim().is_empty() => return Err("`command` has a blank program".into()),
        Some(_) => {}
    }
    if c.port == 0 {
        return Err("`port` must be non-zero".into());
    }
    if c.pages.is_empty() {
        return Err("`pages` is empty".into());
    }
    if c.build_dir.trim().is_empty() {
        return Err("`build_dir` is empty".into());
    }
    if c.web_globs.is_empty() {
        return Err("`web_globs` is empty".into());
    }
    for g in &c.web_globs {
        if let Err(e) = globset::Glob::new(g) {
            return Err(format!("web glob {g:?} does not compile: {e}"));
        }
    }
    if c.ready_timeout_secs == 0 || c.ready_timeout_secs > MAX_READY_TIMEOUT_SECS {
        return Err(format!(
            "`ready_timeout_secs` must be 1..={MAX_READY_TIMEOUT_SECS}"
        ));
    }
    if c.max_attempts == 0 {
        return Err("`max_attempts` must be at least 1".into());
    }
    Ok(())
}
