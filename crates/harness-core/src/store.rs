//! Durable note store. Notes are Obsidian-compatible markdown, grouped per
//! project (keyed by cwd). The store dir can point at a real Obsidian vault.
//!
//! The parallel-session-safe fallback logic (`latest_fallback_note` /
//! `latest_note_for_session` / `recent_session_rescue`) is a harness invariant:
//! it MUST be identical in every plugin, which is why it lives here and is never
//! copied into a plugin crate.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::verdict::Determination;

/// Stable, human-readable project key from a cwd: basename + short hash of the
/// full path (so two different `src/` dirs don't collide).
pub fn project_key(cwd: &Path) -> String {
    let base = cwd
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    let safe: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let h = short_hash(&cwd.to_string_lossy());
    format!("{safe}-{h}")
}

/// FNV-1a 32-bit, hex. Small, dependency-free, stable across runs. Shared so
/// every plugin derives the same project keys and session tags. Thin wrapper
/// over `crate::hash::fnv1a32_hex` — the one FNV-1a implementation.
pub fn short_hash(s: &str) -> String {
    crate::hash::fnv1a32_hex(s)
}

/// Filesystem-safe form of an externally-supplied id (session id, run id) for
/// use as a single path component like `<state_dir>/<safe>.json`. Every char
/// outside `[A-Za-z0-9_.-]` becomes `_`, so the result can never contain a path
/// separator or a `..` traversal that escapes the state dir. Mirrors ctxrot's
/// `safe_session`, hoisted here so condukt/autoflow/ctxrot share one rule.
pub fn safe_session(id: &str) -> String {
    let s: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // A component of only dots (".", "..") is still a traversal/no-op name even
    // though every char is individually allowed — neutralise it.
    if s.chars().all(|c| c == '.') {
        "_".repeat(s.len().max(1))
    } else {
        s
    }
}

/// Base directory for the context-governor ledger state, shared by the
/// context-governor *writer* and the session-insights *reader*. Resolves
/// `CONTEXT_GOVERNOR_STATE_DIR` when set, non-empty, and an **absolute path**,
/// else `$HOME/.context-governor` (`./.context-governor` when `HOME` is
/// unset/empty). A relative override is ignored rather than honored, since a
/// relative path resolves differently depending on the caller's cwd, which
/// would let the writer and reader silently drift onto different bases.
///
/// Centralized here so the writer and reader can never drift on the base path.
pub fn context_ledger_base() -> PathBuf {
    std::env::var("CONTEXT_GOVERNOR_STATE_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .filter(|s| Path::new(s).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| ".".to_string());
            PathBuf::from(home).join(".context-governor")
        })
}

/// Session-scoped state directory for the context ledger:
/// `<`[`context_ledger_base`]`>/<`[`project_key`]`(canonical cwd)>/<`[`safe_session`]`(sid)>`.
///
/// `cwd` is canonicalized *before* the project key is derived, so symlink,
/// trailing-slash, and relative-vs-absolute differences in the caller's cwd
/// string resolve to the same key — this is the single canonicalize step both
/// the writer and reader now share (previously the ledger writer skipped it,
/// letting it drift from the canonicalizing reader). An empty `sid` maps to the
/// `"default"` session, matching the writer's unset-`CLAUDE_CODE_SESSION_ID`
/// fallback.
pub fn context_state_dir(cwd: &Path, sid: &str) -> PathBuf {
    let session = if sid.is_empty() {
        safe_session("default")
    } else {
        safe_session(sid)
    };
    let cwd_canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    context_ledger_base()
        .join(project_key(&cwd_canonical))
        .join(session)
}

/// Full path to the append-only context `ledger.jsonl` for `cwd` / `sid` — the
/// single source of truth that context-governor (writer) and session-insights
/// (reader) both delegate to, so the two can never derive different paths for
/// the same logical repo + session.
pub fn ledger_path(cwd: &Path, sid: &str) -> PathBuf {
    context_state_dir(cwd, sid).join("ledger.jsonl")
}

/// Short, stable tag for a session id, embedded in note filenames so a session
/// can deterministically find its own notes even when sibling sessions write
/// into the same project dir in parallel. Empty id → "nosess".
pub fn session_tag(session_id: &str) -> String {
    if session_id.is_empty() {
        "nosess".to_string()
    } else {
        short_hash(session_id)
    }
}

/// The session tag embedded in a note filename, if it follows the tagged scheme
/// `<slug>-<tag>-<YYYYMMDD>-<HHMMSS>` (tag = 8 hex from `short_hash`, or `nosess`).
/// Returns None for legacy/untagged notes — the signal `latest_fallback_note`
/// uses to tell streams apart.
fn note_session_tag(path: &Path) -> Option<String> {
    let stem = path.file_stem().and_then(|s| s.to_str())?;
    tagged_note_re().captures(stem).map(|c| c[1].to_string())
}

fn tagged_note_re() -> Regex {
    Regex::new(r"-([0-9a-f]{8}|nosess)-\d{8}-\d{6}$").expect("static regex")
}

