//! Per-session attempt counter (the shared gate state in
//! `harness_core::gate::state`; the `last_hash` field of the shared
//! `SessionState` stays empty for this gate) plus donegate's own
//! identical-finding memory (below).

pub use harness_core::gate::state::bump;

/// End the chain of consecutive blocks: clear the attempt counter AND delete
/// the chain id, which makes every remembered signature of the session
/// unmatchable (see [`end_chain`]). Every
/// path that resets the counter (green, give-up, skip, refusal give-up) goes
/// through here, so a signature cannot match outside the chain it was blocked in.
pub fn reset(state_dir: &Path, session: &str) {
    harness_core::gate::state::reset(state_dir, session);
    end_chain(state_dir, session);
}

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
//
// The memory is scoped to one CHAIN of consecutive blocks (verifier F1): every
// remembered signature carries the id of the chain it was recorded in, and
// `is_repeat` only matches when that id equals the session's current chain id
// AND the attempt counter's last real block is within `reset_after_secs`. The
// chain id is minted when a block starts a new chain (attempt 1) and deleted
// whenever the chain ends (`reset`), leaving older signatures inert. So a
// revert to an earlier blocked tree after an intervening green is a first block
// again. The caller consults this memory only in hook mode with a real session
// id (verifier F3a/F3b), and a waived stop does not bump the counter (F2).
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
/// as "the tree changed" and block. The diff is taken with `--no-ext-diff
/// --no-textconv` so a user diff driver cannot hide content from it.
/// Not covered: dirt inside a submodule's own working tree beyond what
/// `git diff` reports for the submodule entry.
pub fn tree_fingerprint(root: &Path) -> Option<String> {
    let mut h = DefaultHasher::new();
    git_out(root, &["rev-parse", "HEAD"])?.hash(&mut h);
    git_out(
        root,
        &["diff", "HEAD", "--binary", "--no-ext-diff", "--no-textconv"],
    )?
    .hash(&mut h);
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

fn safe(session: &str) -> String {
    session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn lastblock_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("lastblock")
}

fn root_hash(root: &Path) -> String {
    let mut h = DefaultHasher::new();
    root.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn sig_path(state_dir: &Path, session: &str, root: &Path) -> PathBuf {
    lastblock_dir(state_dir).join(format!("{}-{}.sig", safe(session), root_hash(root)))
}

fn chain_path(state_dir: &Path, session: &str) -> PathBuf {
    lastblock_dir(state_dir).join(format!("{}.chain", safe(session)))
}

/// Write `body` to `p` via a temp file + rename, so a reader never sees a
/// half-written file. On any failure the destination is removed, so a failed
/// write does not leave an OLDER value behind (unless the removal fails too).
/// Returns whether `p` now holds `body`.
fn write_or_remove(p: &Path, body: &str) -> bool {
    let ok = (|| -> std::io::Result<()> {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = p.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, p).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    })()
    .is_ok();
    if !ok {
        let _ = std::fs::remove_file(p);
    }
    ok
}

/// The session's current chain id, if one was recorded and is readable.
fn current_chain(state_dir: &Path, session: &str) -> Option<String> {
    let s = std::fs::read_to_string(chain_path(state_dir, session)).ok()?;
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Called after a real block recorded `attempt` for this session. Attempt 1 is
/// the start of a new chain (first block, or the counter's idle reset): a
/// fresh chain id is minted, which invalidates every signature recorded in an
/// earlier chain. A later attempt keeps the current id, or mints one if it is
/// missing (an older signature then cannot match: the restrictive side).
/// Returns the chain id to store with the signature, or `None` if it could not
/// be persisted (the caller then remembers no signature).
pub fn chain_for_block(state_dir: &Path, session: &str, attempt: u32) -> Option<String> {
    if attempt > 1 {
        if let Some(c) = current_chain(state_dir, session) {
            return Some(c);
        }
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let id = format!("{nanos:x}-{:x}", std::process::id());
    write_or_remove(&chain_path(state_dir, session), &id).then_some(id)
}

/// True iff the session's chain is still live (counter non-zero and its last
/// real block within `reset_after_secs`, chain id readable) and the signature
/// recorded for this session+root in THIS chain equals `sig`. Anything absent /
/// unreadable / unparsable / stale -> false (the caller blocks).
pub fn is_repeat(
    state_dir: &Path,
    session: &str,
    root: &Path,
    sig: &str,
    reset_after_secs: i64,
) -> bool {
    let st = harness_core::gate::state::load(state_dir, session);
    let now = chrono::Local::now().timestamp();
    if st.attempts == 0 || now - st.last_ts > reset_after_secs {
        return false;
    }
    let Some(chain) = current_chain(state_dir, session) else {
        return false;
    };
    let Ok(s) = std::fs::read_to_string(sig_path(state_dir, session, root)) else {
        return false;
    };
    let mut parts = s.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some(recorded), Some(rchain), None) => recorded == sig && rchain == chain,
        _ => false,
    }
}

/// Record the outcome of a real block on `root`: `sig` stamped with `chain`
/// becomes this root's last blocked signature. With either one `None` (the
/// signature or the chain id could not be determined) the previous signature is
/// removed, so it cannot be mistaken for this block's. Any write failure also
/// ends with the file removed (`write_or_remove`); only if that removal fails
/// too does the older signature remain, and it then still carries the chain id
/// it was recorded under, so it can only match within that same chain.
pub fn remember(
    state_dir: &Path,
    session: &str,
    root: &Path,
    sig: Option<&str>,
    chain: Option<&str>,
) {
    let p = sig_path(state_dir, session, root);
    match (sig, chain) {
        (Some(sig), Some(chain)) => {
            write_or_remove(&p, &format!("{sig} {chain}\n"));
        }
        _ => {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Forget this root's last blocked signature (the root went green).
pub fn forget(state_dir: &Path, session: &str, root: &Path) {
    let _ = std::fs::remove_file(sig_path(state_dir, session, root));
}

/// The chain of consecutive blocks ended: forget the session's chain id. That
/// alone makes every signature recorded in the chain unmatchable — `is_repeat`
/// requires the signature's chain id to equal the current one, and the next
/// chain mints a fresh id — so the per-root signature files are not walked
/// (a stale file left behind is inert). If the removal itself fails, the
/// attempt counter's reset (done alongside, in [`reset`]) still makes
/// `is_repeat` refuse while the counter is zero.
pub fn end_chain(state_dir: &Path, session: &str) {
    let _ = std::fs::remove_file(chain_path(state_dir, session));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dg-state-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn end_chain_alone_makes_the_signature_unmatchable() {
        let d = dir("endchain");
        let root = Path::new("/r");
        assert_eq!(bump(&d, "s", 600), 1);
        let c = chain_for_block(&d, "s", 1).unwrap();
        remember(&d, "s", root, Some("aa"), Some(&c));
        assert!(is_repeat(&d, "s", root, "aa", 600));
        // Counter left live on purpose: only the chain id is gone.
        end_chain(&d, "s");
        assert!(!is_repeat(&d, "s", root, "aa", 600));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn repeat_requires_same_chain() {
        let d = dir("chain");
        let root = Path::new("/r");
        assert_eq!(bump(&d, "s", 600), 1);
        let c1 = chain_for_block(&d, "s", 1).unwrap();
        remember(&d, "s", root, Some("aa"), Some(&c1));
        assert!(is_repeat(&d, "s", root, "aa", 600));
        assert!(!is_repeat(&d, "s", root, "bb", 600));
        // A new chain (attempt 1 again) invalidates the old signature.
        let c2 = chain_for_block(&d, "s", 1).unwrap();
        assert_ne!(c1, c2);
        assert!(!is_repeat(&d, "s", root, "aa", 600));
        // A reset counter is no live chain.
        remember(&d, "s", root, Some("aa"), Some(&c2));
        reset(&d, "s");
        assert!(!is_repeat(&d, "s", root, "aa", 600));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unknown_signature_removes_the_older_one() {
        let d = dir("unknown");
        let root = Path::new("/r");
        assert_eq!(bump(&d, "s", 600), 1);
        let c = chain_for_block(&d, "s", 1).unwrap();
        remember(&d, "s", root, Some("aa"), Some(&c));
        remember(&d, "s", root, None, Some(&c));
        assert!(!is_repeat(&d, "s", root, "aa", 600));
        let _ = std::fs::remove_dir_all(&d);
    }
}
