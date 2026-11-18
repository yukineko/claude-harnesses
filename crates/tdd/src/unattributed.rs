//! Path-less writes keep the full scope (user ruling 2026-11-18 supersedes
//! 2026-10-17).
//!
//! `scope_to_session` narrows to the files this session's transcript shows it
//! writing. A session Bash call that may write but names no existing path
//! (`cargo fmt`, a glob `sed -i`, a generator) could have changed any file the
//! narrowing would exclude, so narrowing past it silently skips the gate for
//! those files (CLAUDE.md 3). Unreadable Bash evidence is the same.

use harness_core::verdict::Determination;

/// `Some(reason)` when the scope must NOT be narrowed past `excluded`; `None`
/// when nothing is excluded or the session provably ran no path-less write.
pub fn why_not_narrowable(transcript_path: &str, excluded: &[String]) -> Option<String> {
    if excluded.is_empty() {
        return None;
    }
    match harness_core::attribution::has_unattributable_write(transcript_path) {
        Determination::Known(false) => None,
        Determination::Known(true) => {
            Some("this session ran a Bash command that may write without naming a path".to_string())
        }
        Determination::Undetermined(why) => Some(format!(
            "this session's Bash writes cannot be read ({})",
            why.as_str()
        )),
    }
}
