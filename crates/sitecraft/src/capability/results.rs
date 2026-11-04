//! D5: the input hash of a capability, its result record, and the Stop
//! gate's freshness check over those records (which never runs anything).
//!
//! * Record: `<state>/capability-results/<root-key>/<name>.json` =
//!   `{"name","exit","input_hash","sitecraft_version","at"}`, written to a
//!   temp file in the same directory and renamed. `<root-key>` = sha256 hex
//!   of the canonical project root path. `input_hash` is `null` only when it
//!   could not be computed (that run is then exit 2 anyway).
//! * Input hash: sha256 of the canonical JSON of the `[capabilities.<name>]`
//!   table followed, for every input file in root-relative path order, by
//!   the JSON string of the path and the JSON string of the sha256 hex of its
//!   content. Input files per kind: blender-headless — `script` +
//!   `verify.inputs`; threejs-render — `sitecraft.toml` + its `web_globs` +
//!   `verify.inputs`; command — `verify.inputs` (no match → `Err`). Any
//!   unreadable file or directory on the way → `Err`.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decide::capability::{self, Capability, Verify};
use crate::Verdict;

/// Sub-directory of the state dir holding the result records.
pub const RESULTS_DIR: &str = "capability-results";
/// The running sitecraft version (stored in, and required of, every record).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Upper bound on directory entries one input collection visits (past it the
/// hash is not computable: never a partial file set).
const MAX_WALK_ENTRIES: usize = 200_000;

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `<root-key>`: sha256 hex of the canonical root path. `Err` when the root
/// cannot be canonicalized.
pub fn root_key(root: &Path) -> Result<String, String> {
    let c = std::fs::canonicalize(root).map_err(|e| {
        format!(
            "cannot canonicalize the project root {}: {e}",
            root.display()
        )
    })?;
    Ok(hex(c.as_os_str().as_bytes()))
}

/// Path of the result record of `name`.
pub fn record_path(state: &Path, root: &Path, name: &str) -> Result<PathBuf, String> {
    Ok(state
        .join(RESULTS_DIR)
        .join(root_key(root)?)
        .join(format!("{name}.json")))
}

/// One result record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultRecord {
    /// Capability name.
    pub name: String,
    /// Exit code of the `check capability` run (0 / 1 / 2).
    pub exit: i32,
    /// [`input_hash`] at the run; `None` when it could not be computed.
    pub input_hash: Option<String>,
    /// [`VERSION`] of the sitecraft that ran the check.
    pub sitecraft_version: String,
    /// UTC RFC 3339 time the record was written.
    pub at: String,
}

/// Write the record of one run (temp file in the same directory, then
/// rename). Any failure is `Err` (the caller turns it into exit 2); the temp
/// file is removed on failure.
pub fn write_record(
    state: &Path,
    root: &Path,
    name: &str,
    exit: i32,
    input_hash: Option<&str>,
) -> Result<PathBuf, String> {
    let path = record_path(state, root, name)?;
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let at = crate::decide::record::now_utc_rfc3339()?;
    let rec = ResultRecord {
        name: name.to_string(),
        exit,
        input_hash: input_hash.map(str::to_string),
        sitecraft_version: VERSION.to_string(),
        at,
    };
    let body =
        serde_json::to_string(&rec).map_err(|e| format!("cannot encode the result record: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".{name}.json.{}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)
    };
    match write() {
        Ok(()) => Ok(path),
        Err(e) => {
            // Best-effort cleanup; the write error is what makes this Err.
            let _cleanup = std::fs::remove_file(&tmp);
            Err(format!("cannot write {}: {e}", path.display()))
        }
    }
}

/// What the threejs-render input set needs from `sitecraft.toml`.
#[derive(Debug, Clone, Copy)]
pub struct WebInputs<'a> {
    /// The exact `sitecraft.toml` bytes (already trust-checked by the caller).
    pub toml_bytes: &'a [u8],
    /// Its `web_globs`.
    pub web_globs: &'a [String],
}

/// Is a path component free of glob metacharacters?
fn literal(c: &str) -> bool {
    !c.chars()
        .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}' | '\\'))
}

