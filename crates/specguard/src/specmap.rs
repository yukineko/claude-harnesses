//! Independent, reusable SPEC/feature ↔ implementation ↔ test ↔ API mapping
//! store.
//!
//! This module owns a *persisted*, **feature/endpoint-centric** mapping. Each
//! [`MapEntry`] relates a spec/feature (or an HTTP endpoint) to the artifacts
//! that realize, exercise, and consume it across two dimensions:
//!   * `impl_files` + `test_files` — a feature relates to BOTH its
//!     implementation and its tests. Reading the tests reveals which features
//!     exist; reading the API implementation reveals which file implements a
//!     feature.
//!   * `api` + `client_refs` — for `Endpoint` entries, the method/route pattern
//!     plus the client-side call sites. This lets a consumer walk
//!     client → endpoint → server code: from a client call site you find the
//!     URL, which maps to the server handler file.
//!
//! It is deliberately **independent of any drift workflow**: it knows nothing
//! about sentinels, reports, ratification, or the audit agent. Future features
//! (spec-audit, drift-map, coverage reports, …) each *consume* this store rather
//! than the store depending on them, and the entry type is general/extensible
//! (see [`EntryKind`]) so it serves full-stack projects, not just this repo.
//!
//! ## Division of labour (important)
//!
//! The *semantic* attribution — deciding which file, test, or endpoint truly
//! belongs to which feature, and resolving the api↔server-code and
//! client↔server links by actually READING the test code, the API/route
//! definitions, and the client HTTP calls — is the **LLM CONSUMER's job** (the
//! `/specguard:drift-map` command and future spec-audit). This store only
//! **PERSISTS** those associations and offers a **deterministic, path-based
//! sync** as the skeleton the consumer refines. The deterministic sync cannot
//! know feature/endpoint boundaries, so on its own it keys a newly-seen file by
//! its own path and classifies it into `impl_files`/`test_files` via the simple
//! documented heuristic [`classify_path`]; a consumer later merges those
//! per-file skeleton entries into real feature/endpoint entries (multiple files
//! under one key, an `api` ref, `client_refs`) and sets a `spec_doc`. Once a
//! consumer has done so, subsequent deterministic syncs find the owning entry by
//! path and update it in place.
//!
//! ## Layers (kept separate so derivation is unit-testable without git)
//!   * [`SpecMap`] — the TOML-persisted store: [`SpecMap::load`],
//!     [`SpecMap::load_or_init`], [`SpecMap::save`].
//!   * pure helpers — [`parse_name_status`] turns raw `git log --name-status`
//!     text into a `Vec<Change>`, [`classify_path`] decides impl-vs-test, and
//!     [`SpecMap::apply_changes`] reflects those changes into the map.
//!     [`SpecMap::sync`] is the thin git-invoking wrapper that glues them.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Default location of the persisted map, relative to the repo root.
pub const DEFAULT_MAP_PATH: &str = ".specguard/spec-map.toml";

/// The lifecycle status of a mapped feature/endpoint. Intentionally minimal and
/// workflow-agnostic — a consumer decides what to do with each state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The entry's spec-doc and its impl/test/client associations are in sync as
    /// far as the map knows (a state a consumer sets after reconciling).
    Tracked,
    /// A related file changed since it was last reconciled; the entry's spec-doc
    /// and associations may need review.
    Changed,
    /// The entry has no remaining implementation/test files (all deleted); its
    /// mapping is orphaned.
    Missing,
}

impl Status {
    /// Stable lowercase token (matches the serde representation).
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Tracked => "tracked",
            Status::Changed => "changed",
            Status::Missing => "missing",
        }
    }
}

fn default_status() -> Status {
    Status::Tracked
}

/// What a [`MapEntry`] represents. Extensible (add variants without breaking old
/// maps); serde default is `Feature` so a map that omits `kind` still parses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// A feature/module: related implementation + test files.
    #[default]
    Feature,
    /// An HTTP endpoint: additionally carries an `api` method/route and the
    /// `client_refs` that call it.
    Endpoint,
}

