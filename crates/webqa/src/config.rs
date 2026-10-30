//! `webqa.toml` — the project's declaration of how to run Playwright and which
//! specs cover which web source files.
//!
//! Three answers, not two ([`Loaded`]): `Absent` (no file: the project never
//! opted in, so the gate has nothing to judge), `Config`, and `Undetermined`
//! (the file exists but cannot be read or parsed, or it is semantically
//! unusable). `Undetermined` is NOT `Absent`: a config that cannot be read is
//! a refusal to judge and blocks.

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const FILE_NAME: &str = "webqa.toml";

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct MapEntry {
    /// Globs (relative to the project root) of web source files.
    pub sources: Vec<String>,
    /// Playwright spec files (relative paths, no globs) that cover them.
    pub specs: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Runner argv, e.g. `["npx", "playwright", "test"]`. `--reporter=json`
    /// and the selected spec paths are appended.
    pub command: Vec<String>,
    #[serde(default, rename = "map")]
    pub maps: Vec<MapEntry>,
    /// Consecutive blocked stops before the bounded give-up.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Idle seconds after which the attempt counter restarts.
    #[serde(default = "default_reset_after_secs")]
    pub reset_after_secs: i64,
    /// Runner wall-clock limit; exceeding it is undetermined.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_max_attempts() -> u32 {
    3
}
fn default_reset_after_secs() -> i64 {
    1800
}
fn default_timeout_secs() -> u64 {
    540
}

/// Ceiling for `timeout_secs`: below the 600 s Stop-hook timeout in
/// `hooks/hooks.json`, leaving time to parse and report.
pub const MAX_TIMEOUT_SECS: u64 = 540;

pub enum Loaded {
    Absent,
    /// The parsed config and the exact bytes it was parsed from (the trust
    /// check hashes these same bytes, so the approved content and the
    /// executed content cannot diverge between two reads).
    Config(Box<Config>, Vec<u8>),
    Undetermined(String),
}

pub fn path_for(root: &Path) -> PathBuf {
    root.join(FILE_NAME)
}

pub fn load(root: &Path) -> Loaded {
    let p = path_for(root);
    // Read once: these bytes are both parsed and hashed for trust.
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Absent,
        Err(e) => return Loaded::Undetermined(format!("cannot read {}: {e}", p.display())),
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(t) => t,
        Err(e) => return Loaded::Undetermined(format!("{} is not UTF-8: {e}", p.display())),
    };
    match parse(text, &p) {
        Ok(c) => Loaded::Config(Box::new(c), bytes),
        Err(why) => Loaded::Undetermined(why),
    }
}

fn parse(text: &str, p: &Path) -> Result<Config, String> {
    let cfg: Config = match toml::from_str(text) {
        Ok(c) => c,
        Err(e) => {
            return Err(format!("cannot parse {}: {e}", p.display()));
        }
    };
    if cfg.command.is_empty() || cfg.command[0].trim().is_empty() {
        return Err(format!("{}: `command` is empty", p.display()));
    }
    if cfg.maps.is_empty() {
        return Err(format!(
            "{}: no [[map]] declared, so no touched file can be tied to a spec",
            p.display()
        ));
    }
    if cfg.timeout_secs > MAX_TIMEOUT_SECS {
        return Err(format!(
            "{}: `timeout_secs` = {} exceeds {MAX_TIMEOUT_SECS} (the Stop hook itself is killed \
             at 600s, so a longer runner limit could never be observed)",
            p.display(),
            cfg.timeout_secs
        ));
    }
    if cfg.max_attempts == 0 {
        return Err(format!(
            "{}: `max_attempts` must be at least 1 (0 would disable the gate)",
            p.display()
        ));
    }
    Ok(cfg)
}
