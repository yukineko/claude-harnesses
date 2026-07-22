//! The knowledge store: atomic markdown notes with TOML frontmatter (`+++`
//! fences, parsed with the `toml` crate — no extra YAML dep). Project notes live
//! under `<store>/<project>/` (cwd basename + a stable hash, so same-named dirs
//! don't collide); shared notes under `<store>/_global/`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use harness_core::verdict::{Determination, Reason};
use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Explicit high-weight trigger terms.
    #[serde(default)]
    pub triggers: Vec<String>,
    /// "project" or "global" (informational; location is authoritative).
    #[serde(default)]
    pub scope: String,
    /// Always inject regardless of relevance (core conventions).
    #[serde(default)]
    pub always: bool,
    #[serde(default)]
    pub created: String,
}

#[derive(Debug, Clone)]
pub struct Note {
    pub slug: String,
    /// Source file; kept for tooling/debugging even when unused by retrieval.
    #[allow(dead_code)]
    pub path: PathBuf,
    pub global: bool,
    pub meta: Meta,
    pub body: String,
}

impl Note {
    /// Roughly how many chars this note adds when injected.
    pub fn injected_len(&self) -> usize {
        self.meta.title.chars().count() + self.body.chars().count() + 8
    }
}

pub struct Store {
    pub store_dir: PathBuf,
    pub include_global: bool,
}

fn hash8(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:08x}", (h.finish() & 0xffff_ffff) as u32)
}

pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    // Unicode-aware: keep letters/digits of any script (so Japanese titles get a
    // readable, distinct slug instead of all collapsing to the fallback).
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_alphanumeric() {
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
        out.push_str("note");
    }
    out.chars().take(48).collect()
}

impl Store {
    pub fn new(cfg: &Config) -> Self {
        Store {
            store_dir: cfg.store_dir.clone(),
            include_global: cfg.include_global,
        }
    }

    /// `<store>/<basename>-<hash>` for a project root.
    pub fn project_dir(&self, root: &Path) -> PathBuf {
        let base = root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "root".to_string());
        let key = root.to_string_lossy();
        self.store_dir
            .join(format!("{}-{}", slugify(&base), hash8(&key)))
    }

    pub fn global_dir(&self) -> PathBuf {
        self.store_dir.join("_global")
    }

    /// All notes visible from `root`: project notes + (optionally) global
    /// notes. `Undetermined` if either store dir exists but could not be
    /// read — a partial read must not be reported as "these are all the
    /// notes there are".
    pub fn load_visible(&self, root: &Path) -> Determination<Vec<Note>> {
        let project = match read_dir_notes(&self.project_dir(root), false) {
            Determination::Known(n) => n,
            Determination::Undetermined(why) => return Determination::Undetermined(why),
        };
        let mut notes = project;
        if self.include_global {
            match read_dir_notes(&self.global_dir(), true) {
                Determination::Known(g) => notes.extend(g),
                Determination::Undetermined(why) => return Determination::Undetermined(why),
            }
        }
        Determination::Known(notes)
    }

    /// Write a note; returns its path. `global` chooses the store.
    pub fn write(
        &self,
        root: &Path,
        slug: &str,
        meta: &Meta,
        body: &str,
        global: bool,
    ) -> std::io::Result<PathBuf> {
        let dir = if global {
            self.global_dir()
        } else {
            self.project_dir(root)
        };
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{slug}.md"));
        std::fs::write(&path, render(meta, body))?;
        Ok(path)
    }

    /// Remove a note by slug (project first, then global). Returns the path.
    pub fn remove(&self, root: &Path, slug: &str) -> Option<PathBuf> {
        for dir in [self.project_dir(root), self.global_dir()] {
            let p = dir.join(format!("{slug}.md"));
            if p.exists() && std::fs::remove_file(&p).is_ok() {
                return Some(p);
            }
        }
        None
    }
}

/// A missing note dir is a legitimate absence (e.g. `playbook init` never
/// ran) and yields `Known(vec![])`; any OTHER `read_dir` error (permission
/// denied, IO) is a cannot-determine — it must not be silently read as "no
/// notes", so it comes back `Undetermined` instead.
fn read_dir_notes(dir: &Path, global: bool) -> Determination<Vec<Note>> {
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
        if let Some((meta, body)) = parse(&text) {
            let slug = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            out.push(Note {
                slug,
                path,
                global,
                meta,
                body,
            });
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Determination::Known(out)
}

/// Parse `+++ <toml> +++ <body>`. Body-only files get an empty meta.
fn parse(text: &str) -> Option<(Meta, String)> {
    let t = text.trim_start_matches('\u{feff}');
    if let Some(rest) = t.strip_prefix("+++") {
        if let Some(end) = rest.find("\n+++") {
            let fm = &rest[..end];
            let body = rest[end + 4..].trim_start_matches('\n').to_string();
            let meta: Meta = toml::from_str(fm.trim()).ok()?;
            return Some((meta, body.trim().to_string()));
        }
    }
    // No frontmatter: treat the whole file as body with a blank meta.
    Some((Meta::default(), t.trim().to_string()))
}

fn render(meta: &Meta, body: &str) -> String {
    let fm = toml::to_string(meta).unwrap_or_default();
    format!("+++\n{}+++\n\n{}\n", fm, body.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_note_dir_yields_known_empty_not_undetermined() {
        let result = read_dir_notes(Path::new("/no/such/playbook/store/dir/at/all"), false);
        assert!(matches!(result, Determination::Known(v) if v.is_empty()));
    }

    /// CA-playbook-store-01: a note dir that EXISTS but is unreadable
    /// (permission denied) must resolve to `Undetermined`, not the same
    /// `Known(vec![])` as a legitimately-absent dir — collapsing both into
    /// "no notes" makes an incomplete read look like a complete empty one.
    #[test]
    #[cfg(unix)]
    fn unreadable_existing_note_dir_is_undetermined_not_known_empty() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "playbook-store-unreadable-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&dir, perms.clone()).unwrap();

        let result = read_dir_notes(&dir, false);

        // Restore permissions so the temp dir can be cleaned up.
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(&dir, perms);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            matches!(result, Determination::Undetermined(_)),
            "an unreadable existing note dir must be Undetermined, got {result:?}"
        );
    }
}
