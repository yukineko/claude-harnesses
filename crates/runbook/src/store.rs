//! The procedure store: markdown files with optional TOML frontmatter (`+++`
//! fences, parsed with the `toml` crate). One file per procedure; the file stem
//! is the macro name (`deploy.md` → `!deploy`). Project procedures live in the
//! repo (`.runbook/` by default, so they are committed and shared); global ones
//! in `~/.runbook/runbooks/`.

use std::path::{Path, PathBuf};

use harness_core::verdict::{Determination, Reason};
use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Meta {
    /// One-line summary shown in `list` and the macro header.
    #[serde(default)]
    pub description: String,
    /// Extra names that also resolve to this procedure.
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Runbook {
    /// Macro name (file stem), already lowercased.
    pub name: String,
    pub path: PathBuf,
    pub global: bool,
    pub meta: Meta,
    pub body: String,
}

impl Runbook {
    /// True if `token` (already lowercased) names this runbook or an alias.
    pub fn matches(&self, token: &str) -> bool {
        self.name == token || self.meta.aliases.iter().any(|a| a.to_lowercase() == token)
    }
}

pub struct Store {
    pub project_dir: PathBuf,
    pub global_dir: PathBuf,
    pub include_global: bool,
}

/// Lowercase, filesystem-safe macro name. ASCII-leaning since macros are typed
/// inline (`!deploy-staging`); keeps `[a-z0-9_-]`.
pub fn normalize_name(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("runbook");
    }
    out.chars().take(48).collect()
}

impl Store {
    pub fn new(cfg: &Config, root: &Path) -> Self {
        Store {
            project_dir: cfg.project_runbook_dir(root),
            global_dir: cfg.global_dir.clone(),
            include_global: cfg.include_global,
        }
    }

    /// All procedures visible from `root`: project first, then global. Project
    /// names shadow global ones with the same name. `Undetermined` if either
    /// dir exists but could not be read — a partial read must not be reported
    /// as "these are all the runbooks there are".
    pub fn load_all(&self) -> Determination<Vec<Runbook>> {
        let mut out = match read_dir_runbooks(&self.project_dir, false) {
            Determination::Known(v) => v,
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        };
        if self.include_global {
            match read_dir_runbooks(&self.global_dir, true) {
                Determination::Known(g) => {
                    for r in g {
                        if !out.iter().any(|o| o.name == r.name) {
                            out.push(r);
                        }
                    }
                }
                Determination::Undetermined(why) => return Determination::Undetermined(why),
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Determination::Known(out)
    }

    pub fn dir_for(&self, global: bool) -> &Path {
        if global {
            &self.global_dir
        } else {
            &self.project_dir
        }
    }
}

/// A missing runbook dir is a legitimate absence (e.g. `runbook init` never
/// ran) and yields `Known(vec![])`; any OTHER `read_dir` error (permission
/// denied, IO) is a cannot-determine — it must not be silently read as "no
/// runbooks", so it comes back `Undetermined` instead.
fn read_dir_runbooks(dir: &Path, global: bool) -> Determination<Vec<Runbook>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Determination::Known(Vec::new())
        }
        Err(e) => {
            return Determination::Undetermined(Reason::new(format!("{}: {e}", dir.display())))
        }
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else {
            continue;
        };
        let (meta, body) = parse(&text);
        if body.is_empty() {
            continue;
        }
        out.push(Runbook {
            name: normalize_name(&stem),
            path,
            global,
            meta,
            body,
        });
    }
    Determination::Known(out)
}

/// Parse `+++ <toml> +++ <body>`. Body-only files get an empty meta.
pub fn parse(text: &str) -> (Meta, String) {
    let t = text.trim_start_matches('\u{feff}');
    if let Some(rest) = t.strip_prefix("+++") {
        if let Some(end) = rest.find("\n+++") {
            let fm = &rest[..end];
            let body = rest[end + 4..].trim_start_matches('\n');
            let meta: Meta = toml::from_str(fm.trim()).unwrap_or_default();
            return (meta, body.trim().to_string());
        }
    }
    (Meta::default(), t.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_basic() {
        assert_eq!(normalize_name("Deploy Staging"), "deploy-staging");
        assert_eq!(normalize_name("build_and_test"), "build_and_test");
        assert_eq!(normalize_name("  "), "runbook");
    }

    #[test]
    fn parse_frontmatter() {
        let (m, b) = parse("+++\ndescription = \"x\"\naliases = [\"d\"]\n+++\n\nbody here");
        assert_eq!(m.description, "x");
        assert_eq!(m.aliases, vec!["d"]);
        assert_eq!(b, "body here");
    }

    #[test]
    fn parse_body_only() {
        let (m, b) = parse("just a body");
        assert_eq!(m.description, "");
        assert_eq!(b, "just a body");
    }

    #[test]
    fn missing_runbook_dir_yields_known_empty_not_undetermined() {
        let result = read_dir_runbooks(Path::new("/no/such/runbook/store/dir/at/all"), false);
        assert!(matches!(result, Determination::Known(v) if v.is_empty()));
    }

    /// CA-runbook-store-01: a runbook dir that EXISTS but is unreadable
    /// (permission denied) must resolve to `Undetermined`, not the same
    /// `Known(vec![])` as a legitimately-absent dir — collapsing both into
    /// "no runbooks" makes an incomplete read look like a complete empty one.
    #[test]
    #[cfg(unix)]
    fn unreadable_existing_runbook_dir_is_undetermined_not_known_empty() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "runbook-store-unreadable-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&dir, perms.clone()).unwrap();

        let result = read_dir_runbooks(&dir, false);

        // Restore permissions so the temp dir can be cleaned up.
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(&dir, perms);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            matches!(result, Determination::Undetermined(_)),
            "an unreadable existing runbook dir must be Undetermined, got {result:?}"
        );
    }

    #[test]
    fn matches_name_and_alias() {
        let r = Runbook {
            name: "deploy".into(),
            path: PathBuf::new(),
            global: false,
            meta: Meta {
                description: String::new(),
                aliases: vec!["ship".into()],
            },
            body: "x".into(),
        };
        assert!(r.matches("deploy"));
        assert!(r.matches("ship"));
        assert!(!r.matches("build"));
    }
}