fn globset(globs: &[String]) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        b.add(Glob::new(g).map_err(|e| format!("glob {g:?} does not compile: {e}"))?);
    }
    b.build()
        .map_err(|e| format!("cannot build the input globs: {e}"))
}

/// Root-relative `/`-joined form of `rel`, or `Err` for a path that is not a
/// plain relative path.
fn rel_string(rel: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => return Err(format!("{} is not a root-relative path", rel.display())),
        }
    }
    Ok(parts.join("/"))
}

/// Every regular file under `root` whose root-relative path matches one of
/// `globs`, keyed by that path. Only the directories below each glob's
/// literal leading components are walked. Symlinks are followed (a loop back
/// to an ancestor is skipped); any IO error is `Err`.
pub fn collect(root: &Path, globs: &[String]) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut out = BTreeMap::new();
    if globs.is_empty() {
        return Ok(out);
    }
    let set = globset(globs)?;
    let mut budget = MAX_WALK_ENTRIES;
    for g in globs {
        let comps: Vec<&str> = g.split('/').collect();
        let lead: Vec<&str> = comps.iter().copied().take_while(|c| literal(c)).collect();
        if lead.len() == comps.len() {
            // A literal path: it is either that file or nothing.
            let rel = PathBuf::from(g);
            let p = root.join(&rel);
            match std::fs::metadata(&p) {
                Ok(m) if m.is_file() => {
                    out.insert(rel_string(&rel)?, p);
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("cannot inspect {}: {e}", p.display())),
            }
            continue;
        }
        let base_rel: PathBuf = lead.iter().filter(|c| !c.is_empty()).collect();
        let base = root.join(&base_rel);
        match std::fs::metadata(&base) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot inspect {}: {e}", base.display())),
        }
        let mut stack = Vec::new();
        walk(&base, &base_rel, &set, &mut out, &mut stack, &mut budget)?;
    }
    Ok(out)
}

fn walk(
    dir: &Path,
    rel: &Path,
    set: &GlobSet,
    out: &mut BTreeMap<String, PathBuf>,
    stack: &mut Vec<PathBuf>,
    budget: &mut usize,
) -> Result<(), String> {
    let canon = std::fs::canonicalize(dir)
        .map_err(|e| format!("cannot resolve directory {}: {e}", dir.display()))?;
    if stack.contains(&canon) {
        return Ok(());
    }
    let rd = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read directory {}: {e}", dir.display()))?;
    stack.push(canon);
    for ent in rd {
        let ent = ent.map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        if *budget == 0 {
            return Err(format!(
                "more than {MAX_WALK_ENTRIES} entries under the input globs; narrow the globs"
            ));
        }
        *budget -= 1;
        let path = ent.path();
        let child_rel = rel.join(ent.file_name());
        let meta = std::fs::metadata(&path)
            .map_err(|e| format!("cannot inspect {} (dangling link?): {e}", path.display()))?;
        if meta.is_dir() {
            walk(&path, &child_rel, set, out, stack, budget)?;
        } else if meta.is_file() && set.is_match(&child_rel) {
            out.insert(rel_string(&child_rel)?, path);
        }
    }
    stack.pop();
    Ok(())
}

/// The D5 input hash of capability `name` (module docs).
pub fn input_hash(
    root: &Path,
    name: &str,
    table: &toml::Value,
    verify: &Verify,
    web: Option<WebInputs<'_>>,
) -> Result<String, String> {
    // (root-relative path → content sha256 hex)
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut add_all = |found: BTreeMap<String, PathBuf>| -> Result<(), String> {
        for (rel, p) in found {
            let bytes =
                std::fs::read(&p).map_err(|e| format!("cannot read input {}: {e}", p.display()))?;
            files.insert(rel, hex(&bytes));
        }
        Ok(())
    };
    match verify {
        Verify::Blender { script, inputs, .. } => {
            let p = root.join(script);
            let bytes = std::fs::read(&p)
                .map_err(|e| format!("cannot read the capability script {}: {e}", p.display()))?;
            add_all(collect(root, inputs)?)?;
            files.insert(rel_string(Path::new(script))?, hex(&bytes));
        }
        Verify::Threejs { inputs, .. } => {
            let Some(w) = web else {
                return Err(format!(
                    "capability `{name}` (threejs-render) needs a trusted sitecraft.toml for its input set"
                ));
            };
            add_all(collect(root, w.web_globs)?)?;
            add_all(collect(root, inputs)?)?;
            files.insert(crate::config::FILE_NAME.to_string(), hex(w.toml_bytes));
        }
        Verify::Command { inputs, .. } => {
            let found = collect(root, inputs)?;
            if found.is_empty() {
                return Err(format!(
                    "capability `{name}`: verify.inputs {inputs:?} match no file under {}, so its inputs cannot be pinned; fix verify.inputs",
                    root.display()
                ));
            }
            add_all(found)?;
        }
    }
    let mut buf = capability::canonical_json(table);
    for (rel, h) in &files {
        buf.push_str(&serde_json::Value::String(rel.clone()).to_string());
        buf.push_str(&serde_json::Value::String(h.clone()).to_string());
    }
    Ok(hex(buf.as_bytes()))
}

