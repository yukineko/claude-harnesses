//! Process context: environment seams (CONTRACT §1), `~/.hostaudit/config.toml`
//! thresholds with clamps (canon §7, CONTRACT §3).

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const SSH_TIMEOUT_DEFAULT: i64 = 10;
pub const SSH_TIMEOUT_MIN: i64 = 1;
pub const SSH_TIMEOUT_MAX: i64 = 60;
pub const OSV_AGE_DEFAULT_H: i64 = 24;
pub const OSV_AGE_MIN_H: i64 = 1;
pub const OSV_AGE_MAX_H: i64 = 168;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    ssh_connect_timeout_secs: Option<i64>,
    osv_cache_max_age_hours: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Ctx {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub now: i64,
    pub ssh_bin: PathBuf,
    pub aws_bin: PathBuf,
    pub osv_url: Option<String>,
    pub tls_probe: Option<PathBuf>,
    pub ssh_connect_timeout: i64,
    pub osv_max_age_secs: i64,
}

impl Ctx {
    /// `~/.hostaudit`
    pub fn state_dir(&self) -> PathBuf {
        self.home.join(".hostaudit")
    }
}

/// Build the context. `Err` = usage error (exit 2): unparseable
/// `HOSTAUDIT_NOW`, no HOME, unreadable/unparseable config.
pub fn load() -> Result<Ctx, String> {
    let home = match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => dirs::home_dir().ok_or("cannot determine the home directory (HOME unset)")?,
    };
    let cwd = std::env::current_dir().map_err(|e| format!("cannot read the cwd: {e}"))?;
    let now = match std::env::var("HOSTAUDIT_NOW") {
        Ok(s) => s
            .trim()
            .parse::<i64>()
            .map_err(|e| format!("HOSTAUDIT_NOW={s:?} is not unix seconds: {e}"))?,
        Err(std::env::VarError::NotPresent) => chrono::Utc::now().timestamp(),
        Err(e) => return Err(format!("HOSTAUDIT_NOW unreadable: {e}")),
    };
    let fc = read_file_config(&home.join(".hostaudit").join("config.toml"))?;
    let ssh_connect_timeout = fc
        .ssh_connect_timeout_secs
        .unwrap_or(SSH_TIMEOUT_DEFAULT)
        .clamp(SSH_TIMEOUT_MIN, SSH_TIMEOUT_MAX);
    let osv_h = fc
        .osv_cache_max_age_hours
        .unwrap_or(OSV_AGE_DEFAULT_H)
        .clamp(OSV_AGE_MIN_H, OSV_AGE_MAX_H);
    let path_var = |k: &str, d: &str| -> PathBuf {
        match std::env::var_os(k) {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => PathBuf::from(d),
        }
    };
    Ok(Ctx {
        home,
        cwd,
        now,
        ssh_bin: path_var("HOSTAUDIT_SSH", "ssh"),
        aws_bin: path_var("HOSTAUDIT_AWS", "aws"),
        osv_url: std::env::var("HOSTAUDIT_OSV_URL")
            .ok()
            .filter(|s| !s.is_empty()),
        tls_probe: std::env::var_os("HOSTAUDIT_TLS_PROBE")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
        ssh_connect_timeout,
        osv_max_age_secs: osv_h * 3600,
    })
}

fn read_file_config(path: &Path) -> Result<FileConfig, String> {
    use harness_core::verdict::Required;
    match harness_core::boundary::read_to_string(path).require() {
        Required::Determined(None) => Ok(FileConfig::default()),
        Required::Determined(Some(text)) => toml::from_str::<FileConfig>(&text)
            .map_err(|e| format!("{} is not valid hostaudit config: {e}", path.display())),
        Required::Blocked(why) => Err(format!("{}: {why}", path.display())),
    }
}
