//! Repro for backlog 7b6bcfe6: `text_similarity` does not segment Japanese, so a
//! whole phrase becomes ONE token and two near-identical Japanese titles score
//! far below the 0.6 duplicate threshold used by overwatch `possible_duplicate`
//! (overwatch lease.rs POSSIBLE_DUPLICATE_THRESHOLD).

use harness_core::lessons::text_similarity;

/// Pure-Japanese pair, identical except the last character. A tokenizer that
/// works on Japanese must score this far above 0.6. Observed: 0.
#[test]
#[ignore = "backlog 7b6bcfe6: open defect, remove ignore when fixed"]
fn pure_japanese_single_char_difference_is_similar() {
    let s = text_similarity(
        "他セッションの赤が全セッションを止める",
        "他セッションの赤が全セッションを止めた",
    );
    assert!(s >= 0.6, "pure-Japanese 1-char-diff pair scored {s}");
}

/// A sentence compared with a lightly reworded copy (one phrase inserted).
#[test]
#[ignore = "backlog 7b6bcfe6: open defect, remove ignore when fixed"]
fn pure_japanese_inserted_word_is_similar() {
    let s = text_similarity(
        "共有の作業ツリーを複数セッションが同時に編集する",
        "共有の作業ツリーを複数のセッションが同時に編集する",
    );
    assert!(s >= 0.6, "pure-Japanese reworded pair scored {s}");
}