/// Which side of the impl/test dimension a changed path belongs to, per the
/// deterministic path heuristic ([`classify_path`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRole {
    /// Implementation / server-code file.
    Impl,
    /// Test file.
    Test,
}

/// An HTTP endpoint reference: the method + route (URL) pattern a client call
/// site resolves to and a server handler implements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiRef {
    /// HTTP method (e.g. `GET`, `POST`). Free-form; the consumer normalizes.
    pub method: String,
    /// Route/URL pattern (e.g. `/api/users/:id`).
    pub route: String,
}

/// One mapping: a feature or endpoint related to the implementation file(s),
/// test file(s), API route, and client call sites that realize / exercise /
/// consume it. Keyed in [`SpecMap::entries`] by [`MapEntry::key`] (a stable
/// feature/module name or endpoint id; the deterministic skeleton keys a
/// not-yet-attributed file by its own path).
///
/// Every optional/vec field carries a serde default, so an old or partial map
/// (missing newer fields) still parses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapEntry {
    /// Stable id: a feature/module name or an endpoint id (mirrors the map key).
    #[serde(default)]
    pub key: String,
    /// What this entry represents (feature vs endpoint).
    #[serde(default)]
    pub kind: EntryKind,
    /// Path to the spec document. `None`/empty means the spec is not yet
    /// authored (a consumer typically marks such an entry `Missing`).
    #[serde(default)]
    pub spec_doc: Option<String>,
    /// Lifecycle status of the mapping.
    #[serde(default = "default_status")]
    pub status: Status,
    /// Last git ref this entry was synced at (`None` until first synced).
    #[serde(default)]
    pub last_ref: Option<String>,
    /// Implementation / server-code file paths realizing this entry.
    #[serde(default)]
    pub impl_files: Vec<String>,
    /// Test file paths exercising this entry.
    #[serde(default)]
    pub test_files: Vec<String>,
    /// Client-side call sites (files) that call this entry's api/url.
    #[serde(default)]
    pub client_refs: Vec<String>,
    /// For `Endpoint` entries: the method/route this entry maps to. A sub-table,
    /// declared LAST so TOML emits it after all scalar/array fields.
    #[serde(default)]
    pub api: Option<ApiRef>,
}

impl MapEntry {
    /// A fresh path-keyed skeleton entry (kind `Feature`, no spec-doc yet),
    /// created by the deterministic sync for a not-yet-attributed file.
    fn skeleton(key: &str, status: Status, last_ref: Option<String>) -> MapEntry {
        MapEntry {
            key: key.to_string(),
            kind: EntryKind::default(),
            spec_doc: None,
            status,
            last_ref,
            impl_files: Vec::new(),
            test_files: Vec::new(),
            client_refs: Vec::new(),
            api: None,
        }
    }

    /// True when this entry references `path` in the impl or test vector.
    fn has_path(&self, path: &str) -> bool {
        self.impl_files.iter().any(|p| p == path) || self.test_files.iter().any(|p| p == path)
    }

    /// Remove `path` from the impl and test vectors.
    fn remove_path(&mut self, path: &str) {
        self.impl_files.retain(|p| p != path);
        self.test_files.retain(|p| p != path);
    }

    /// Add `path` to the vector for `role` (deduped, sorted for stable diffs),
    /// first removing it from the other vector so a re-classified path never
    /// appears twice.
    fn add_path(&mut self, path: &str, role: FileRole) {
        self.remove_path(path);
        let v = match role {
            FileRole::Impl => &mut self.impl_files,
            FileRole::Test => &mut self.test_files,
        };
        v.push(path.to_string());
        v.sort();
        v.dedup();
    }

    /// True when no implementation or test file remains attributed.
    fn is_orphaned(&self) -> bool {
        self.impl_files.is_empty() && self.test_files.is_empty()
    }
}

