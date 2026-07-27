// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Parity oracle for the SINGLE-SOURCE-OF-TRUTH choice documented in
//! `src/scan.rs`: this crate keeps its own `regex::Regex` taxonomy (option
//! (ii), not a literally-shared pattern source with
//! `scripts/check-prompt-injection.py`), so this test and
//! `scripts/test_check_prompt_injection.py`'s `ParityWithFetchguardCorpus`
//! BOTH run the SAME fixture corpus
//! (`scripts/tests/fixtures/injection_parity_corpus.json`) against their
//! respective scanner. A category renamed, a phrase added to one side only,
//! or a defense marker recognised by one but not the other trips whichever
//! side's suite runs — divergence cannot silently drift.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    id: String,
    category: Option<String>,
    expect_hit: bool,
    text: String,
}

fn repo_root() -> PathBuf {
    // crates/fetchguard -> crates -> <repo root>
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/fetchguard has a parent")
        .parent()
        .expect("crates/ has a parent")
        .to_path_buf()
}

fn load_corpus() -> Vec<Fixture> {
    let path = repo_root()
        .join("scripts")
        .join("tests")
        .join("fixtures")
        .join("injection_parity_corpus.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read shared parity corpus {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("shared parity corpus is valid JSON")
}

#[test]
fn corpus_is_non_trivial_and_covers_all_four_categories() {
    // Anti-vacuity: the corpus itself must actually exercise something, and
    // must cover every category this crate's scanner recognises, or this
    // whole parity test proves nothing.
    let corpus = load_corpus();
    assert!(
        corpus.len() >= 8,
        "corpus is suspiciously small: {}",
        corpus.len()
    );
    let malicious_categories: std::collections::BTreeSet<&str> = corpus
        .iter()
        .filter(|f| f.expect_hit)
        .filter_map(|f| f.category.as_deref())
        .collect();
    for want in [
        "conceal-ja",
        "conceal-en",
        "verify-bypass",
        "override",
        "egress",
    ] {
        assert!(
            malicious_categories.contains(want),
            "corpus is missing a malicious fixture for category {want:?}: {malicious_categories:?}"
        );
    }
    assert!(
        corpus.iter().any(|f| !f.expect_hit),
        "corpus must include at least one benign control"
    );
}

#[test]
fn fetchguard_scanner_matches_corpus_expectations() {
    let corpus = load_corpus();
    for fixture in &corpus {
        let hits = fetchguard::scan::scan(&fixture.text);
        let got_hit = !hits.is_empty();
        assert_eq!(
            got_hit, fixture.expect_hit,
            "fixture {:?}: expected hit={}, got hits={hits:?}",
            fixture.id, fixture.expect_hit
        );
        if fixture.expect_hit {
            if let Some(want_category) = &fixture.category {
                assert!(
                    hits.iter().any(|h| &h.category == want_category),
                    "fixture {:?}: expected category {:?} among hits {hits:?}",
                    fixture.id,
                    want_category
                );
            }
        }
    }
}