/// A `distill-*` note (the high-value, LLM-distilled carryover), as opposed to a
/// deterministic `rescue-*`. Used by `prune` to protect distills preferentially,
/// and by `restore` to nudge when only deterministic rescues exist.
pub fn is_distill(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .map(|n| n.starts_with("distill-"))
        .unwrap_or(false)
}

/// Outcome of `Store::prune`: how many notes survived and which were removed
/// (the removal set is also the dry-run preview).
pub struct PruneResult {
    pub kept: usize,
    pub removed: Vec<PathBuf>,
}

pub struct Store {
    pub root: PathBuf,
}

impl Store {
    /// Build a store rooted at `root` (a plugin passes its configured `store_dir`).
    pub fn new(root: PathBuf) -> Self {
        Store { root }
    }

    /// Directory holding a project's notes (created on demand by `write`).
    pub fn project_dir(&self, cwd: &Path) -> PathBuf {
        self.root.join(project_key(cwd))
    }

    /// Write a note. `slug` is a filesystem-safe stem; returns the full path.
    pub fn write_note(&self, cwd: &Path, slug: &str, body: &str) -> std::io::Result<PathBuf> {
        let dir = self.project_dir(cwd);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{slug}.md"));
        std::fs::write(&path, body)?;
        Ok(path)
    }

