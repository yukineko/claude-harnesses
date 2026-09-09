// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! CLAUDE.md §4 — the THIRD mirror of the "no accompanying test" claim tdd
//! never checked: the user-facing plugin description.
//!
//! `crates/tdd/src/gate.rs` scans the UNCOMMITTED change set only — the
//! porcelain status, the unstaged and staged diffs, and the untracked files,
//! all relative to HEAD. It never reads the commit log, so a test that landed
//! in an earlier commit is invisible to it. The gate may report "no test was
//! visible in the uncommitted changes"; it may not assert that the change has
//! no test.
//!
//! `b61d68b4` corrected that wording in TWO of the three places the description
//! is mirrored — `crates/tdd/Cargo.toml` and
//! `crates/tdd/.claude-plugin/plugin.json`, which now both say the gate blocks
//! when "no test is visible in the uncommitted changes ... never
//! already-committed tests". The third mirror, the entry for plugin name `tdd`
//! in the repo-root `.claude-plugin/marketplace.json`, was missed and still
//! carries the original claim verbatim. This is the recurring mirror-gap class:
//! a fix lands on one copy and the audit reads as converged.
//!
//! No existing gate covers this text. `scripts/check-doc-claims.py` scopes
//! itself to `docs/**/*.md` by its own docstring, and the version gates
//! (`check-plugin-versions.py` / `check-version-bumped.py`) compare only
//! version fields, never description prose.
//!
//! Written by a DISINTERESTED author (CLAUDE.md §2(a)).
//!
//! ## Fail-closed reads (CLAUDE.md §3)
//!
//! Every way this test could fail to REACH the description — the repo root not
//! resolving, the file not reading, the JSON not parsing, no entry named `tdd`,
//! no `description` key — is a cannot-determine, and every one of them panics.
//! None of them is allowed to look like "the claim is absent, so we pass".

use std::path::{Path, PathBuf};

/// The repo root, from this crate's manifest dir (`<root>/crates/tdd`).
/// A root that does not carry the file we are about to read is not a root:
/// resolving it wrongly must fail here, not silently pass downstream.
fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| {
            panic!(
                "cannot resolve the repo root two levels above CARGO_MANIFEST_DIR ({}); \
                 refusing to guess",
                manifest.display()
            )
        })
        .to_path_buf();
    assert!(
        root.join(".claude-plugin/marketplace.json").is_file(),
        "resolved repo root {} has no .claude-plugin/marketplace.json — the read is \
         undetermined, which is NOT the same fact as 'the dishonest claim is absent' \
         (CLAUDE.md §3)",
        root.display()
    );
    root
}

/// The `description` of the marketplace entry whose `name` is exactly `tdd`.
/// Panics on every undetermined path (see the module docstring).
fn tdd_marketplace_description() -> String {
    let path = repo_root().join(".claude-plugin/marketplace.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e} (undetermined, not clean)",
            path.display()
        )
    });
    let doc: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|e| {
        panic!(
            "cannot parse {}: {e} (undetermined, not clean)",
            path.display()
        )
    });

    // The plugin list is either the top-level array or the `plugins` key;
    // accept both shapes, but refuse to invent an empty one.
    let plugins = doc
        .get("plugins")
        .and_then(|p| p.as_array())
        .or_else(|| doc.as_array())
        .unwrap_or_else(|| {
            panic!(
                "{} has no plugin array (neither a top-level array nor a `plugins` key) \
                 — undetermined, not clean",
                path.display()
            )
        });
    assert!(
        !plugins.is_empty(),
        "{} lists zero plugins; an empty set is not evidence that the claim is gone \
         (CLAUDE.md §3 — never return an empty set on a failed read)",
        path.display()
    );

    let entry = plugins
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("tdd"))
        .unwrap_or_else(|| {
            panic!(
                "{} has no plugin entry named \"tdd\" — a MISSING entry must fail this \
                 test, never pass it",
                path.display()
            )
        });

    entry
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or_else(|| {
            panic!(
                "the \"tdd\" entry in {} has no string \"description\" — undetermined, \
                 not clean",
                path.display()
            )
        })
        .to_string()
}

/// (a) The user-facing description must not assert an absence tdd never
/// checked. Both phrasings below appear verbatim in the entry today.
#[test]
fn tdd_marketplace_description_does_not_assert_an_unchecked_absence_of_a_test() {
    let desc = tdd_marketplace_description();
    for claim in ["without an accompanying test", "missing test coverage"] {
        assert!(
            !desc.contains(claim),
            "the marketplace description for plugin \"tdd\" states the absence of a test \
             as an unqualified fact ({claim:?}). The gate reads the UNCOMMITTED change \
             set only and never consults already-committed tests, so it cannot know \
             this. Cargo.toml and .claude-plugin/plugin.json were both corrected in \
             b61d68b4; this mirror was missed.\n--- description ---\n{desc}"
        );
    }
}

/// (b) ANTI-VACUITY: the honest wording must still be a description. Emptying
/// the field, or deleting the sentence that names what the plugin is, is not a
/// fix for the claim above.
#[test]
fn tdd_marketplace_description_still_describes_the_plugin() {
    let desc = tdd_marketplace_description();
    assert!(
        !desc.trim().is_empty(),
        "the marketplace description for plugin \"tdd\" must not be empty — deleting \
         the prose is not a way to satisfy the honesty assertion"
    );
    let lower = desc.to_lowercase();
    assert!(
        lower.contains("tdd") || lower.contains("test-first"),
        "the marketplace description must still say what the plugin is (mention \"tdd\" \
         or \"test-first\")\n--- description ---\n{desc}"
    );
}
