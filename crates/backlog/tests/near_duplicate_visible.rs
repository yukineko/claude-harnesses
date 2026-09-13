//! (C) The near-duplicate scan must reach a HUMAN, through the real binary.
//!
//! backlog f7b018f8: two sessions (in the observed case, one session twice)
//! claimed 91e503c8 and 15eb3f94 — the same piece of work filed twice — because
//! nothing in `backlog` ever compared a new or claimed task against the rest of
//! the queue. The store's only duplicate guard is an EXACT hashkey match, which
//! two differently-phrased titles sail straight past.
//!
//! The ruling on what to do about it was explicit: 「blocking しないまま、
//! ただし必ず可視化」 — never block, never skip, never refuse; always show the
//! peer task id. These tests therefore assert on VISIBILITY and on the exit
//! code staying 0, and they drive the built binary rather than the library so
//! that "it is computed" cannot pass for "a human can see it".
//!
//! The scan is also asserted to speak when it finds NOTHING (a queue of one),
//! because the scorer is lexical Jaccard over whitespace-segmented tokens and
//! is largely inert on this repo's Japanese titles (backlog 7b6bcfe6): an
//! empty peer list rendered as silence would read as "checked, no duplicates",
//! which is exactly the false clean bill of health CLAUDE.md §3 forbids.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-neardup-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // macOS' temp dir is a symlink (/var -> /private/var); canonicalize once so
    // the project label this test passes equals the one the binary resolves.
    std::fs::canonicalize(&dir).unwrap()
}

/// A temp dir that IS a repo root (a `.git` DIRECTORY is enough for both the
/// store's ancestor scan and the main-working-tree identity scan, so no `git`
/// binary is needed).
fn temp_repo(tag: &str) -> PathBuf {
    let dir = unique_dir(tag);
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    dir
}

/// Run the real binary; returns (exit code, stdout, stderr).
fn run(args: &[&str], cwd: &Path, home: &Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .env("HOME", home)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Two ASCII titles that really do score above the 0.6 threshold, so this test
/// exercises the surfacing path rather than the tokenizer's blind spot. (The
/// Japanese pair that motivated the feature scores 0.263 and is pinned as a
/// measurement in `dedup.rs`, not used here.)
const TITLE_A: &str = "add touched files field to the backlog task record";
const TITLE_B: &str = "add touched files field to the backlog task record and group";

#[test]
fn add_surfaces_the_near_duplicate_peer_id_without_blocking() {
    let home = unique_dir("add-home");
    let repo = temp_repo("add-repo");
    let project = repo.to_string_lossy().into_owned();

    let (code, out, err) = run(
        &["add", "--title", TITLE_A, "--project", &project],
        &repo,
        &home,
    );
    assert_eq!(code, 0, "first add must succeed: {err}");
    let first_id = out
        .trim()
        .strip_prefix("added: ")
        .expect("add prints the new id")
        .to_string();

    let (code, out, err) = run(
        &["add", "--title", TITLE_B, "--project", &project],
        &repo,
        &home,
    );
    // NOT blocked: the near-duplicate is advisory. It is a different hashkey,
    // so the store's exact-match guard does not fire either.
    assert_eq!(
        code, 0,
        "a near-duplicate add must NOT be blocked (ruling: do not block): {err}"
    );
    assert!(
        out.contains("added: "),
        "the task must still be filed: {out}"
    );
    // ...and VISIBLE: the peer's id has to appear where a human reads it.
    assert!(
        err.contains(&first_id),
        "the near-duplicate peer id {first_id} must be surfaced, got stderr: {err}"
    );
    assert!(
        err.contains("near-duplicate"),
        "the notice must say what it is, got stderr: {err}"
    );
}

#[test]
fn claim_surfaces_the_near_duplicate_peer_id_in_both_channels() {
    let home = unique_dir("claim-home");
    let repo = temp_repo("claim-repo");
    let project = repo.to_string_lossy().into_owned();

    let (code, out, err) = run(
        &["add", "--title", TITLE_A, "--project", &project],
        &repo,
        &home,
    );
    assert_eq!(code, 0, "first add must succeed: {err}");
    let first_id = out
        .trim()
        .strip_prefix("added: ")
        .expect("add prints the new id")
        .to_string();
    let (code, _, err) = run(
        &["add", "--title", TITLE_B, "--project", &project],
        &repo,
        &home,
    );
    assert_eq!(code, 0, "second add must succeed: {err}");

    // The claim hands out the FIRST task (same priority -> older created_at
    // wins); its near-duplicate peer is the second one.
    let (code, out, err) = run(&["next", "--claim"], &repo, &home);
    assert_eq!(code, 0, "the claim must succeed: {err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("claim prints JSON");
    assert_eq!(v["id"].as_str(), Some(first_id.as_str()), "got: {out}");
    // Machine-readable channel: the JSON the driver parses.
    let peers = v["near_duplicates"]
        .as_array()
        .unwrap_or_else(|| panic!("claim JSON must carry near_duplicates: {out}"));
    assert_eq!(peers.len(), 1, "expected one peer: {out}");
    assert_eq!(peers[0]["title"].as_str(), Some(TITLE_B), "got: {out}");
    // Human-readable channel: stderr, because a driver's JSON is not a human.
    assert!(
        err.contains("near-duplicate") && err.contains(peers[0]["id"].as_str().unwrap()),
        "the peer id must be visible on stderr too, got: {err}"
    );
}

/// The empty case must NOT be silent. A queue of one has no peer, and that is
/// precisely when a reader is most likely to infer "checked, no duplicates" —
/// an inference the lexical scorer does not support on Japanese titles.
#[test]
fn an_empty_peer_set_is_still_reported_with_its_limitation() {
    let home = unique_dir("solo-home");
    let repo = temp_repo("solo-repo");
    let project = repo.to_string_lossy().into_owned();

    let (code, _, err) = run(
        &["add", "--title", TITLE_A, "--project", &project],
        &repo,
        &home,
    );
    assert_eq!(code, 0, "add must succeed: {err}");
    assert!(
        err.contains("near-duplicate"),
        "the scan must speak even with no peer, got stderr: {err}"
    );
    let lower = err.to_lowercase();
    assert!(
        lower.contains("japanese"),
        "the empty result must carry its limitation, got stderr: {err}"
    );
    assert!(
        err.contains("7b6bcfe6"),
        "the notice must point at the filed tokenizer defect, got stderr: {err}"
    );
}
