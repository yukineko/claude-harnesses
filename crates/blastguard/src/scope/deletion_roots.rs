//! The temp and cache DELETION roots for [`SafeRoots`](super::SafeRoots)
//! (module doc of [`crate::scope`], "Temp and cache roots"). Split out of
//! `scope.rs` unchanged (file-growth cap).

#[cfg(doc)]
use super::TEMP_ROOTS;
use super::{
    is_acceptable_derived_root, normalize_abs, raw_session_path_refusal, real_root_below_anchor,
    resolve_with, DirProbe, RealPathResolver, NEVER_A_ROOT,
};

/// The fixed temp roots a recursive `rm` may delete inside WITHOUT
/// confirmation (spec 3aa215e1 class 2). Narrower than [`TEMP_ROOTS`] on
/// purpose: `/var/tmp` survives reboots and is not listed by the ruling, so it
/// keeps only the safe-root `Ask`.
pub(super) const DELETION_TEMP_ROOTS: &[&str] = &["/tmp", "/private/tmp"];

/// The cache roots below `$HOME` (spec 3aa215e1 class 3), as components below
/// the canonicalised home directory.
const DELETION_CACHE_ROOTS: &[&[&str]] = &[&[".cache"], &["Library", "Caches"]];

/// True when `root` is `home` or an ancestor of it — a root that would put the
/// whole home directory inside a delete-freely region.
fn contains_home(root: &str, homes: &[&str]) -> bool {
    homes.iter().any(|h| {
        *h == root || root == "/" || h.starts_with(&format!("{}/", root.trim_end_matches('/')))
    })
}

/// True when `root` is strictly below one of `homes`. A TEMP root there would
/// make an ordinary home subtree (`$TMPDIR=$HOME/src`) delete-freely; only the
/// fixed cache roots may live below home.
fn below_home(root: &str, homes: &[&str]) -> bool {
    homes
        .iter()
        .any(|h| root.starts_with(&format!("{}/", h.trim_end_matches('/'))))
}

/// Prefixes under which the OS's per-user temp directory lives (macOS
/// `$TMPDIR` = `/var/folders/<xx>/<id>/T`). A deletion root strictly below
/// one of these is not refused as a system directory; everything else
/// [`crate::deletion::is_system_path`] names is.
const OS_TEMP_PARENTS: &[&str] = &["/private/var/folders", "/var/folders"];

/// True when `root` is a system directory in the sense of
/// [`crate::deletion::is_system_path`] and not strictly below
/// [`OS_TEMP_PARENTS`].
fn system_root(root: &str) -> bool {
    crate::deletion::is_system_path(root)
        && !OS_TEMP_PARENTS
            .iter()
            .any(|p| root.starts_with(&format!("{p}/")))
}

/// Build the temp and cache DELETION roots (module doc, "Temp and cache
/// roots"). Same filesystem discipline as the worktree storage roots: each
/// must exist, right now, as a real directory with no symlink below its
/// anchor ([`real_root_below_anchor`]). `$TMPDIR` and `$HOME` are refused on
/// their RAW spelling (a `..`/`.` component, or not absolute) exactly like the
/// payload cwd ([`raw_session_path_refusal`]). No root may be on
/// [`NEVER_A_ROOT`], be `/`, be a system directory
/// ([`crate::deletion::is_system_path`] — `$TMPDIR=/etc` resolves to
/// `/private/etc`, which is not on [`NEVER_A_ROOT`]; only the OS per-user
/// temp dir under `/var/folders` is exempt), or be the home directory or one
/// of its ancestors (every spelling of HOME counts for that, refused or not).
/// A TEMP root may also not be strictly below the home directory
/// (`$TMPDIR=$HOME/src` would make `~/src` delete-freely). `$TMPDIR` is not a
/// root at all when HOME is absent (those checks would have nothing to compare
/// against).
pub(super) fn deletion_roots(
    tmpdir: Option<&str>,
    home: Option<&str>,
    resolver: Option<RealPathResolver>,
    probe: Option<DirProbe>,
) -> (Vec<String>, Vec<String>) {
    let mut temp: Vec<String> = Vec::new();
    let mut cache: Vec<String> = Vec::new();
    let Some(probe) = probe else {
        return (temp, cache);
    };
    // For EXCLUSION ("no root may contain the home directory") every spelling
    // of HOME counts, refused or not; for ANCHORING the cache roots only an
    // unrefused one does.
    let home_any = home.and_then(normalize_abs);
    let home_any_real = home_any.as_deref().and_then(|h| resolve_with(h, resolver));
    let home_norm = home
        .filter(|h| raw_session_path_refusal(h, true).is_none())
        .and_then(normalize_abs);
    let homes: Vec<&str> = [home_any.as_deref(), home_any_real.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    let acceptable = |root: &str| {
        root.starts_with('/')
            && !NEVER_A_ROOT.contains(&root)
            && !system_root(root)
            && !contains_home(root, &homes)
    };
    let acceptable_temp = |root: &str| acceptable(root) && !below_home(root, &homes);
    for fixed in DELETION_TEMP_ROOTS {
        if let Some(r) = real_root_below_anchor(fixed, &[], resolver, probe) {
            if acceptable_temp(&r) && !temp.contains(&r) {
                temp.push(r);
            }
        }
    }
    // `$TMPDIR` is only trusted when it can be checked against a known home
    // directory: with no HOME at all, "is this TMPDIR the home directory or
    // above it?" has no answer, so it is not a root (restrictive side).
    let tmpdir = tmpdir.filter(|_| !homes.is_empty());
    if let Some(t) = tmpdir.filter(|t| raw_session_path_refusal(t, true).is_none()) {
        if let Some(norm) = normalize_abs(t) {
            if let Some(r) = real_root_below_anchor(&norm, &[], resolver, probe) {
                if acceptable_temp(&r)
                    && is_acceptable_derived_root(&r, home_norm.as_deref())
                    && !temp.contains(&r)
                {
                    temp.push(r);
                }
            }
        }
    }
    if let Some(h) = home_norm.as_deref() {
        for below in DELETION_CACHE_ROOTS {
            if let Some(r) = real_root_below_anchor(h, below, resolver, probe) {
                if acceptable(&r)
                    && is_acceptable_derived_root(&r, home_norm.as_deref())
                    && !cache.contains(&r)
                {
                    cache.push(r);
                }
            }
        }
    }
    (temp, cache)
}
