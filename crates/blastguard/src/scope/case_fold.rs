//! Case-only spelling differences on a case-insensitive filesystem (#1630).
//! Kept apart from `scope.rs` for the file-size cap.

/// True ONLY when `typed` (`<real parent>/<name as spelled>`) and `real` (what
/// the resolver returned for that spelling) are the SAME directory entry whose
/// names differ only in letter case — what a case-insensitive filesystem
/// (default APFS) reports when the on-disk name is `mergeMain.log` and the
/// command spells `mergemain.log`. That is not a symlink, so
/// [`super::SafeRoots::no_symlink_below`] must not read it as one.
///
/// Every condition is checked on the real filesystem and any failure answers
/// `false` (the caller then keeps "symlink below the root"):
///   * `real` has the same parent as `typed`, and its last component differs
///     from the typed one but is equal under Unicode lowercasing;
///   * `lstat(typed)` and `lstat(real)` both succeed and NEITHER is a symlink
///     (a symlink named with the other case fails here — `lstat` does not
///     follow the final component);
///   * both `lstat`s report the same `(st_dev, st_ino)`. On a case-sensitive
///     filesystem a differently-cased `typed` either does not exist or is a
///     separate entry whose realpath would have been `typed` itself, so this
///     holds only where the filesystem really folds case.
pub(super) fn is_case_only_same_entry(typed: &str, real: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Some((tp, tname)), Some((rp, rname))) = (typed.rsplit_once('/'), real.rsplit_once('/'))
    else {
        return false;
    };
    if tp != rp || tname == rname || tname.to_lowercase() != rname.to_lowercase() {
        return false;
    }
    let (Ok(t), Ok(r)) = (
        std::fs::symlink_metadata(typed),
        std::fs::symlink_metadata(real),
    ) else {
        return false;
    };
    !t.file_type().is_symlink()
        && !r.file_type().is_symlink()
        && t.dev() == r.dev()
        && t.ino() == r.ino()
}