/// Pure: does `entry` match the (case-insensitive substring) `query`? An
/// empty/blank query matches every entry (the whole-map default). Otherwise the
/// query is matched against the entry key, its spec_doc, any impl/test file
/// path, and — for endpoint entries — the api route. This lets a consumer scope
/// an operation to a specific command/crate/API by path or route (e.g.
/// `drift-map`, `crates/specguard`, `/health`). No filesystem access.
///
/// This is the single source of truth for entry targeting, shared by
/// `specguard audit --filter` and `specguard map list --filter`.
pub fn entry_matches(entry: &MapEntry, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let hay = |s: &str| s.to_lowercase().contains(&q);
    hay(&entry.key)
        || entry.spec_doc.as_deref().is_some_and(hay)
        || entry.impl_files.iter().any(|p| hay(p))
        || entry.test_files.iter().any(|p| hay(p))
        || entry.api.as_ref().is_some_and(|a| hay(&a.route))
}

/// The persisted feature/endpoint map. Keyed by entry id, kept in a `BTreeMap`
/// so serialization is deterministic (stable diffs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecMap {
    /// Git ref the whole map was last synced up to (advisory; each entry also
    /// carries its own `last_ref`). Declared before `entries` so TOML emits this
    /// scalar before the `[entries.*]` tables.
    #[serde(default)]
    pub last_synced: String,
    /// entry id → mapping.
    #[serde(default)]
    pub entries: BTreeMap<String, MapEntry>,
}

/// A single file change parsed from `git log --name-status`. This is the pure
/// input the map reflection logic consumes, so the derivation is testable
/// without shelling out to git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// `A` — file added.
    Added(String),
    /// `M`/`T` — file modified (content or type changed).
    Modified(String),
    /// `R`/`C` — file renamed/copied from → to.
    Renamed { from: String, to: String },
    /// `D` — file deleted.
    Deleted(String),
}

/// Classify a repo-relative path as an implementation or a test file using a
/// simple, documented, purely path-based heuristic (the deterministic
/// skeleton; the LLM consumer refines true attribution):
///   * any path component named `tests` or `test` → [`FileRole::Test`];
///   * a `*.test.*` filename (e.g. `foo.test.ts`), or a file stem starting with
///     `test_`, ending with `_test`, or containing `_test_` → [`FileRole::Test`];
///   * otherwise → [`FileRole::Impl`].
pub fn classify_path(path: &str) -> FileRole {
    let norm = path.replace('\\', "/");
    if norm.split('/').any(|seg| seg == "tests" || seg == "test") {
        return FileRole::Test;
    }
    let file = norm.rsplit('/').next().unwrap_or(&norm);
    if file.contains(".test.") {
        return FileRole::Test;
    }
    let stem = file.split('.').next().unwrap_or(file);
    if stem.starts_with("test_") || stem.ends_with("_test") || stem.contains("_test_") {
        return FileRole::Test;
    }
    FileRole::Impl
}

/// True when `code` is a git `--name-status` status token: a leading status
/// letter (`A`/`M`/`D`/`R`/`C`/`T`) optionally followed by a similarity score
/// (e.g. `R100`). Checked against the *raw* first field (no leading-whitespace
/// trim) so indented commit-message lines never masquerade as change lines.
fn is_status_code(code: &str) -> bool {
    let mut chars = code.chars();
    match chars.next() {
        Some('A' | 'M' | 'D' | 'R' | 'C' | 'T') => chars.all(|c| c.is_ascii_digit()),
        _ => false,
    }
}