/// Is `p` an executable regular file?
#[must_use]
pub fn is_executable_file(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The Stop gate's capability check (D5). Runs nothing.
///
/// Contract: for every relevant capability, `Clean` only when its record
/// exists, parses, names it, has `exit == 0`, the current [`VERSION`], and an
/// `input_hash` equal to the one recomputed now. A missing record, a failed
/// run, another version or a changed input is a `Violation`; an unreadable /
/// unparseable record or an input hash that cannot be recomputed is
/// `Undetermined`. Every non-clean reason names the capability and
/// `sitecraft check capability <name>`. No relevant capability → `Clean`
/// (nothing is added). The per-capability verdicts are combined with
/// [`Verdict::worst_of`], so every failing capability is listed.
pub fn freshness(
    state: &Path,
    root: &Path,
    raw: &BTreeMap<String, toml::Value>,
    caps: &BTreeMap<String, Capability>,
    web: Option<WebInputs<'_>>,
) -> Verdict {
    let mut out = Vec::new();
    for (name, c) in caps {
        if !c.is_relevant() {
            continue;
        }
        out.push(one(state, root, name, raw.get(name), c, web));
    }
    Verdict::worst_of(out)
}

fn one(
    state: &Path,
    root: &Path,
    name: &str,
    table: Option<&toml::Value>,
    c: &Capability,
    web: Option<WebInputs<'_>>,
) -> Verdict {
    let fix = format!("run `sitecraft check capability {name}` and fix it until it exits 0");
    let viol = |why: String| Verdict::violation(format!("capability {name}: {why}; {fix}"));
    let undet = |why: String| Verdict::undetermined(format!("capability {name}: {why}; {fix}"));
    let (Some(table), Some(verify)) = (table, c.verify.as_ref()) else {
        return undet("the capability has no verify table, so it cannot have been checked".into());
    };
    let path = match record_path(state, root, name) {
        Ok(p) => p,
        Err(e) => return undet(e),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return viol(format!(
                "not verified (no result record {})",
                path.display()
            ))
        }
        Err(e) => {
            return undet(format!(
                "cannot read the result record {}: {e}",
                path.display()
            ))
        }
    };
    let rec: ResultRecord = match serde_json::from_str(&text) {
        Ok(r) => r,
        Err(e) => {
            return undet(format!(
                "the result record {} does not parse: {e}",
                path.display()
            ))
        }
    };
    if rec.name != name {
        return undet(format!(
            "the result record {} names `{}`",
            path.display(),
            rec.name
        ));
    }
    if rec.exit != 0 {
        return viol(format!(
            "the last check (at {}) exited {}",
            rec.at, rec.exit
        ));
    }
    if rec.sitecraft_version != VERSION {
        return viol(format!(
            "the last check was run by sitecraft {} (now {VERSION})",
            rec.sitecraft_version
        ));
    }
    let now = match input_hash(root, name, table, verify, web) {
        Ok(h) => h,
        Err(e) => return undet(format!("the input hash cannot be recomputed: {e}")),
    };
    match rec.input_hash {
        Some(h) if h == now => Verdict::from_findings(Vec::new()),
        _ => viol(format!(
            "its inputs or its [capabilities.{name}] table changed since the last check (at {})",
            rec.at
        )),
    }
}
