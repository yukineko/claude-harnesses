//! Package / target lookups for rule 4 (task scope) of `condukt testscope`:
//! which packages a task worktree changed, a manifest's package name, a
//! package's lib / bin unit-test targets, and the `-p` specs of a command.
//!
//! Moved here from `testscope.rs` to keep that file under the file-growth cap
//! (backlog f7d0e13b, 2026-11-22). New with that backlog item:
//! [`package_unit_targets`], and [`worktree_changed_packages`] now returns each
//! package's manifest directory beside its name (which packages it reports is
//! unchanged). [`manifest_package_name`] and [`package_specs`] are unchanged.

use std::path::{Path, PathBuf};

use super::git_stdout;

/// The packages owning the paths the worktree at `wt` added, changed or
/// deleted against `base` (the same three sources as
/// [`super::worktree_changed_tests`], read with `-z`). A path is owned by the package
/// of the nearest `Cargo.toml` with a `[package]` table at or above the path's
/// directory, within the worktree's top level; a path with no such manifest
/// (a root `Cargo.lock`, `docs/…`, a virtual-workspace-only path) adds no
/// package — it never widens the set. A failing git, or a manifest that
/// exists but cannot be read or parsed, or whose `[package].name` is not a
/// string, is an `Err` (undetermined; the caller denies). Each package comes
/// with the directory of its manifest (where its targets are looked up, see
/// [`package_unit_targets`]).
pub(super) fn worktree_changed_packages(
    wt: &Path,
    base: &str,
) -> Result<Vec<(String, PathBuf)>, String> {
    let top = PathBuf::from(
        git_stdout(wt, &["rev-parse", "--show-toplevel"])?
            .trim()
            .to_string(),
    );
    let range = format!("{base}...HEAD");
    let mut raw = String::new();
    for args in [
        &["diff", "--name-only", "-z", &range][..],
        &["diff", "--name-only", "-z", "HEAD"][..],
        &["ls-files", "--others", "--exclude-standard", "-z"][..],
    ] {
        raw.push_str(&git_stdout(&top, args)?);
        raw.push('\0');
    }
    // dir -> package named by `dir/Cargo.toml` (None: no manifest there, or a
    // manifest without `[package]`).
    let mut seen: std::collections::HashMap<PathBuf, Option<String>> =
        std::collections::HashMap::new();
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for rel in raw.split('\0').filter(|s| !s.is_empty()) {
        let mut dir = match top.join(rel).parent() {
            Some(d) => d.to_path_buf(),
            None => continue,
        };
        while dir.starts_with(&top) {
            let pkg = match seen.get(&dir) {
                Some(p) => p.clone(),
                None => {
                    let p = manifest_package_name(&dir.join("Cargo.toml"))?;
                    seen.insert(dir.clone(), p.clone());
                    p
                }
            };
            if let Some(name) = pkg {
                if !out.iter().any(|(n, _)| *n == name) {
                    out.push((name, dir.clone()));
                }
                break;
            }
            if dir == top || !dir.pop() {
                break;
            }
        }
    }
    Ok(out)
}

/// The `[package].name` of the manifest at `path`. `Ok(None)` when no file
/// exists there (an observed absence) or the manifest has no `[package]`
/// table (a virtual workspace root); `Err` when it exists but cannot be read
/// or parsed, or its name is not a string.
pub(super) fn manifest_package_name(path: &Path) -> Result<Option<String>, String> {
    let txt = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read manifest {}: {e}", path.display())),
    };
    let v: toml::Value = toml::from_str(&txt)
        .map_err(|e| format!("unparseable manifest {}: {e}", path.display()))?;
    match v.get("package") {
        None => Ok(None),
        Some(p) => match p.get("name").and_then(toml::Value::as_str) {
            Some(n) => Ok(Some(n.to_string())),
            None => Err(format!(
                "manifest {} has a [package] table without a string name",
                path.display()
            )),
        },
    }
}

