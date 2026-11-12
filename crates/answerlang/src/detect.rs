//! Target-language resolution: explicit config, or auto detection
//! (1. user prompt script, 2. macOS UI language, 3. LC_ALL/LC_MESSAGES/LANG).

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::lang::Lang;
use crate::state::{Session, MACOS, PROMPT};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Known(Lang),
    /// An explicit configured language not in the table: injected verbatim,
    /// not enforceable.
    Unknown(String),
    /// Auto detection found nothing.
    Undetermined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Config,
    Prompt,
    Macos,
    Env,
    None,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Config => "config",
            Source::Prompt => "prompt",
            Source::Macos => "macos",
            Source::Env => "env",
            Source::None => "none",
        }
    }
}

/// Resolve the target. `prompt` is step 1's result (already known by the
/// caller: fresh from UserPromptSubmit or read back from the session state).
/// The macOS step runs only when step 1 did not determine the language.
pub fn resolve(
    cfg_language: &str,
    prompt: Option<Lang>,
    session: Option<&Session>,
) -> (Target, Source) {
    if cfg_language != "auto" {
        return match Lang::from_name_or_tag(cfg_language) {
            Some(l) => (Target::Known(l), Source::Config),
            None => (Target::Unknown(cfg_language.to_string()), Source::Config),
        };
    }
    if let Some(l) = prompt {
        return (Target::Known(l), Source::Prompt);
    }
    if let Some(l) = macos(session) {
        return (Target::Known(l), Source::Macos);
    }
    if let Some(l) = env_lang() {
        return (Target::Known(l), Source::Env);
    }
    (Target::Undetermined, Source::None)
}

/// Record step 1's result for this session (`none` when it did not determine).
pub fn record_prompt(session: &Session, lang: Option<Lang>) -> Result<(), String> {
    session.write(PROMPT, lang.map(Lang::name).unwrap_or("none"))
}

/// Read step 1's recorded result. `Ok(None)` = nothing recorded or recorded
/// `none`; `Err` = the record exists but cannot be read or is not one this
/// binary writes.
pub fn recorded_prompt(session: &Session) -> Result<Option<Lang>, String> {
    match session.read(PROMPT)? {
        None => Ok(None),
        Some(v) if v == "none" => Ok(None),
        Some(v) => match Lang::from_name_or_tag(&v) {
            Some(l) if l.name() == v => Ok(Some(l)),
            _ => Err(format!(
                "per-session prompt-language record has unrecognised content {v:?}"
            )),
        },
    }
}

/// Step 3: first non-empty of LC_ALL, LC_MESSAGES, LANG. `C`, `POSIX`,
/// `C.UTF-8` and other values without a known primary subtag do not
/// determine (and do not fall through to the next variable: the first
/// non-empty one is the effective locale).
pub fn env_lang() -> Option<Lang> {
    for k in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(k) {
            let v = v.trim();
            if !v.is_empty() {
                return Lang::from_name_or_tag(v);
            }
        }
    }
    None
}

/// Step 2: macOS UI language, at most once per session (the result, including
/// "not determined", is cached in the session state). Without a usable session
/// id there is nothing to key the cache on and the query runs uncached.
fn macos(session: Option<&Session>) -> Option<Lang> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if let Some(s) = session {
        match s.read(MACOS) {
            Ok(Some(v)) if v == "none" => return None,
            Ok(Some(v)) => return Lang::from_name_or_tag(&v),
            Ok(None) => {}
            Err(e) => eprintln!("answerlang: {e}; querying the macOS UI language again"),
        }
    }
    let lang = query_defaults();
    if let Some(s) = session {
        if let Err(e) = s.write(MACOS, lang.map(Lang::name).unwrap_or("none")) {
            eprintln!("answerlang: macOS language not cached ({e})");
        }
    }
    lang
}

const DEFAULTS_TIMEOUT: Duration = Duration::from_secs(3);

/// `defaults read -g AppleLanguages` (program overridable through
/// `ANSWERLANG_DEFAULTS_CMD`). Spawn failure, timeout, non-zero exit or
/// unparseable output → `None` (the step did not determine).
fn query_defaults() -> Option<Lang> {
    let prog = std::env::var("ANSWERLANG_DEFAULTS_CMD")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/usr/bin/defaults".to_string());
    let mut child = match Command::new(&prog)
        .args(["read", "-g", "AppleLanguages"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("answerlang: could not run {prog}: {e}");
            return None;
        }
    };
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() < DEFAULTS_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Ok(None) => {
                if let Err(e) = child.kill() {
                    eprintln!("answerlang: could not kill timed-out {prog}: {e}");
                }
                if let Err(e) = child.wait() {
                    eprintln!("answerlang: could not reap {prog}: {e}");
                }
                eprintln!("answerlang: {prog} timed out; macOS UI language not determined");
                return None;
            }
            Err(e) => {
                eprintln!("answerlang: waiting for {prog} failed: {e}");
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    let mut stdout = child.stdout.take()?;
    if let Err(e) = stdout.read_to_string(&mut out) {
        eprintln!("answerlang: could not read {prog} output: {e}");
        return None;
    }
    parse_apple_languages(&out).and_then(|tag| Lang::from_name_or_tag(&tag))
}

/// First entry of a plist array like `(\n    "ja-JP",\n    "en-US"\n)`.
fn parse_apple_languages(out: &str) -> Option<String> {
    let inner = out.trim().strip_prefix('(')?.strip_suffix(')')?;
    let first = inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim())
        .find(|s| !s.is_empty())?;
    first
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        .then(|| first.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plist_array() {
        assert_eq!(
            parse_apple_languages("(\n    \"ja-JP\",\n    \"en-US\"\n)\n").as_deref(),
            Some("ja-JP")
        );
        assert_eq!(
            parse_apple_languages("(\n    en,\n    ja\n)").as_deref(),
            Some("en")
        );
        assert_eq!(parse_apple_languages("garbage"), None);
        assert_eq!(parse_apple_languages("()"), None);
    }
}
