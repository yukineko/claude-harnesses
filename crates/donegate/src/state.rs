//! Per-session attempt counter — a thin re-export of the shared gate state in
//! `harness_core::gate::state`. donegate only uses `bump`/`reset` (the
//! `last_hash` field of the shared `SessionState` stays empty for this gate).

pub use harness_core::gate::state::{bump, reset};

// ---------------------------------------------------------------------------
// Identical-finding memory (backlog b80023b3).
//
// donegate remembers, per session and per root, the signature of the last red
// verdict it BLOCKED on: a hash of the root's tree content (tracked diff vs
// HEAD + every untracked-not-ignored file's bytes) and of each blocking
// check's name / exit status / output tail. A later stop whose signature is
// equal has nothing new to verify, so the caller waives the block (loudly, via
// a systemMessage — never silently). Fail-closed: if the tree fingerprint
// cannot be computed, `tree_fingerprint` returns `None` and the caller keeps
// blocking.
// ---------------------------------------------------------------------------

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

fn git_out(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = Command::new("git");
    harness_core::git_env::scrub(&mut cmd);
    let out = cmd.current_dir(root).args(args).output().ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}

/// Content fingerprint of `root`'s working tree (HEAD commit, tracked changes
/// staged or not, and untracked-not-ignored files). `None` means it could not
/// be determined (git failure, unreadable file) — the caller must treat that
/// as "the tree changed" and block.
pub fn tree_fingerprint(root: &Path) -> Option<String> {
    let mut h = DefaultHasher::new();
    git_out(root, &["rev-parse", "HEAD"])?.hash(&mut h);
    git_out(root, &["diff", "HEAD", "--binary"])?.hash(&mut h);
    let list = git_out(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut names: Vec<&[u8]> = list.split(|b| *b == 0).filter(|s| !s.is_empty()).collect();
    names.sort();
    for n in names {
        let rel = String::from_utf8_lossy(n).into_owned();
        n.hash(&mut h);
        // A file that cannot be read (or a symlink to nowhere) is undetermined.
        std::fs::read(root.join(&rel)).ok()?.hash(&mut h);
    }
    Some(format!("{:016x}", h.finish()))
}

/// Hash the pieces of a red verdict into one signature string.
pub fn signature(root: &Path, tree: &str, checks: &[(String, Option<i32>, String)]) -> String {
    let mut h = DefaultHasher::new();
    root.hash(&mut h);
    tree.hash(&mut h);
    for c in checks {
        c.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

fn sig_path(state_dir: &Path, session: &str, root: &Path) -> PathBuf {
    let mut h = DefaultHasher::new();
    root.hash(&mut h);
    let safe: String = session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    state_dir
        .join("lastblock")
        .join(format!("{safe}-{:016x}.sig", h.finish()))
}

/// True iff the last recorded block signature for this session+root equals
/// `sig`. Unreadable / absent → false (block).
pub fn is_repeat(state_dir: &Path, session: &str, root: &Path, sig: &str) -> bool {
    match std::fs::read_to_string(sig_path(state_dir, session, root)) {
        Ok(s) => s.trim() == sig,
        // No / unreadable memory: cannot show a repeat -> the caller blocks.
        Err(_) => false,
    }
}

/// Remember `sig` as the last blocked signature. Best effort: a write failure
/// only means the next identical stop blocks again (restrictive side).
pub fn remember(state_dir: &Path, session: &str, root: &Path, sig: &str) {
    let p = sig_path(state_dir, session, root);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(p, sig);
}
