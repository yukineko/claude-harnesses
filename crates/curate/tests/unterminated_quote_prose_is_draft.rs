//! GH 1518 extension (2026-11-04): the no-backtick prose path of curate's
//! `derive::test_runner_command` never tokenizes, so a done_criteria whose
//! written runner command has an unterminated quote still becomes the broader
//! `cargo test -p demo` golden. User ruling 2026-11-03: unterminated quote means
//! NO mechanical command; `promote` must emit a draft (no `cmd`).
//! CONTROL: plain prose still yields a runnable golden, so "always draft" fails.

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
fn unterminated_quote_prose_yields_a_draft_not_a_runnable_golden() {
    let cases = [
        ("dq", "cargo test -p demo -- \"x exits 0"),
        ("sq", "cargo test -p demo -- 'x exits 0"),
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

/// CONTROL: must stay green.
#[test]
fn plain_prose_still_yields_a_runnable_golden() {
    let v = promote("plain", "cargo test -p demo passes");
    assert!(
        v.get("cmd").is_some_and(|c| !c.is_null()),
        "plain prose produced no cmd: {v}"
    );
}