/// Parse raw `git log --name-status` output into a flat list of [`Change`]s
/// (in the order they appear — newest commit first as git emits them). Commit
/// headers, author/date lines and indented message bodies are ignored: only
/// tab-separated `<status>\t<path...>` lines are recognized. Pure — no I/O.
pub fn parse_name_status(text: &str) -> Vec<Change> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let Some(code) = fields.next() else {
            continue;
        };
        if !is_status_code(code) {
            continue;
        }
        let first = code.chars().next().unwrap();
        match first {
            'A' => {
                if let Some(p) = fields.next() {
                    out.push(Change::Added(p.trim().to_string()));
                }
            }
            'M' | 'T' => {
                if let Some(p) = fields.next() {
                    out.push(Change::Modified(p.trim().to_string()));
                }
            }
            'D' => {
                if let Some(p) = fields.next() {
                    out.push(Change::Deleted(p.trim().to_string()));
                }
            }
            'R' | 'C' => {
                if let (Some(from), Some(to)) = (fields.next(), fields.next()) {
                    out.push(Change::Renamed {
                        from: from.trim().to_string(),
                        to: to.trim().to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

impl SpecMap {
    /// Load the map from `path`. A missing file yields an empty (default) map —
    /// callers get a usable store on first use without special-casing. A present
    /// but unparseable file is a hard error (don't silently discard state).
    /// Backfills each entry's `key` from its map key if the persisted entry
    /// omitted it (older/partial maps).
    pub fn load(path: &Path) -> Result<SpecMap> {
        let mut map: SpecMap = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("parsing spec map {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => SpecMap::default(),
            Err(e) => {
                return Err(anyhow::Error::new(e))
                    .with_context(|| format!("reading spec map {}", path.display()))
            }
        };
        for (k, entry) in map.entries.iter_mut() {
            if entry.key.is_empty() {
                entry.key = k.clone();
            }
        }
        Ok(map)
    }

    /// Load the map, creating an empty one on disk if the file is absent
    /// (create-if-absent). Returns the loaded (or freshly created) map.
    pub fn load_or_init(path: &Path) -> Result<SpecMap> {
        if !path.exists() {
            let empty = SpecMap::default();
            empty.save(path)?;
            return Ok(empty);
        }
        SpecMap::load(path)
    }

    /// Persist the map to `path` as TOML, creating parent directories as needed.
    /// Deterministic output (BTreeMap key + sorted vectors) for clean diffs.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating spec map dir {}", dir.display()))?;
            }
        }
        let body = toml::to_string(self).context("serializing spec map")?;
        std::fs::write(path, body)
            .with_context(|| format!("writing spec map {}", path.display()))?;
        Ok(())
    }

    /// Number of mapped entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the map has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The key of the entry that already references `path` (in its impl or test
    /// vector), if any. Lets a consumer-authored feature/endpoint entry (multiple
    /// files under one key) claim a changed path instead of the skeleton spawning
    /// a per-file entry.
    fn key_owning(&self, path: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|(_, e)| e.has_path(path))
            .map(|(k, _)| k.clone())
    }

    /// Reflect a batch of parsed [`Change`]s into the map (pure — no I/O) using
    /// the deterministic path skeleton. `spec_dir` is available for a consumer's
    /// derivation conventions; `synced_ref` stamps every touched entry's
    /// `last_ref`. Behaviour:
    ///   * `A`/`M`/`T` (added/modified) → attribute the path via
    ///     [`classify_path`]. If an existing entry already owns it, add it to the
    ///     correct vector there and mark that entry `Changed`. Otherwise create a
    ///     new skeleton entry keyed by the path itself (a consumer merges it into
    ///     a real feature/endpoint later), status `Changed`.
    ///   * `R`/`C` (renamed) → detach the old path from its owning entry and
    ///     attribute the new path.
    ///   * `D` (deleted) → detach the path from its owning entry; if that leaves
    ///     the entry with no impl/test files it is marked `Missing`, else
    ///     `Changed`. A delete of an unmapped file is a no-op.
    pub fn apply_changes(&mut self, changes: &[Change], _spec_dir: &str, synced_ref: &str) {
        for change in changes {
            match change {
                Change::Added(p) | Change::Modified(p) => self.attribute_path(p, synced_ref),
                Change::Renamed { from, to } => {
                    self.detach_path(from, synced_ref);
                    self.attribute_path(to, synced_ref);
                }
                Change::Deleted(p) => self.detach_path(p, synced_ref),
            }
        }
        if !synced_ref.is_empty() {
            self.last_synced = synced_ref.to_string();
        }
    }

    /// Attribute a changed/added path into the map: into its owning entry if one
    /// exists, else into a fresh path-keyed skeleton entry. Marks the entry
    /// `Changed`.
    fn attribute_path(&mut self, path: &str, synced_ref: &str) {
        let role = classify_path(path);
        let last_ref = ref_opt(synced_ref);
        let key = self.key_owning(path).unwrap_or_else(|| path.to_string());
        let entry = self
            .entries
            .entry(key.clone())
            .or_insert_with(|| MapEntry::skeleton(&key, Status::Changed, last_ref.clone()));
        entry.add_path(path, role);
        entry.status = Status::Changed;
        entry.last_ref = last_ref;
    }

    /// Detach a deleted/renamed-away path from its owning entry. The entry is
    /// marked `Missing` if it now has no impl/test files, else `Changed`.
    fn detach_path(&mut self, path: &str, synced_ref: &str) {
        let Some(key) = self.key_owning(path) else {
            return;
        };
        let entry = self
            .entries
            .get_mut(&key)
            .expect("key_owning returned a live key");
        entry.remove_path(path);
        entry.last_ref = ref_opt(synced_ref);
        entry.status = if entry.is_orphaned() {
            Status::Missing
        } else {
            Status::Changed
        };
    }

    /// Reconcile the map against `git log --name-status <baseline>..HEAD`, run
    /// from `repo_root`. Parses the name-status output (via [`parse_name_status`])
    /// and reflects it (via [`apply_changes`]). `synced_ref` is stamped onto every
    /// touched entry (typically the current HEAD). The git invocation is the only
    /// impure part; the derivation is delegated to the pure helpers above.
    ///
    /// [`apply_changes`]: SpecMap::apply_changes
    pub fn sync(
        &mut self,
        repo_root: &Path,
        baseline: &str,
        spec_dir: &str,
        synced_ref: &str,
    ) -> Result<()> {
        let text = git_log_name_status(repo_root, baseline)?;
        let changes = parse_name_status(&text);
        self.apply_changes(&changes, spec_dir, synced_ref);
        Ok(())
    }
}

/// `Some(ref)` for a non-empty git ref, else `None` (used for `last_ref`).
fn ref_opt(r: &str) -> Option<String> {
    if r.is_empty() {
        None
    } else {
        Some(r.to_string())
    }
}

/// Run `git log --name-status <baseline>..HEAD` from `repo_root` and return its
/// raw stdout. The `baseline` is validated with the same safe-ref guard the
/// scope resolver uses, so a hostile ref can never reach git.
fn git_log_name_status(repo_root: &Path, baseline: &str) -> Result<String> {
    if !crate::scope::is_safe_ref(baseline) {
        anyhow::bail!(
            "refusing unsafe baseline ref '{baseline}': only [A-Za-z0-9_./~^-] are allowed and it must not start with '-'"
        );
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("log")
        .arg("--name-status")
        .arg("--no-color")
        .arg(format!("{baseline}..HEAD"))
        .output()
        .context("spawning git log")?;
    if !out.status.success() {
        anyhow::bail!(
            "git log --name-status {baseline}..HEAD failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_absent_returns_empty_default() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".specguard/spec-map.toml");
        // File does not exist yet.
        let map = SpecMap::load(&path).unwrap();
        assert!(map.is_empty());
        assert_eq!(map.last_synced, "");
        // load() must not have created the file.
        assert!(!path.exists());
    }

    #[test]
    fn load_or_init_creates_file_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".specguard/spec-map.toml");
        let map = SpecMap::load_or_init(&path).unwrap();
        assert!(map.is_empty());
        assert!(path.exists(), "load_or_init should create the file");
        // Reloading the created file yields the same empty map.
        assert_eq!(SpecMap::load(&path).unwrap(), map);
    }

    #[test]
    fn save_then_load_round_trips_endpoint_entry_with_api_and_client_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/spec-map.toml");
        let mut map = SpecMap::default();
        // A consumer-authored ENDPOINT entry exercising every field, to prove the
        // api + client_refs dimensions serialize and round-trip.
        map.entries.insert(
            "GET /api/users/:id".to_string(),
            MapEntry {
                key: "GET /api/users/:id".to_string(),
                kind: EntryKind::Endpoint,
                spec_doc: Some("docs/specs/users.md".to_string()),
                status: Status::Tracked,
                last_ref: Some("cafef00d".to_string()),
                impl_files: vec!["src/server/users.rs".to_string()],
                test_files: vec!["tests/users_test.rs".to_string()],
                client_refs: vec!["web/api/users.ts".to_string()],
                api: Some(ApiRef {
                    method: "GET".to_string(),
                    route: "/api/users/:id".to_string(),
                }),
            },
        );
        map.last_synced = "cafef00d".to_string();
        map.save(&path).unwrap();

        let loaded = SpecMap::load(&path).unwrap();
        assert_eq!(loaded, map);
        let e = &loaded.entries["GET /api/users/:id"];
        assert_eq!(e.kind, EntryKind::Endpoint);
        assert_eq!(e.spec_doc.as_deref(), Some("docs/specs/users.md"));
        assert_eq!(e.impl_files, vec!["src/server/users.rs".to_string()]);
        assert_eq!(e.test_files, vec!["tests/users_test.rs".to_string()]);
        assert_eq!(e.client_refs, vec!["web/api/users.ts".to_string()]);
        assert_eq!(
            e.api,
            Some(ApiRef {
                method: "GET".to_string(),
                route: "/api/users/:id".to_string()
            })
        );
        assert_eq!(e.last_ref.as_deref(), Some("cafef00d"));
    }

    #[test]
    fn partial_map_still_parses_and_backfills_key() {
        // An older/partial map that predates the newer fields (no kind, no
        // api/client_refs, no inner key) must still parse via serde defaults.
        let toml = "\
last_synced = \"r0\"
[entries.\"feat-x\"]
status = \"tracked\"
impl_files = [\"src/x.rs\"]
";
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("spec-map.toml");
        std::fs::write(&path, toml).unwrap();
        let map = SpecMap::load(&path).unwrap();
        let e = &map.entries["feat-x"];
        assert_eq!(e.kind, EntryKind::Feature); // default
        assert_eq!(e.key, "feat-x"); // backfilled from map key
        assert!(e.api.is_none());
        assert!(e.client_refs.is_empty());
        assert_eq!(e.impl_files, vec!["src/x.rs".to_string()]);
    }

    #[test]
    fn classify_path_impl_vs_test() {
        assert_eq!(classify_path("src/server/api.rs"), FileRole::Impl);
        assert_eq!(classify_path("crates/x/src/lib.rs"), FileRole::Impl);
        // path component `tests`/`test`
        assert_eq!(classify_path("tests/integration.rs"), FileRole::Test);
        assert_eq!(classify_path("crates/x/tests/e2e.rs"), FileRole::Test);
        // filename conventions
        assert_eq!(classify_path("src/foo_test.rs"), FileRole::Test);
        assert_eq!(classify_path("src/test_foo.py"), FileRole::Test);
        assert_eq!(classify_path("web/button.test.ts"), FileRole::Test);
    }

    #[test]
    fn added_test_path_lands_in_test_files_impl_path_in_impl_files() {
        let mut map = SpecMap::default();
        map.apply_changes(
            &[
                Change::Added("src/server/api.rs".to_string()),
                Change::Added("tests/api_it.rs".to_string()),
            ],
            "docs/specs",
            "ref1",
        );
        // Each unattributed file becomes its own skeleton entry keyed by path.
        let impl_entry = &map.entries["src/server/api.rs"];
        assert_eq!(impl_entry.impl_files, vec!["src/server/api.rs".to_string()]);
        assert!(impl_entry.test_files.is_empty());
        assert_eq!(impl_entry.status, Status::Changed);
        assert_eq!(impl_entry.last_ref.as_deref(), Some("ref1"));

        let test_entry = &map.entries["tests/api_it.rs"];
        assert_eq!(test_entry.test_files, vec!["tests/api_it.rs".to_string()]);
        assert!(test_entry.impl_files.is_empty());
    }

    #[test]
    fn modified_marks_owning_entry_changed() {
        let mut map = SpecMap::default();
        // Consumer-authored feature: one impl + one test under a feature key.
        map.entries.insert(
            "login".to_string(),
            MapEntry {
                key: "login".to_string(),
                kind: EntryKind::Feature,
                spec_doc: Some("docs/specs/login.md".to_string()),
                status: Status::Tracked,
                last_ref: Some("ref0".to_string()),
                impl_files: vec!["src/login.rs".to_string()],
                test_files: vec!["tests/login_test.rs".to_string()],
                client_refs: vec![],
                api: None,
            },
        );
        map.apply_changes(
            &[Change::Modified("src/login.rs".to_string())],
            "docs/specs",
            "ref2",
        );
        let e = &map.entries["login"];
        assert_eq!(e.status, Status::Changed);
        assert_eq!(e.last_ref.as_deref(), Some("ref2"));
        // No stray per-file entry was created (the owning feature claimed it).
        assert_eq!(map.len(), 1);
        assert_eq!(e.impl_files, vec!["src/login.rs".to_string()]);
    }

    #[test]
    fn renamed_moves_path_between_entries() {
        let mut map = SpecMap::default();
        map.apply_changes(
            &[Change::Added("src/old.rs".to_string())],
            "docs/specs",
            "ref1",
        );
        map.apply_changes(
            &[Change::Renamed {
                from: "src/old.rs".to_string(),
                to: "src/new.rs".to_string(),
            }],
            "docs/specs",
            "ref2",
        );
        // Old skeleton entry emptied → Missing; new path attributed.
        assert_eq!(map.entries["src/old.rs"].status, Status::Missing);
        assert!(map.entries["src/old.rs"].is_orphaned());
        let e = &map.entries["src/new.rs"];
        assert_eq!(e.impl_files, vec!["src/new.rs".to_string()]);
        assert_eq!(e.status, Status::Changed);
        assert_eq!(e.last_ref.as_deref(), Some("ref2"));
    }

    #[test]
    fn deleted_detaches_and_marks_missing_when_empty() {
        let mut map = SpecMap::default();
        // Feature with two impl files: deleting one keeps it Changed (still has
        // files); deleting the last leaves it Missing.
        map.entries.insert(
            "feat".to_string(),
            MapEntry {
                key: "feat".to_string(),
                kind: EntryKind::Feature,
                spec_doc: Some("docs/specs/feat.md".to_string()),
                status: Status::Tracked,
                last_ref: Some("ref0".to_string()),
                impl_files: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
                test_files: vec![],
                client_refs: vec![],
                api: None,
            },
        );
        map.apply_changes(
            &[Change::Deleted("src/a.rs".to_string())],
            "docs/specs",
            "ref1",
        );
        assert_eq!(map.entries["feat"].status, Status::Changed);
        assert_eq!(map.entries["feat"].impl_files, vec!["src/b.rs".to_string()]);

        map.apply_changes(
            &[Change::Deleted("src/b.rs".to_string())],
            "docs/specs",
            "ref2",
        );
        assert_eq!(map.entries["feat"].status, Status::Missing);
        assert!(map.entries["feat"].is_orphaned());

        // Deleting an unmapped file is a no-op.
        map.apply_changes(
            &[Change::Deleted("src/never.rs".to_string())],
            "docs/specs",
            "ref3",
        );
        assert!(!map.entries.contains_key("src/never.rs"));
    }

    #[test]
    fn parse_name_status_recognizes_all_kinds() {
        // Realistic `git log --name-status` fragment: commit headers + message
        // body (indented) must be ignored; only status lines are parsed.
        let text = "commit abc123\n\
                    Author: A U Thor <a@b.c>\n\
                    Date:   Mon Jan 1 00:00:00 2026 +0000\n\
                    \n    Add and modify things\n\n\
                    A\tsrc/added.rs\n\
                    M\tsrc/modified.rs\n\
                    R100\tsrc/old.rs\tsrc/new.rs\n\
                    D\tsrc/deleted.rs\n\
                    T\tsrc/typechange.rs\n";
        let changes = parse_name_status(text);
        assert_eq!(
            changes,
            vec![
                Change::Added("src/added.rs".to_string()),
                Change::Modified("src/modified.rs".to_string()),
                Change::Renamed {
                    from: "src/old.rs".to_string(),
                    to: "src/new.rs".to_string(),
                },
                Change::Deleted("src/deleted.rs".to_string()),
                Change::Modified("src/typechange.rs".to_string()),
            ]
        );
    }

    #[test]
    fn parse_name_status_ignores_message_lines_starting_with_status_letter() {
        // A message body line that happens to start with 'A'/'M' etc. (indented,
        // no tab-separated path) must not be mistaken for a change line.
        let text = "    Added a new module\n    Move things around\nA\tsrc/real.rs\n";
        let changes = parse_name_status(text);
        assert_eq!(changes, vec![Change::Added("src/real.rs".to_string())]);
    }

    #[test]
    fn end_to_end_name_status_text_reflects_into_map() {
        // The pure git-log→map pipeline without shelling out to real git:
        // feed synthetic --name-status text through parse + apply. A test path
        // and an impl path are attributed to their respective vectors.
        let mut map = SpecMap::default();
        let text = "A\tsrc/keep.rs\nA\ttests/keep_test.rs\nM\tsrc/keep.rs\n";
        let changes = parse_name_status(text);
        map.apply_changes(&changes, "docs/specs", "r1");

        assert_eq!(
            map.entries["src/keep.rs"].impl_files,
            vec!["src/keep.rs".to_string()]
        );
        assert_eq!(map.entries["src/keep.rs"].status, Status::Changed);
        assert_eq!(
            map.entries["tests/keep_test.rs"].test_files,
            vec!["tests/keep_test.rs".to_string()]
        );
        assert_eq!(map.last_synced, "r1");
    }

    // -- targeted filter (entry_matches) ------------------------------------

    fn filter_entry(
        key: &str,
        spec_doc: Option<&str>,
        impl_files: &[&str],
        test_files: &[&str],
    ) -> MapEntry {
        MapEntry {
            key: key.to_string(),
            kind: EntryKind::Feature,
            spec_doc: spec_doc.map(|s| s.to_string()),
            status: Status::Tracked,
            last_ref: None,
            impl_files: impl_files.iter().map(|s| s.to_string()).collect(),
            test_files: test_files.iter().map(|s| s.to_string()).collect(),
            client_refs: vec![],
            api: None,
        }
    }

    #[test]
    fn entry_matches_on_key_spec_paths_route_case_insensitive() {
        let mut e = filter_entry(
            "drift-map",
            Some("docs/specs/DriftMap.md"),
            &["crates/specguard/src/drift.rs"],
            &["crates/specguard/tests/drift_test.rs"],
        );
        // Matches on the key (case-insensitive).
        assert!(entry_matches(&e, "drift-map"));
        assert!(entry_matches(&e, "DRIFT-MAP"));
        // Matches on the spec_doc.
        assert!(entry_matches(&e, "driftmap.md"));
        // Matches on an impl path fragment.
        assert!(entry_matches(&e, "crates/specguard"));
        // Matches on a test path fragment.
        assert!(entry_matches(&e, "drift_test"));
        // Non-match.
        assert!(!entry_matches(&e, "unrelated-feature"));

        // Matches on the api route for an endpoint entry.
        e.api = Some(ApiRef {
            method: "GET".to_string(),
            route: "/api/health".to_string(),
        });
        assert!(entry_matches(&e, "/health"));
    }

    #[test]
    fn entry_matches_empty_query_matches_all() {
        let e = filter_entry("anything", None, &["src/x.rs"], &[]);
        assert!(entry_matches(&e, ""));
        assert!(entry_matches(&e, "   "));
    }
}
