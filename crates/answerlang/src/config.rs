//! `~/.answerlang/config.toml` — loading (fail-soft to defaults, loudly) and
//! the atomic merge-write used by `answerlang set`.

use std::path::PathBuf;

pub const DEFAULT_MIN_SHARE: f64 = 0.3;
pub const MIN_SHARE_FLOOR: f64 = 0.05;
pub const MIN_SHARE_CEIL: f64 = 0.95;

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    pub inject: bool,
    pub enforce: bool,
    /// Lowercased: `auto`, a canonical name, a tag, or an unknown name.
    pub language: String,
    /// Effective (clamped) share.
    pub min_share: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            inject: true,
            enforce: true,
            language: "auto".to_string(),
            min_share: DEFAULT_MIN_SHARE,
        }
    }
}

#[derive(serde::Deserialize, Default)]
struct Raw {
    enabled: Option<bool>,
    inject: Option<bool>,
    enforce: Option<bool>,
    language: Option<String>,
    min_share: Option<f64>,
}

pub struct Loaded {
    pub config: Config,
    /// The file exists (even if it could not be read or parsed). Absent means
    /// first run: the ask-once directive is injected and Stop never blocks.
    pub present: bool,
    pub path: PathBuf,
}

pub fn dir() -> PathBuf {
    harness_core::config::base_dir("answerlang")
}

pub fn path() -> PathBuf {
    dir().join("config.toml")
}

/// Clamp into [`MIN_SHARE_FLOOR`, `MIN_SHARE_CEIL`]. A non-finite value is
/// replaced by the default (with a stderr note): an unclamped NaN would make
/// every `share < min_share` comparison false, i.e. always pass.
pub fn clamp_min_share(v: f64) -> f64 {
    if !v.is_finite() {
        eprintln!("answerlang: min_share {v} is not a finite number; using {DEFAULT_MIN_SHARE}");
        return DEFAULT_MIN_SHARE;
    }
    v.clamp(MIN_SHARE_FLOOR, MIN_SHARE_CEIL)
}

/// Load the config. Never fails: an absent file is the first-run state; an
/// unreadable or unparseable file is reported on stderr and the defaults
/// (everything enabled) are used, and it still counts as present so
/// enforcement applies.
pub fn load() -> Loaded {
    let path = path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                config: Config::default(),
                present: false,
                path,
            }
        }
        Err(e) => {
            eprintln!(
                "answerlang: config {} could not be read ({e}); using defaults (enabled, inject, enforce, language = auto)",
                path.display()
            );
            return Loaded {
                config: Config::default(),
                present: true,
                path,
            };
        }
    };
    let raw: Raw = match toml::from_str(&text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "answerlang: config {} could not be parsed ({e}); using defaults (enabled, inject, enforce, language = auto)",
                path.display()
            );
            return Loaded {
                config: Config::default(),
                present: true,
                path,
            };
        }
    };
    let d = Config::default();
    let language = match raw.language.map(|l| l.trim().to_lowercase()) {
        Some(l) if !l.is_empty() => l,
        Some(_) => {
            eprintln!("answerlang: config language is empty; using auto");
            d.language
        }
        None => d.language,
    };
    Loaded {
        config: Config {
            enabled: raw.enabled.unwrap_or(d.enabled),
            inject: raw.inject.unwrap_or(d.inject),
            enforce: raw.enforce.unwrap_or(d.enforce),
            language,
            min_share: raw.min_share.map(clamp_min_share).unwrap_or(d.min_share),
        },
        present: true,
        path,
    }
}

/// One `set` change.
pub enum Change {
    Bool(&'static str, bool),
    Language(String),
}

/// Merge `changes` into the config file (created with its directory when
/// absent) and write it atomically: a temp file in the same directory, then
/// rename. A file that exists but cannot be read or parsed is an error — it is
/// not overwritten, because that would discard whatever the user wrote.
pub fn merge_write(changes: &[Change]) -> Result<PathBuf, String> {
    let path = path();
    let mut table: toml::Table = match std::fs::read_to_string(&path) {
        Ok(t) => toml::from_str(&t).map_err(|e| {
            format!(
                "existing config {} could not be parsed ({e}); fix or remove it first",
                path.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => {
            return Err(format!(
                "existing config {} could not be read: {e}",
                path.display()
            ))
        }
    };
    for c in changes {
        match c {
            Change::Bool(k, v) => {
                table.insert((*k).to_string(), toml::Value::Boolean(*v));
            }
            Change::Language(l) => {
                table.insert("language".to_string(), toml::Value::String(l.clone()));
            }
        }
    }
    let body = toml::to_string(&table).map_err(|e| format!("could not serialize config: {e}"))?;
    let parent = dir();
    std::fs::create_dir_all(&parent)
        .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    let tmp = parent.join(format!(".config.toml.tmp.{}", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, body.as_bytes()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("could not write {}: {e}", tmp.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "could not rename {} to {}: {e}",
            tmp.display(),
            path.display()
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_bounds_and_nan() {
        assert_eq!(clamp_min_share(0.0), MIN_SHARE_FLOOR);
        assert_eq!(clamp_min_share(1.0), MIN_SHARE_CEIL);
        assert_eq!(clamp_min_share(0.6), 0.6);
        assert_eq!(clamp_min_share(f64::NAN), DEFAULT_MIN_SHARE);
    }
}
