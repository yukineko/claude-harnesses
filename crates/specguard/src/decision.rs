//! Decision records (ADRs): the *why* behind a canon change, pinned to the
//! canon commit it was made against.
//!
//! The harness only *lists* and *scaffolds* these notes — it never parses their
//! semantics. The D3 audit (see the decisions prompt) makes the agent read each
//! record's live content, extract its canon pins / drivers / review-when, and
//! check them against the live canon. This keeps the one-truth discipline: the
//! record is evidence pinned to a commit, never a second authority.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Resolve the decisions directory. Empty `dir` disables the feature (None).
/// Absolute paths (e.g. an Obsidian vault) are used as-is; relative paths
/// resolve under `repo_root`.
pub fn resolve_dir(repo_root: &Path, dir: &str) -> Option<PathBuf> {
    let dir = dir.trim();
    if dir.is_empty() {
        return None;
    }
    let p = Path::new(dir);
    Some(if p.is_absolute() {
        p.to_path_buf()
    } else {
        repo_root.join(p)
    })
}

/// List decision record files (`*.md`) under the decisions dir, sorted. Returns
/// absolute path strings the read-only agent can open. `Ok(vec![])` when the
/// feature is disabled or the dir is ABSENT (legitimately "no decision
/// records"). Fails closed (`Err`) when the dir exists but is unreadable, or an
/// individual entry can't be read: incomplete input must NOT masquerade as "no
/// decisions" and silently skip the D3 audit shard → false-GREEN
/// (CA-specguard-001).
pub fn list_files(repo_root: &Path, dir: &str) -> Result<Vec<String>> {
    let Some(d) = resolve_dir(repo_root, dir) else {
        return Ok(vec![]);
    };
    let entries = match std::fs::read_dir(&d) {
        Ok(e) => e,
        // A MISSING dir legitimately means "no decision records". A dir that
        // EXISTS but is unreadable is incomplete input, not "no decisions" →
        // fail closed so the gate can't pass GREEN on a dir it never read.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(err) => {
            return Err(anyhow::Error::new(err)
                .context(format!("cannot read decisions dir '{}'", d.display())));
        }
    };
    let mut files: Vec<String> = Vec::new();
    for entry in entries {
        // A per-entry error means the listing is INCOMPLETE. The old code
        // dropped it (`filter_map(.. Err => None)`), so a decisions dir with an
        // unreadable entry could collapse to an empty list and silently skip the
        // D3 shard. Fail closed instead.
        let de = entry
            .with_context(|| format!("unreadable entry in decisions dir '{}'", d.display()))?;
        let p = de.path();
        if p.extension().is_some_and(|x| x == "md") {
            files.push(p.to_string_lossy().into_owned());
        }
    }
    files.sort();
    Ok(files)
}

/// Slugify a title for the record id. Unicode-aware so Japanese titles keep
/// their characters; runs of non-alphanumerics collapse to a single dash.
pub fn slug(title: &str) -> String {
    let mut s = String::new();
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            for lc in ch.to_lowercase() {
                s.push(lc);
            }
            prev_dash = false;
        } else if !prev_dash && !s.is_empty() {
            s.push('-');
            prev_dash = true;
        }
    }
    while s.ends_with('-') {
        s.pop();
    }
    if s.is_empty() {
        "decision".to_string()
    } else {
        s
    }
}

/// Write a starter decision record pinned to `canon_commit`. Returns the path.
/// Errors (without overwriting) if it already exists and `force` is false.
pub fn scaffold(
    repo_root: &Path,
    dir: &str,
    id: &str,
    title: &str,
    date: &str,
    canon_commit: &str,
    force: bool,
) -> Result<PathBuf> {
    let d = resolve_dir(repo_root, dir).unwrap_or_else(|| repo_root.join("decisions"));
    std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    let path = d.join(format!("{id}.md"));
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    let body = format!(
        "---\n\
         id: {id}\n\
         title: \"{title}\"\n\
         date: {date}\n\
         status: proposed\n\
         canon_commit: {canon_commit}\n\
         canon: []          # この決定が支配する canon ポインタ (file または file:section)\n\
         drivers: []        # 反証可能な理由 (例: \"HMAC 鍵ローテーションが単一署名経路を要求\")\n\
         review_when: \"\"    # この条件が成立したら再検討 (driver が崩れる条件)\n\
         supersedes: []     # 置き換えた決定があれば (例: [[2026-01-01-old]])\n\
         ---\n\n\
         ## 決定\n\n\
         (何を変えたか。中身を写さず canon は `canon:` にポインタで。)\n\n\
         ## 理由 (なぜ)\n\n\
         (反証可能な driver と、却下した代替を書く。`drivers:` / `review_when:` を埋める。)\n\n\
         ## 影響\n\n\
         (関連する実装・canon。`[[他の決定]]` でリンク可。)\n"
    );
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_ascii_and_collapse() {
        assert_eq!(slug("Single Signing Path!"), "single-signing-path");
        assert_eq!(slug("  a -- b  "), "a-b");
        assert_eq!(slug("!!!"), "decision");
    }

    #[test]
    fn slug_keeps_unicode() {
        assert_eq!(slug("署名の単一経路"), "署名の単一経路");
    }

    #[test]
    fn list_files_ok_and_empty_when_dir_absent() {
        let tmp = tempfile::tempdir().unwrap();
        // A decisions dir that does not exist is legit "no decision records".
        let got = list_files(tmp.path(), "decisions").unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn list_files_lists_md_sorted_ignoring_non_md() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("decisions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.md"), "x").unwrap();
        std::fs::write(dir.join("a.md"), "x").unwrap();
        std::fs::write(dir.join("note.txt"), "x").unwrap();
        let got = list_files(tmp.path(), "decisions").unwrap();
        assert_eq!(got.len(), 2, "only .md, sorted: {got:?}");
        assert!(
            got[0].ends_with("a.md") && got[1].ends_with("b.md"),
            "{got:?}"
        );
    }

    /// CA-specguard-001: a decisions dir that EXISTS but is unreadable must fail
    /// closed (`Err`), not silently return an empty list — an empty list skips
    /// the D3 audit shard → false-GREEN on incomplete input. Unix-only
    /// (chmod-based unreadability); `#[cfg(unix)]` per repo convention.
    #[cfg(unix)]
    #[test]
    fn list_files_fails_closed_on_unreadable_dir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("decisions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.md"), "x").unwrap();

        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o000); // unreadable/unsearchable
        std::fs::set_permissions(&dir, perms).unwrap();

        let got = list_files(tmp.path(), "decisions");

        // Restore perms so tempdir cleanup can remove it (before asserting).
        let mut restore = std::fs::metadata(&dir).unwrap().permissions();
        restore.set_mode(0o700);
        let _ = std::fs::set_permissions(&dir, restore);

        assert!(
            got.is_err(),
            "unreadable decisions dir must fail closed, got {got:?}"
        );
    }
}
