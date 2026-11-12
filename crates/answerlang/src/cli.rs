//! `answerlang set` / `answerlang show`.

use crate::config::{self, Change};
use crate::detect::{self, Target};

pub const SET_USAGE: &str =
    "usage: answerlang set [--language <name|tag|auto>] [--inject on|off] [--enforce on|off] [--enabled on|off]";

fn on_off(flag: &str, v: &str) -> Result<bool, String> {
    match v {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(format!("{flag} takes on|off, got {v:?}")),
    }
}

/// Returns the process exit code: 0 ok, 2 usage / invalid value (nothing
/// written), 1 write failure.
pub fn set(args: &[String]) -> i32 {
    let mut changes = Vec::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let Some(value) = it.next() else {
            eprintln!("answerlang set: {flag} needs a value\n{SET_USAGE}");
            return 2;
        };
        let change = match flag.as_str() {
            "--language" => {
                let l = value.trim().to_lowercase();
                if l.is_empty() {
                    Err("--language must not be empty".to_string())
                } else {
                    Ok(Change::Language(l))
                }
            }
            "--inject" => on_off(flag, value).map(|b| Change::Bool("inject", b)),
            "--enforce" => on_off(flag, value).map(|b| Change::Bool("enforce", b)),
            "--enabled" => on_off(flag, value).map(|b| Change::Bool("enabled", b)),
            other => Err(format!("unknown option {other:?}")),
        };
        match change {
            Ok(c) => changes.push(c),
            Err(e) => {
                eprintln!("answerlang set: {e}\n{SET_USAGE}");
                return 2;
            }
        }
    }
    if changes.is_empty() {
        eprintln!("answerlang set: at least one option is required\n{SET_USAGE}");
        return 2;
    }
    match config::merge_write(&changes) {
        Ok(p) => {
            println!("answerlang: wrote {}", p.display());
            0
        }
        Err(e) => {
            eprintln!("answerlang set: {e}");
            1
        }
    }
}

/// Print the effective settings. Prompt detection (step 1) has no input here,
/// so `language_source` is `config`, `macos`, `env` or `none`.
pub fn show() -> i32 {
    let loaded = config::load();
    let c = &loaded.config;
    let (target, source) = detect::resolve(&c.language, None, None);
    let effective = match &target {
        Target::Known(l) => l.name().to_string(),
        Target::Unknown(name) => name.clone(),
        Target::Undetermined => "none".to_string(),
    };
    println!("config_path = {}", loaded.path.display());
    println!("config_present = {}", loaded.present);
    println!("enabled = {}", c.enabled);
    println!("inject = {}", c.inject);
    println!("enforce = {}", c.enforce);
    println!("language = {}", c.language);
    println!("min_share = {}", c.min_share);
    println!("effective_language = {effective}");
    println!("language_source = {}", source.as_str());
    0
}