    /// All `.md` notes in a project's dir, newest first (by modified time).
    ///
    /// Three-valued on purpose (see [`Determination`]):
    ///
    /// * `Known(notes)` — the project dir was enumerated. An **absent** dir
    ///   (`NotFound`: this project has never been written to) is a genuine
    ///   `Known(vec![])`.
    /// * `Undetermined(why)` — the project dir exists but could not be walked
    ///   (permissions, IO error), or a directory entry could not be read. **No
    ///   partial list is returned**: every derived selector below (latest /
    ///   fallback / per-session / prune) would otherwise silently operate on a
    ///   truncated view and report "no carryover exists" for a store it merely
    ///   could not read. `why` carries the path and the OS error.
    pub fn list_notes(&self, cwd: &Path) -> Determination<Vec<PathBuf>> {
        let dir = self.project_dir(cwd);
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            // A project dir that does not exist yet genuinely holds no notes.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Determination::Known(Vec::new())
            }
            Err(e) => {
                return Determination::undetermined(format!(
                    "note store {} could not be read: {e}",
                    dir.display()
                ))
            }
        };
        let mut entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        for e in rd {
            // An unreadable entry makes the listing incomplete; a truncated
            // list would be read downstream as the whole note history.
            let e = match e {
                Ok(e) => e,
                Err(err) => {
                    return Determination::undetermined(format!(
                        "note store {}: a directory entry could not be read \
                         (listing is incomplete): {err}",
                        dir.display()
                    ))
                }
            };
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("md") {
                let mtime = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                entries.push((mtime, p));
            }
        }
        // Newest-first by mtime, breaking ties by filename descending. Notes embed
        // a timestamp in their name (e.g. rescue-<tag>-20260619-110000), so the
        // lexically greater name is the chronologically later note. Without this
        // tie-break two notes written in the same mtime tick (fast filesystems /
        // CI) order nondeterministically, which flaked latest_fallback_note.
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        Determination::Known(entries.into_iter().map(|(_, p)| p).collect())
    }

    /// Most recent note for a project. `Known(None)` = the dir was read and
    /// holds no note; `Undetermined` = the dir could not be read (propagated
    /// from [`list_notes`](Self::list_notes)) — the two must not collapse.
    pub fn latest_note(&self, cwd: &Path) -> Determination<Option<PathBuf>> {
        self.list_notes(cwd).map(|n| n.into_iter().next())
    }

    /// Write a note under an exact filename (no slug sanitizing). Test/utility helper.
    ///
    /// `name` must not contain path separators or `..` components — if it does, those
    /// characters are replaced with `-` so the write always lands inside the project dir.
    pub fn write_note_named(&self, cwd: &Path, name: &str, body: &str) -> std::io::Result<PathBuf> {
        let dir = self.project_dir(cwd);
        std::fs::create_dir_all(&dir)?;
        // Sanitise: strip any path-separator or dot-dot so the caller cannot escape the
        // project dir via a crafted `name` (e.g. "../../etc/passwd").
        let safe_name: String = name
            .chars()
            .map(|c| if c == '/' || c == '\\' { '-' } else { c })
            .collect();
        // Collapse remaining `..` segments that could survive as part of a file-stem.
        // A lone ".." or "." in the name is also replaced.
        let safe_name = safe_name
            .split("--")
            .flat_map(|seg| seg.split('-'))
            .filter(|seg| !seg.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        // Use only the final path component (after any remaining separator) as the name.
        let safe_name = Path::new(&safe_name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("note")
            .to_string();
        let path = dir.join(format!("{safe_name}.md"));
        // Verify the resolved path is actually inside `dir` (defence-in-depth).
        let canonical_dir = dir.canonicalize().unwrap_or(dir.clone());
        // The note file does not exist yet, so `path.canonicalize()` fails in the common
        // case; fall back to joining onto the *canonical* dir (not the raw `dir`). On
        // platforms where the store root is reached through a symlink (e.g. macOS temp
        // dirs under /var -> /private/var), using the raw `dir` here would make the
        // prefix check below spuriously fail. `safe_name` is already reduced to a single
        // sanitised component, so this join cannot escape the root.
        let canonical_path = path
            .canonicalize()
            .unwrap_or_else(|_| canonical_dir.join(format!("{safe_name}.md")));
        if !canonical_path.starts_with(&canonical_dir) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("write_note_named: path would escape store root: {canonical_path:?}"),
            ));
        }
        std::fs::write(&path, body)?;
        Ok(path)
    }

    /// Cross-session fallback note for `restore` when this session has no note of
    /// its own. Prevents grabbing a *sibling* stream's carryover in shared-cwd
    /// parallel use, WITHOUT breaking ordinary cross-session continuity:
    ///   * ≤1 distinct session tag in the dir → unambiguous (single stream, or a
    ///     prior sequential session) → return the latest note of any kind.
    ///   * ≥2 distinct session tags → parallel usage detected → restrict to
    ///     untagged (legacy / explicitly-shared) notes; never another session's.
    ///
    /// (Own-session notes are already handled by `latest_note_for_session`, so by
    /// the time we get here the tags present belong to *other* sessions.)
    ///
    /// `Undetermined` propagates from [`list_notes`](Self::list_notes): "the
    /// store could not be read" must not present as "this project has no
    /// carryover", which would silently start a session from scratch.
    pub fn latest_fallback_note(&self, cwd: &Path) -> Determination<Option<PathBuf>> {
        self.list_notes(cwd).map(|notes| {
            let distinct: HashSet<String> =
                notes.iter().filter_map(|p| note_session_tag(p)).collect();
            if distinct.len() <= 1 {
                notes.into_iter().next()
            } else {
                notes.into_iter().find(|p| note_session_tag(p).is_none())
            }
        })
    }

    /// Most recent `rescue-<tag>-*` note for this session whose mtime is within
    /// `within_secs` of now — the coalescing probe (P3). `Known(None)` when the
    /// dir was read and holds no such fresh rescue, so the caller writes a new
    /// one.
    ///
    /// `Undetermined` propagates from [`list_notes`](Self::list_notes) rather
    /// than being folded into `Known(None)`. The caller's restricted side here
    /// happens to be "write another rescue" (a duplicate note, never lost
    /// data), but it must reach that by *seeing* the undetermined answer, not
    /// by mistaking it for "no fresh rescue exists".
    pub fn recent_session_rescue(
        &self,
        cwd: &Path,
        session_id: &str,
        within_secs: u64,
    ) -> Determination<Option<PathBuf>> {
        if session_id.is_empty() {
            return Determination::Known(None);
        }
        let prefix = format!("rescue-{}-", session_tag(session_id));
        let cutoff = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(within_secs))
            .unwrap_or(std::time::UNIX_EPOCH);
        // list_notes is newest-first, so the first match in window wins.
        self.list_notes(cwd).map(|notes| {
            notes.into_iter().find(|p| {
                let is_ours = p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|n| n.starts_with(&prefix))
                    .unwrap_or(false);
                is_ours
                    && std::fs::metadata(p)
                        .and_then(|m| m.modified())
                        .map(|t| t >= cutoff)
                        .unwrap_or(false)
            })
        })
    }

    /// GC: keep the newest `keep` notes overall, plus the newest `keep_distill_min`
    /// `distill-*` notes (higher value than rescues) even if they fall outside that
    /// window; delete the rest. `dry_run` computes the removal set without touching
    /// disk. Deletes are best-effort.
    ///
    /// `Undetermined` propagates from [`list_notes`](Self::list_notes) and
    /// **nothing is deleted** in that case: pruning against a partial listing
    /// could delete notes that a complete listing would have protected.
    pub fn prune(
        &self,
        cwd: &Path,
        keep: usize,
        keep_distill_min: usize,
        dry_run: bool,
    ) -> Determination<PruneResult> {
        self.list_notes(cwd).map(|notes| {
            // newest first
            let mut protect: HashSet<PathBuf> = notes.iter().take(keep).cloned().collect();
            for p in notes
                .iter()
                .filter(|p| is_distill(p))
                .take(keep_distill_min)
            {
                protect.insert(p.clone());
            }
            let mut removed = Vec::new();
            for p in &notes {
                if protect.contains(p) {
                    continue;
                }
                if !dry_run {
                    let _ = std::fs::remove_file(p);
                }
                removed.push(p.clone());
            }
            PruneResult {
                kept: notes.len() - removed.len(),
                removed,
            }
        })
    }

    /// Most recent note whose filename carries this session's tag. Lets the
    /// originating session reach its own note amid parallel sessions sharing the
    /// project dir. `Known(None)` if the id is empty or no tagged note exists;
    /// `Undetermined` propagates from [`list_notes`](Self::list_notes) so an
    /// unreadable store cannot present as "this session wrote no note".
    pub fn latest_note_for_session(
        &self,
        cwd: &Path,
        session_id: &str,
    ) -> Determination<Option<PathBuf>> {
        if session_id.is_empty() {
            return Determination::Known(None);
        }
        let needle = format!("-{}-", session_tag(session_id));
        self.list_notes(cwd).map(|notes| {
            notes.into_iter().find(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|n| n.contains(&needle))
                    .unwrap_or(false)
            })
        })
    }
}

