//! RED repro (2026-11-04): curate's `derive::mechanical_cmd` is the
//! split_whitespace twin of condukt's verify::mechanical_cmd, which GH 1518 made
//! fail-closed (user ruling 2026-11-03: an unterminated quote yields NO
//! mechanical command). curate still turns an unparseable backtick span into a
//! runnable golden: either the span verbatim with the stray `"` glued to a token,
//! or — for a non-runner head like `FOO=1` — the prose-derived `cargo test -p x`,
//! a different command from the one written. evalkit then runs that golden and
//! asserts exit 0.
//!
//! Property pinned: a done_criteria whose backtick span cannot be tokenized is
//! not mechanical, so `promote` emits a draft (no `cmd`), per curate's own
//! "auto + draft fallback" policy.

use std::process::Command;

fn promote(tag: &str, criteria: &str) -> serde_json::Value {
    let root = std::env::temp_dir().join(format!("curate-unterm-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp root");
    let store = root.join("playbooks.jsonl");
    let seed =
        serde_json::json!({"ts": 100, "title": format!("unterm {tag}"), "done_criteria": criteria});
    std::fs::write(&store, format!("{seed}\n")).expect("seed store");
    let out = Command::new(env!("CARGO_BIN_EXE_curate"))
        .args([
            "promote",
            &format!("unterm {tag}"),
            "--dataset",
            "e2e",
            "--store",
            store.to_str().expect("utf8"),
            "--root",
            root.to_str().expect("utf8"),
        ])
        .output()
        .expect("binary runs");
    assert!(
        out.status.success(),
        "promote failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(root.join("evals/curated/e2e.jsonl")).expect("dataset");
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .expect("one golden");
    let v: serde_json::Value = serde_json::from_str(line).expect("golden JSON");
    let _ = std::fs::remove_dir_all(&root);
    v
}

#[test]
fn unterminated_quote_span_yields_a_draft_not_a_runnable_golden() {
    let cases = [
        (
            "runnerhead",
            "`cargo test -p demo -- \"never_closed` passes",
        ),
        (
            "envassign",
            "`FOO=1 cargo test -p demo -- \"never_closed` passes",
        ),
    ];
    let mut failures = Vec::new();
    for (tag, criteria) in cases {
        let v = promote(tag, criteria);
        if v.get("cmd").is_some_and(|c| !c.is_null()) || v.get("draft").is_none() {
            failures.push(format!(
                "{tag}: {criteria:?} produced a runnable golden: {v}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
