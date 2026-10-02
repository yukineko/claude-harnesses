#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog 196b811b: store.rs's review_findings docstrings contradict each
//! other inside the same file. One doc calls condukt's gate-exec escalate path
//! "the first real producer of this stream" while others still claim "today
//! there is no producer" / "with no producer wired yet". Producers exist
//! (condukt gate_exec, specguard, propguard), so the "no producer" prose is the
//! stale half (CLAUDE.md §4: prose that disagrees with the code deceives the
//! next reviewer).

const STORE_RS: &str = include_str!("../src/store.rs");

#[test]
#[ignore = "backlog 196b811b: open defect, remove ignore when fixed"]
fn store_rs_review_findings_docs_do_not_claim_there_is_no_producer() {
    assert!(
        STORE_RS.contains("first real producer"),
        "precondition: store.rs documents a real producer of review_findings.jsonl"
    );
    let stale: Vec<(usize, &str)> = STORE_RS
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            l.contains("today there is no producer")
                || l.contains("no producer wired")
                || l.contains("while no producer is")
        })
        .map(|(i, l)| (i + 1, l.trim()))
        .collect();
    assert!(
        stale.is_empty(),
        "store.rs still says review_findings has no producer, contradicting its own \
         'first real producer' doc: {stale:?}"
    );
}