/// Load a JSON value, collapsing every miss AND every read/parse FAILURE into
/// `T::default()`.
///
/// This is fail-**open**: a missing file, an unreadable one (EACCES, an
/// unsearchable parent dir), and a corrupt/truncated one all return the SAME
/// `T::default()`, so a caller cannot tell "nothing recorded yet" from "I could
/// not read what was recorded". The collapse is written out below rather than
/// buried in `.ok()`, and it is acceptable ONLY for callers whose decision does
/// not depend on that distinction.
///
/// **A gate that reads state to reach a verdict must use
/// [`load_json_determined`] instead** — it keeps "could not read"
/// (`Undetermined`) apart from "absent" (`Known(default)`), so an unreadable
/// history cannot silence a gate. See CLAUDE.md §3.
pub fn load_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    match load_json_determined(path) {
        Determination::Known(v) => v,
        // Fail-open, deliberately and in the open: a caller of THIS function has
        // opted into "the default on anything I could not read".
        Determination::Undetermined(_) => T::default(),
    }
}

/// Load a JSON value with the three outcomes of a load kept distinct instead of
/// collapsed into the type's default.
///
/// | situation | answer |
/// |---|---|
/// | the file is not there | `Known(T::default())` — a real "nothing recorded yet" |
/// | there but unreadable (EACCES / unsearchable parent / I/O fault) | `Undetermined` |
/// | there but it does not parse as `T` (schema drift, truncated write) | `Undetermined` |
/// | it parsed | `Known(T)` |
///
/// Absence is the ONLY miss that stays `Known`: a file that was never written is
/// genuinely empty. Opacity — unreadable or unparseable — is `Undetermined`,
/// because the bytes the caller wanted may well be there. This is the input-side
/// counterpart to [`Determination`]'s output-side contract, and the fix for the
/// fail-open that [`load_json`] documents: a gate reads through this so an
/// unreadable store cannot masquerade as a fresh one.
pub fn load_json_determined<T: serde::de::DeserializeOwned + Default>(
    path: &Path,
) -> Determination<T> {
    match crate::boundary::read_to_string(path) {
        Determination::Known(None) => Determination::known(T::default()),
        Determination::Known(Some(text)) => match serde_json::from_str::<T>(&text) {
            Ok(v) => Determination::known(v),
            Err(e) => Determination::undetermined(format!(
                "{} exists but does not parse as {}: {e} — a corrupt or truncated \
                 store is not an empty one",
                path.display(),
                std::any::type_name::<T>()
            )),
        },
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Save a JSON value (compact), creating parent dirs. Fail-soft: IO/serialize
/// errors are swallowed.
///
/// The compact counterpart of `load_json`; routes the
/// `create_dir_all`→`to_string`→`write` idiom through one place. Pretty-printed
/// or `Result`-returning save sites deliberately keep their own bodies.
///
/// The write is **atomic**: the payload is streamed to a sibling temp file
/// (unique per process + call so parallel sessions never share one), flushed to
/// disk, then `rename`d over the target. A crash or a concurrent writer can
/// therefore never observe a half-written store — readers see either the old
/// file or the complete new one, never a truncated middle (the failure mode of
/// the previous `fs::write`, which truncated in place before writing).
pub fn save_json<T: serde::Serialize>(path: &Path, val: &T) {
    let Ok(s) = serde_json::to_string(val) else {
        return;
    };
    save_bytes(path, s.as_bytes());
}

/// Durably write raw `bytes` to `path` — the crash-safe primitive behind
/// [`save_json`], usable directly for non-JSON payloads (e.g. a marker file
/// holding a bare path). Writes to a sibling temp, fsyncs it, then renames over
/// `path`, so a crash / power loss / process kill can only ever expose complete
/// data — the reader sees either the old file or the whole new one, never a
/// truncated middle (the failure mode of a plain in-place `fs::write`).
///
/// Best-effort and panic-free (never break a turn): creates parent dirs, and on
/// any write/rename failure removes the temp and returns without erroring.
pub fn save_bytes(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Sibling temp name, unique across processes (pid) and across concurrent
    // calls in this process (monotonic counter) so two writers can't clobber a
    // shared temp. Same dir as `path` so the final `rename` stays on one
    // filesystem (cross-device rename would fail).
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let fname = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("store.tmp");
    let tmp = path.with_file_name(format!(".{fname}.tmp.{}.{seq}", std::process::id()));

    // Write fully + fsync the temp before renaming, so the rename can only ever
    // expose complete data even across a power loss / process kill.
    let wrote = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if wrote.is_ok() {
        if std::fs::rename(&tmp, path).is_err() {
            // Rename failed (e.g. target dir vanished); don't leave the temp behind.
            let _ = std::fs::remove_file(&tmp);
        }
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared ledger-path derivation must canonicalize cwd before the project
    /// key, so a symlinked cwd resolves to the *target's* key — this is what keeps
    /// the context-governor writer and the session-insights reader from drifting
    /// when their cwd strings differ. Asserts only the base-independent path tail
    /// (`<project_key>/<session>/ledger.jsonl`) so it never races env-mutating
    /// tests. Would fail if the canonicalize step were dropped (the symlink's own
    /// basename would produce a different key).
    #[test]
    fn ledger_path_canonicalizes_cwd_and_defaults_empty_session() {
        let base = std::env::temp_dir().join(format!("harness-ledger-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("realrepo");
        std::fs::create_dir_all(&real).expect("mk real dir");

        #[cfg(unix)]
        let link = {
            let link = base.join("linkrepo");
            std::os::unix::fs::symlink(&real, &link).expect("symlink");
            link
        };
        #[cfg(not(unix))]
        let link = real.clone();

        let canonical = real.canonicalize().expect("canonicalize real");
        let expected_tail = Path::new(&project_key(&canonical))
            .join(safe_session("sess-1"))
            .join("ledger.jsonl");

        // Symlinked cwd must canonicalize onto the target's key.
        let p_link = ledger_path(&link, "sess-1");
        assert!(
            p_link.ends_with(&expected_tail),
            "symlinked cwd must canonicalize to the target key: {p_link:?} !~ {expected_tail:?}"
        );
        // Real cwd yields the same tail (writer/reader agree).
        let p_real = ledger_path(&real, "sess-1");
        assert!(
            p_real.ends_with(&expected_tail),
            "real cwd tail: {p_real:?}"
        );

        // Empty session id → the "default" session component (writer's
        // unset-CLAUDE_CODE_SESSION_ID fallback).
        let default_tail = Path::new(&project_key(&canonical))
            .join(safe_session("default"))
            .join("ledger.jsonl");
        let p_default = ledger_path(&real, "");
        assert!(
            p_default.ends_with(&default_tail),
            "empty sid must map to 'default': {p_default:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn save_bytes_roundtrips_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("harness-savebytes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("marker");

        // Exact bytes written, parent dirs created on demand.
        save_bytes(&path, b"/notes/distill-abc.md");
        assert_eq!(std::fs::read(&path).unwrap(), b"/notes/distill-abc.md");

        // Overwrite with shorter content — reader sees the whole new value, not a
        // torn mix of old+new (the in-place fs::write failure mode).
        save_bytes(&path, b"/x");
        assert_eq!(std::fs::read(&path).unwrap(), b"/x");

        // A successful write renames the temp away — no `.tmp.` sibling lingers.
        let temps: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(temps.is_empty(), "temp must be renamed away: {temps:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_bytes_concurrent_same_path_is_atomic() {
        use std::sync::Arc;
        let dir =
            std::env::temp_dir().join(format!("harness-savebytes-conc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = Arc::new(dir.join("marker"));

        // Each writer writes a distinct full-size content concurrently. Because
        // every write goes through its own temp + rename, the final file must
        // equal EXACTLY one writer's whole content — never a truncated or
        // interleaved blend (durability guard for the distill marker).
        let contents: Vec<Vec<u8>> = (0..8u8)
            .map(|i| vec![b'A' + i; 4096 + i as usize])
            .collect();
        std::thread::scope(|s| {
            for c in &contents {
                let p = Arc::clone(&path);
                s.spawn(move || save_bytes(&p, c));
            }
        });
        let final_bytes = std::fs::read(&*path).unwrap();
        assert!(
            contents.contains(&final_bytes),
            "final file must be exactly one writer's full content; got {} bytes",
            final_bytes.len()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Resolve a `Determination` in a test whose store is readable by
    /// construction. Panics (rather than substituting an empty value) if the
    /// listing came back `Undetermined`, so a regression that makes a readable
    /// store unreadable fails the test loudly instead of silently asserting
    /// over an empty list.
    fn known<T>(d: Determination<T>) -> T {
        match d {
            Determination::Known(v) => v,
            Determination::Undetermined(why) => {
                panic!("readable store must be Known, got Undetermined({why:?})")
            }
        }
    }

    /// A throwaway store rooted under the temp dir, isolated per test name + pid.
    fn temp_store(name: &str) -> (Store, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("harness-store-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        (Store::new(root.clone()), root)
    }

    #[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Demo {
        a: u32,
        b: String,
    }

    #[test]
    fn json_roundtrips_compact() {
        let root = std::env::temp_dir().join(format!("harness-json-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let path = root.join("nested").join("demo.json");
        // Missing file → Default, never an error.
        assert_eq!(load_json::<Demo>(&path), Demo::default());

        let val = Demo {
            a: 7,
            b: "hi".into(),
        };
        save_json(&path, &val); // creates parent dirs
        assert_eq!(load_json::<Demo>(&path), val);
        // Compact: no pretty-print newlines/indent.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains('\n'), "save_json must be compact: {raw}");

        // Corrupt file → Default, fail-soft.
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_json::<Demo>(&path), Demo::default());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_json_is_atomic_under_concurrency() {
        // Many threads hammer the same path. With a non-atomic in-place write,
        // an interleaved read could see a truncated/partial file and fail to
        // parse. With the temp-file + rename scheme, every read must yield a
        // fully-valid `Demo` (either the old or a new complete value).
        use std::sync::Arc;
        let root = std::env::temp_dir().join(format!("harness-json-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let path = Arc::new(root.join("nested").join("demo.json"));
        // Seed a valid file so readers never hit the missing-file path.
        save_json(
            &path,
            &Demo {
                a: 0,
                b: "seed".into(),
            },
        );

        let mut handles = Vec::new();
        for t in 0..8u32 {
            let p = Arc::clone(&path);
            handles.push(std::thread::spawn(move || {
                for i in 0..50u32 {
                    save_json(
                        &p,
                        &Demo {
                            a: t * 1000 + i,
                            b: format!("writer-{t}-{i}"),
                        },
                    );
                    // Interleave reads: each must parse to a complete value.
                    let got = load_json::<Demo>(&p);
                    assert!(
                        !got.b.is_empty(),
                        "load_json saw a partial/empty store: {got:?}"
                    );
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Final state is some complete, valid value and no temp files leaked.
        let final_val = load_json::<Demo>(&path);
        assert!(final_val.b.starts_with("writer-") || final_val.b == "seed");
        let leftover: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftover.is_empty(), "temp files leaked: {leftover:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn safe_session_blocks_traversal() {
        // A crafted id must never yield a component with a separator or `..`.
        for evil in ["../../etc/passwd", "..", "a/b", "a\\b", "...", "."] {
            let s = safe_session(evil);
            assert!(!s.contains('/'), "safe_session leaked '/': {s}");
            assert!(!s.contains('\\'), "safe_session leaked '\\': {s}");
            assert_ne!(s, "..", "safe_session left a traversal: {s}");
            assert_ne!(s, ".", "safe_session left a no-op component: {s}");
            // The result is always a single path component.
            assert_eq!(
                Path::new(&s).components().count(),
                1,
                "safe_session must be one component: {s}"
            );
            // Joining onto a base dir cannot escape it.
            let joined = Path::new("/state").join(format!("{s}.json"));
            assert!(joined.starts_with("/state"), "escaped base: {joined:?}");
        }
        // Ordinary ids pass through unchanged.
        assert_eq!(safe_session("sess-A_1.2"), "sess-A_1.2");
    }

    #[test]
    fn session_tag_is_stable_and_distinct() {
        assert_eq!(session_tag("sess-A"), session_tag("sess-A"));
        assert_ne!(session_tag("sess-A"), session_tag("sess-B"));
        assert_eq!(session_tag(""), "nosess");
    }

    #[test]
    fn session_routing_picks_own_note() {
        let (store, root) = temp_store("routing");
        let cwd = Path::new("/some/project");
        let a = session_tag("session-A");
        let b = session_tag("session-B");

        store
            .write_note_named(cwd, &format!("distill-{a}-20260619-100000"), "mine")
            .unwrap();
        store
            .write_note_named(cwd, &format!("rescue-{b}-20260619-110000"), "theirs")
            .unwrap();

        let mine = known(store.latest_note_for_session(cwd, "session-A")).unwrap();
        assert!(mine.to_string_lossy().contains(&a));
        assert!(!mine.to_string_lossy().contains(&b));

        // Unknown session → no tagged match, caller falls back to latest_note.
        assert!(known(store.latest_note_for_session(cwd, "session-C")).is_none());
        // Empty session id is never routed.
        assert!(known(store.latest_note_for_session(cwd, "")).is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_filename_session_tag() {
        let a = session_tag("session-A");
        assert_eq!(
            note_session_tag(Path::new(&format!("/x/distill-{a}-20260619-100000.md"))),
            Some(a)
        );
        assert_eq!(
            note_session_tag(Path::new("/x/rescue-nosess-20260619-100000.md")),
            Some("nosess".to_string())
        );
        // Legacy/untagged notes carry no session tag.
        assert_eq!(
            note_session_tag(Path::new("/x/rescue-20260619-100000.md")),
            None
        );
        assert_eq!(note_session_tag(Path::new("/x/handwritten-notes.md")), None);
    }

    #[test]
    fn fallback_single_stream_keeps_continuity() {
        let (store, root) = temp_store("fb-single");
        let cwd = Path::new("/some/project");
        let a = session_tag("prev-session");

        // Only one (prior, sequential) session's notes → unambiguous → return latest.
        store
            .write_note_named(cwd, &format!("distill-{a}-20260619-100000"), "old")
            .unwrap();
        store
            .write_note_named(cwd, &format!("rescue-{a}-20260619-110000"), "newer")
            .unwrap();
        let fb = known(store.latest_fallback_note(cwd)).unwrap();
        assert!(std::fs::read_to_string(&fb).unwrap().contains("newer"));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Write `n` notes with the given slug prefix, oldest first, nudging mtime
    /// forward so `list_notes` ordering is deterministic.
    fn write_series(store: &Store, cwd: &Path, prefix: &str, n: usize) {
        for i in 0..n {
            store
                .write_note_named(cwd, &format!("{prefix}-{i:02}"), &format!("body {i}"))
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn prune_dry_run_removes_nothing() {
        let (store, root) = temp_store("prune-dry");
        let cwd = Path::new("/some/project");
        write_series(&store, cwd, "rescue-aaaaaaaa-2026010", 5);

        let res = known(store.prune(cwd, 2, 0, true));
        assert_eq!(res.removed.len(), 3);
        // Nothing actually deleted.
        assert_eq!(known(store.list_notes(cwd)).len(), 5);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_keeps_newest_n() {
        let (store, root) = temp_store("prune-n");
        let cwd = Path::new("/some/project");
        write_series(&store, cwd, "rescue-aaaaaaaa-2026010", 5);

        let res = known(store.prune(cwd, 2, 0, false));
        assert_eq!(res.removed.len(), 3);
        assert_eq!(known(store.list_notes(cwd)).len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_protects_distill_floor() {
        let (store, root) = temp_store("prune-distill");
        let cwd = Path::new("/some/project");
        // Oldest = one distill, then 4 rescues on top.
        store
            .write_note_named(cwd, "distill-aaaaaaaa-20260101-000000", "d")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        write_series(&store, cwd, "rescue-aaaaaaaa-2026010", 4);

        // keep newest 2 (both rescues) + protect newest 1 distill (the old one).
        let res = known(store.prune(cwd, 2, 1, false));
        assert_eq!(res.removed.len(), 2); // the two oldest rescues
        let remaining = known(store.list_notes(cwd));
        assert_eq!(remaining.len(), 3);
        assert!(
            remaining.iter().any(|p| is_distill(p)),
            "the distill note must survive: {remaining:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── path-traversal safety tests ──────────────────────────────────────────

    /// project_key must never embed `/`, `\`, or `..` even when the cwd itself
    /// contains traversal sequences like `/home/user/../../../etc`.
    #[test]
    fn project_key_is_path_safe() {
        let cwd = Path::new("/home/user/../../../etc");
        let key = project_key(cwd);
        assert!(
            !key.contains('/'),
            "project_key must not contain '/': {key}"
        );
        assert!(
            !key.contains('\\'),
            "project_key must not contain '\\': {key}"
        );
        assert!(
            !key.contains(".."),
            "project_key must not contain '..': {key}"
        );
        // The key must be a single path component (no directory separators).
        assert_eq!(
            Path::new(&key).components().count(),
            1,
            "project_key must be a single path component: {key}"
        );
    }

    /// Even when the cwd uses `..` in non-trailing position the resulting key
    /// must still be a safe, single-component string.
    #[test]
    fn project_key_dotdot_in_basename() {
        let cwd = Path::new("/tmp/../tmp/foo");
        let key = project_key(cwd);
        assert!(
            !key.contains('/'),
            "project_key must not contain '/': {key}"
        );
        assert!(
            !key.contains(".."),
            "project_key must not contain '..': {key}"
        );
        assert_eq!(
            Path::new(&key).components().count(),
            1,
            "project_key must be a single path component: {key}"
        );
    }

    /// write_note_named must keep the output path inside the store root even
    /// when `name` contains traversal sequences like `../../escape`.
    #[cfg(unix)]
    #[test]
    fn write_note_named_succeeds_through_symlinked_root() {
        // Regression: when the store root is reached via a symlink (e.g. macOS temp
        // dirs under /var -> /private/var), the escape check canonicalized the dir but
        // fell back to the raw, non-canonical path for the not-yet-created note — so the
        // prefix check spuriously failed and every write was rejected.
        let base =
            std::env::temp_dir().join(format!("harness-store-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = base.join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // Point the store root *through* the symlink.
        let store = Store::new(link.clone());
        let cwd = Path::new("/some/project");
        let path = store
            .write_note_named(cwd, "rescue-deadbeef-20260101-000000", "body")
            .expect("write through symlinked root should succeed");
        assert!(
            path.exists(),
            "note file should have been written: {path:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "body");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn write_note_named_stays_in_root() {
        let (store, root) = temp_store("traversal");
        let cwd = Path::new("/some/project");

        // Ensure the project dir exists before canonicalize can work.
        let project_dir = store.project_dir(cwd);
        std::fs::create_dir_all(&project_dir).unwrap();

        let result = store.write_note_named(cwd, "../../escape", "body");
        // The call must either succeed with a path inside the root or return an error.
        // It must NOT successfully write a file outside the store root.
        match result {
            Ok(path) => {
                assert!(
                    path.starts_with(&root),
                    "returned path {path:?} escapes store root {root:?}"
                );
                // Confirm the file actually landed inside the root.
                let canonical = path.canonicalize().unwrap_or(path.clone());
                let canonical_root = root.canonicalize().unwrap_or(root.clone());
                assert!(
                    canonical.starts_with(&canonical_root),
                    "canonical path {canonical:?} escapes store root {canonical_root:?}"
                );
            }
            Err(e) => {
                // An error is also acceptable (permission denied / path blocked).
                assert!(
                    e.kind() == std::io::ErrorKind::PermissionDenied
                        || e.kind() == std::io::ErrorKind::NotFound,
                    "unexpected error kind: {e}"
                );
            }
        }

        // Additionally verify no file was created outside the root.
        let escaped = root.parent().unwrap_or(&root).join("escape.md");
        assert!(
            !escaped.exists(),
            "a file was created outside the store root: {escaped:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Exercises the `canonical_dir` fallback at the top of the escape check
    /// (`dir.canonicalize().unwrap_or(dir.clone())`): when the project dir
    /// cannot be canonicalized (here: it was removed out from under us right
    /// after `write_note_named` created it, simulating a DrvFs/WSL-style
    /// mount or a race where the dir briefly doesn't resolve), the function
    /// must fall back to the raw `dir` rather than panicking or silently
    /// treating every write as an escape.
    #[cfg(unix)]
    #[test]
    fn write_note_named_canonicalize_dir_fallback_does_not_panic() {
        let (store, root) = temp_store("canon-dir-fallback");
        let cwd = Path::new("/some/other-project");

        let project_dir = store.project_dir(cwd);
        std::fs::create_dir_all(&project_dir).unwrap();

        // Replace the project dir with a dangling symlink so
        // `project_dir.canonicalize()` fails (ENOENT via the broken symlink)
        // even though the parent directory structure still exists — this is
        // the shape of failure the `unwrap_or(dir.clone())` fallback guards
        // against (canonicalize() erroring on a real, resolvable-by-path but
        // not resolvable-by-canonicalize directory reference).
        std::fs::remove_dir_all(&project_dir).unwrap();
        let dangling_target = root.join("does-not-exist-target");
        std::os::unix::fs::symlink(&dangling_target, &project_dir).unwrap();
        assert!(
            project_dir.canonicalize().is_err(),
            "test setup: project_dir must fail to canonicalize (dangling symlink)"
        );

        // write_note_named must not panic; it either errors cleanly or
        // succeeds with a path that is still inside `root` (the fallback
        // path must not let a write escape just because canonicalize failed).
        let result = store.write_note_named(cwd, "fallback-note", "body");
        match result {
            Ok(path) => {
                assert!(
                    path.starts_with(&root) || path.starts_with(&project_dir),
                    "fallback path must stay under the project dir/root, got {path:?}"
                );
            }
            Err(e) => {
                // A clean IO error (e.g. can't create/write through the
                // dangling symlink) is an acceptable outcome — the important
                // invariant is "no panic, no escape", not "must succeed".
                let _ = e;
            }
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Guards tests that mutate the process-global `CONTEXT_GOVERNOR_STATE_DIR`
    /// env var so they never race each other (harness-core tests run in
    /// parallel by default).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn context_ledger_base_only_honors_absolute_state_dir() {
        const VAR: &str = "CONTEXT_GOVERNOR_STATE_DIR";
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(VAR).ok();

        // Relative path → must be ignored, falling back to the home-based default.
        std::env::set_var(VAR, "relstate");
        let base = context_ledger_base();
        assert_ne!(
            base,
            PathBuf::from("relstate"),
            "a relative CONTEXT_GOVERNOR_STATE_DIR must not be honored: {base:?}"
        );
        assert!(
            base.ends_with(".context-governor"),
            "relative override must fall back to the home-based default: {base:?}"
        );

        // Absolute path → honored as-is.
        std::env::set_var(VAR, "/tmp/cg-abs-test");
        let base = context_ledger_base();
        assert_eq!(base, PathBuf::from("/tmp/cg-abs-test"));

        // Restore whatever was there before this test (or remove it).
        match prev {
            Some(v) => std::env::set_var(VAR, v),
            None => std::env::remove_var(VAR),
        }
    }

    #[test]
    fn fallback_parallel_avoids_sibling_tagged_note() {
        let (store, root) = temp_store("fb-parallel");
        let cwd = Path::new("/some/project");
        let a = session_tag("sib-A");
        let b = session_tag("sib-B");

        // Two distinct sessions → parallel → must NOT return either tagged note.
        store
            .write_note_named(cwd, &format!("distill-{a}-20260619-100000"), "A")
            .unwrap();
        store
            .write_note_named(cwd, &format!("rescue-{b}-20260619-110000"), "B")
            .unwrap();
        assert!(known(store.latest_fallback_note(cwd)).is_none());

        // With an untagged shared note present, fall back to that instead.
        store
            .write_note_named(cwd, "shared-handoff", "shared")
            .unwrap();
        let fb = known(store.latest_fallback_note(cwd)).unwrap();
        assert!(std::fs::read_to_string(&fb).unwrap().contains("shared"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