/// The unit-test targets of the package whose manifest is `dir/Cargo.toml`:
/// whether it has a lib target, and the names of its bin targets. A lib is a
/// `[lib]` table or, unless `autolib = false`, an existing `src/lib.rs`. A bin
/// is a `[[bin]]` entry's `name` or, unless `autobins = false`, `src/main.rs`
/// (named after the package), `src/bin/<n>.rs` or `src/bin/<n>/main.rs`.
/// Discovery cases this does not model (a `[[bin]]` without `name`, a
/// non-default `path`) only ever leave a target out, which refuses it. A
/// manifest that cannot be read or parsed, a `[lib]` / `[[bin]]` / `auto*`
/// key of the wrong shape, or a `src/bin` that exists but cannot be listed is
/// an `Err` (undetermined; the caller denies).
pub(super) fn package_unit_targets(dir: &Path) -> Result<(bool, Vec<String>), String> {
    let path = dir.join("Cargo.toml");
    let txt = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read manifest {}: {e}", path.display()))?;
    let v: toml::Value = toml::from_str(&txt)
        .map_err(|e| format!("unparseable manifest {}: {e}", path.display()))?;
    let pkg = v
        .get("package")
        .ok_or_else(|| format!("manifest {} has no [package] table", path.display()))?;
    let name = pkg
        .get("name")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("manifest {} has no string package name", path.display()))?;
    let auto = |key: &str| -> Result<bool, String> {
        match pkg.get(key) {
            None => Ok(true),
            Some(toml::Value::Boolean(b)) => Ok(*b),
            Some(_) => Err(format!(
                "manifest {}: `{key}` is not a boolean",
                path.display()
            )),
        }
    };
    let has_lib = match v.get("lib") {
        Some(toml::Value::Table(_)) => true,
        Some(_) => return Err(format!("manifest {}: `lib` is not a table", path.display())),
        None => auto("autolib")? && dir.join("src/lib.rs").is_file(),
    };
    let mut bins: Vec<String> = Vec::new();
    match v.get("bin") {
        None => {}
        Some(toml::Value::Array(entries)) => {
            for e in entries {
                let toml::Value::Table(t) = e else {
                    return Err(format!(
                        "manifest {}: a `[[bin]]` entry is not a table",
                        path.display()
                    ));
                };
                if let Some(n) = t.get("name").and_then(toml::Value::as_str) {
                    bins.push(n.to_string());
                }
            }
        }
        Some(_) => {
            return Err(format!(
                "manifest {}: `bin` is not an array of tables",
                path.display()
            ))
        }
    }
    if auto("autobins")? {
        if dir.join("src/main.rs").is_file() {
            bins.push(name.to_string());
        }
        let bin_dir = dir.join("src/bin");
        match std::fs::read_dir(&bin_dir) {
            Ok(rd) => {
                for entry in rd {
                    let entry = entry
                        .map_err(|e| format!("unreadable entry in {}: {e}", bin_dir.display()))?;
                    let p = entry.path();
                    let stem = if p.is_dir() && p.join("main.rs").is_file() {
                        p.file_name().and_then(|n| n.to_str()).map(str::to_string)
                    } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                        p.file_stem().and_then(|n| n.to_str()).map(str::to_string)
                    } else {
                        None
                    };
                    if let Some(s) = stem {
                        bins.push(s);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot list {}: {e}", bin_dir.display())),
        }
    }
    Ok((has_lib, bins))
}

/// The package specs of `--package <spec>` / `--package=<spec>` and of every
/// short-flag cluster among `words` (options before `--` only). A cluster is
/// a word with a single leading `-` (`-p x`, `-px`, `-p=x`, `-qp x`, `-qpx`,
/// `-vp x`): cargo parses it letter by letter, so it is parsed the same way
/// here. Value-less letters `q`, `v`, `h`, `r` are skipped; `p` takes the
/// rest of the cluster (a leading `=` dropped) or else the next word as a
/// spec; `j`, `F`, `Z` take the rest or the next word as a value that is not a
/// spec. Any other letter, or a value-taking letter with no value, is `Err`:
/// whether the cluster carries a `p` is undetermined.
pub(super) fn package_specs(words: &[String]) -> Result<Vec<String>, String> {
    let opts: Vec<&String> = words.iter().take_while(|a| a.as_str() != "--").collect();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(a) = opts.get(i) {
        let a = a.as_str();
        if a == "--package" {
            match opts.get(i + 1).filter(|v| !v.starts_with('-')) {
                Some(v) => {
                    out.push(v.to_string());
                    i += 1;
                }
                None => return Err(format!("`{a}` names no package")),
            }
        } else if let Some(v) = a.strip_prefix("--package=") {
            out.push(v.to_string());
        } else if let Some(cluster) = a.strip_prefix('-').filter(|c| !c.starts_with('-')) {
            for (pos, c) in cluster.char_indices() {
                match c {
                    'q' | 'v' | 'h' | 'r' => {}
                    'p' | 'j' | 'F' | 'Z' => {
                        let rest = &cluster[pos + c.len_utf8()..];
                        let value = if rest.is_empty() {
                            let next = opts.get(i + 1).map(|s| s.as_str());
                            match next.filter(|v| c != 'p' || !v.starts_with('-')) {
                                Some(v) => {
                                    i += 1;
                                    v.to_string()
                                }
                                None => {
                                    return Err(format!("`-{c}` in `{a}` has no value"));
                                }
                            }
                        } else {
                            rest.strip_prefix('=').unwrap_or(rest).to_string()
                        };
                        if c == 'p' {
                            out.push(value);
                        }
                        break;
                    }
                    other => {
                        return Err(format!(
                            "short-flag cluster `{a}` has `-{other}`, which is not modelled, so \
                             whether it selects a package is undetermined"
                        ));
                    }
                }
            }
        }
        i += 1;
    }
    Ok(out)
}
