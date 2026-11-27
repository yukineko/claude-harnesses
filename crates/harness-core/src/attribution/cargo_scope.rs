//! Where a `cargo fmt` can write (backlog ff46a113; user ruling 2026-11-21:
//! limit scope to the effective cwd ONLY for writers statically bounded to the
//! cwd's cargo workspace; anything that can write elsewhere keeps the full set).

use std::path::{Path, PathBuf};

use super::{plain_word, Stage};

/// The directory whose files `stage` can change, from a known absolute `cwd`;
/// `None` = it may write anywhere (the caller treats that as global).
///
/// Only `cargo fmt` with no redirect and, besides, `--check`, `-q`/`--quiet`,
/// `-v`/`--verbose`, `-p`/`--package NAME` is bounded; its scope is the cargo
/// workspace containing `cwd` ([`workspace_root`]), never `cwd` itself (`-p`
/// and the workspace's other members can live outside it). Everything else is
/// `None`: `--all` (it also formats local path dependencies outside the
/// workspace), `--manifest-path`, `--`, a toolchain or env prefix, a redirect,
/// a relative `cwd`, an unreadable manifest, and any `fmt` alias in a
/// `.cargo/config*` of `cwd`'s ancestry or of the cargo home (cargo runs the
/// alias instead of the built-in `fmt`).
pub(super) fn cargo_fmt_scope(stage: &Stage, cwd: &Path) -> Option<PathBuf> {
    if !stage.redirects.is_empty() || !cwd.is_absolute() {
        return None;
    }
    let w: Vec<&str> = stage.words.iter().map(String::as_str).collect();
    if w.len() < 2 || w[0] != "cargo" || w[1] != "fmt" {
        return None;
    }
    let mut it = w[2..].iter().copied();
    while let Some(a) = it.next() {
        match a {
            "--check" | "-q" | "--quiet" | "-v" | "--verbose" => {}
            "-p" | "--package" => {
                if !it.next().is_some_and(plain_word) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    if fmt_alias_may_exist(cwd) {
        return None;
    }
    match workspace_root(cwd) {
        Ok(Some(root)) => Some(root),
        // No Cargo.toml anywhere above: `cargo fmt` fails and writes nothing.
        Ok(None) => Some(cwd.to_path_buf()),
        Err(()) => None,
    }
}

/// The workspace root cargo uses for `cwd`: the nearest ancestor manifest that
/// has a `[workspace…]` table, else the directory of the nearest manifest
/// (a lone package). `Ok(None)`: no manifest. `Err`: a manifest that exists
/// but cannot be read.
fn workspace_root(cwd: &Path) -> Result<Option<PathBuf>, ()> {
    let mut package: Option<PathBuf> = None;
    for dir in cwd.ancestors() {
        match std::fs::read_to_string(dir.join("Cargo.toml")) {
            Ok(t) => {
                if t.lines().any(|l| l.trim_start().starts_with("[workspace")) {
                    return Ok(Some(dir.to_path_buf()));
                }
                package.get_or_insert_with(|| dir.to_path_buf());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(()),
        }
    }
    Ok(package)
}

/// Whether a cargo config that applies to `cwd` may define an alias `fmt`.
/// A config that exists but cannot be read counts as "may".
fn fmt_alias_may_exist(cwd: &Path) -> bool {
    let mut dirs: Vec<PathBuf> = cwd.ancestors().map(|d| d.join(".cargo")).collect();
    if let Some(h) = std::env::var_os("CARGO_HOME") {
        dirs.push(PathBuf::from(h));
    } else if let Some(h) = std::env::var_os("HOME") {
        dirs.push(Path::new(&h).join(".cargo"));
    }
    for d in dirs {
        for name in ["config.toml", "config"] {
            match std::fs::read_to_string(d.join(name)) {
                Ok(t) => {
                    if text_may_define_fmt_alias(&t) {
                        return true;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return true,
            }
        }
    }
    false
}

/// `fmt` as a key of `[alias]`, `[alias.fmt]`, or a dotted `alias.fmt = …`.
fn text_may_define_fmt_alias(text: &str) -> bool {
    let mut in_alias = false;
    for raw in text.lines() {
        let l = raw.trim();
        if l.starts_with('[') {
            let name = l.trim_matches(|c| c == '[' || c == ']').trim();
            if name == "alias.fmt" || name == "alias.\"fmt\"" {
                return true;
            }
            in_alias = name == "alias";
        } else if l.starts_with("alias.fmt") || l.starts_with("alias.\"fmt\"") {
            return true;
        } else if in_alias {
            let key = l.split('=').next().unwrap_or("").trim().trim_matches('"');
            if key == "fmt" {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn stage(cmd: &str) -> Stage {
        Stage {
            words: cmd.split_whitespace().map(String::from).collect(),
            ..Stage::default()
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hc-cargo-scope-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn scope_is_the_workspace_root_not_the_cwd() {
        let d = tmp("ws");
        std::fs::create_dir_all(d.join("a/src")).unwrap();
        std::fs::write(d.join("Cargo.toml"), "[workspace]\nmembers = [\"a\"]\n").unwrap();
        std::fs::write(d.join("a/Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
        assert_eq!(
            cargo_fmt_scope(&stage("cargo fmt"), &d.join("a")),
            Some(d.clone())
        );
        assert_eq!(
            cargo_fmt_scope(&stage("cargo fmt -p b"), &d.join("a/src")),
            Some(d.clone())
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn all_and_unlisted_forms_are_global() {
        let d = tmp("global");
        std::fs::write(d.join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
        for c in [
            "cargo fmt --all",
            "cargo fmt --manifest-path x",
            "cargo fmt -- --check",
            "cargo fmt -p",
            "cargo +nightly fmt",
        ] {
            assert_eq!(cargo_fmt_scope(&stage(c), &d), None, "{c}");
        }
        assert_eq!(cargo_fmt_scope(&stage("cargo fmt"), Path::new("rel")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_fmt_alias_in_the_cwd_ancestry_is_global() {
        for cfg in [
            "[alias]\nfmt = \"run -p evil\"\n",
            "[alias.fmt]\nx = 1\n",
            "alias.fmt = \"run\"\n",
        ] {
            let d = tmp("alias");
            std::fs::create_dir_all(d.join(".cargo")).unwrap();
            std::fs::create_dir_all(d.join("sub")).unwrap();
            std::fs::write(d.join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
            std::fs::write(d.join(".cargo/config.toml"), cfg).unwrap();
            assert_eq!(
                cargo_fmt_scope(&stage("cargo fmt"), &d.join("sub")),
                None,
                "{cfg}"
            );
            let _ = std::fs::remove_dir_all(&d);
        }
        let d = tmp("noalias");
        std::fs::create_dir_all(d.join(".cargo")).unwrap();
        std::fs::write(d.join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
        std::fs::write(d.join(".cargo/config.toml"), "[alias]\nb = \"build\"\n").unwrap();
        assert_eq!(cargo_fmt_scope(&stage("cargo fmt"), &d), Some(d.clone()));
        let _ = std::fs::remove_dir_all(&d);
    }
}
