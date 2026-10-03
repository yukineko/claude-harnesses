//! `backlog audit-closures [--json]`: read-only classification of done rows.
//! Assumed JSON shape: somewhere in the document there is an object per row
//! carrying `"id"` and a string `"class"` (observed-f2p, doc-only, duplicate,
//! ruling-approved, cited-only, judgment, none).
mod common;
use common::*;

fn class_of(v: &serde_json::Value, id: &str) -> Option<String> {
    match v {
        serde_json::Value::Object(m) => {
            if m.get("id").and_then(|x| x.as_str()) == Some(id) {
                if let Some(c) = m.get("class").and_then(|x| x.as_str()) {
                    return Some(c.to_string());
                }
            }
            m.values().find_map(|x| class_of(x, id))
        }
        serde_json::Value::Array(a) => a.iter().find_map(|x| class_of(x, id)),
        _ => None,
    }
}

struct Scenario {
    f: Fixture,
    f2p: String,
    doc: String,
    dup: String,
    cited: String,
    none: String,
}

fn scenario() -> Scenario {
    let f = Fixture::new("audit");
    let (red, _) = f.bug_then_fix();
    let f2p = f.add("f2p closed");
    assert_eq!(
        f.run(&[
            "done",
            &f2p,
            "--test",
            "bash tests/check.sh",
            "--red-rev",
            &red
        ])
        .code,
        0
    );
    f.write("docs/x.md", "doc\n");
    let c = f.commit_paths(&["docs/x.md"], "docs");
    let doc = f.add("doc closed");
    assert_eq!(f.run(&["done", &doc, "--doc-only", &c]).code, 0);
    let target = f.add("canonical");
    let dup = f.add("dup closed");
    assert_eq!(f.run(&["done", &dup, "--duplicate-of", &target]).code, 0);
    // Legacy rows (no closure table) written straight to the done file.
    let cited = "c1c1c1c1".to_string();
    f.plant_legacy_done(&cited, "legacy cited", "done");
    let none = "d2d2d2d2".to_string();
    f.plant_legacy_done(&none, "legacy bare", "done");
    // The cited row cites a commit and a file:line in its notes (never evidence).
    let s = std::fs::read_to_string(f.done_path()).unwrap();
    let idx = s.find(&format!("id = \"{cited}\"")).unwrap();
    let (head, tail) = s.split_at(idx);
    let tail = tail.replacen(
        "notes = \"\"",
        "notes = \"fixed in 3f2a1bc, see crates/x/src/lib.rs:42\"",
        1,
    );
    std::fs::write(f.done_path(), format!("{head}{tail}")).unwrap();
    Scenario {
        f,
        f2p,
        doc,
        dup,
        cited,
        none,
    }
}

#[test]
fn classifies_each_done_row() {
    let s = scenario();
    let o = s.f.run(&["audit-closures", "--json"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let v: serde_json::Value = serde_json::from_str(o.stdout.trim())
        .unwrap_or_else(|e| panic!("--json must be valid JSON ({e}): {:?}", o.stdout));
    for (id, want) in [
        (&s.f2p, "observed-f2p"),
        (&s.doc, "doc-only"),
        (&s.dup, "duplicate"),
        (&s.cited, "cited-only"),
        (&s.none, "none"),
    ] {
        assert_eq!(class_of(&v, id).as_deref(), Some(want), "row {id}: {v}");
    }
}

#[test]
fn human_output_lists_every_class_name_in_use() {
    let s = scenario();
    let o = s.f.run(&["audit-closures"]);
    assert_eq!(o.code, 0, "{}", o.both());
    for c in [
        "observed-f2p",
        "doc-only",
        "duplicate",
        "cited-only",
        "none",
    ] {
        assert!(o.stdout.contains(c), "class {c} missing: {}", o.stdout);
    }
}

#[test]
fn audit_is_read_only_store_byte_identical_and_never_reopens() {
    let s = scenario();
    let before = s.f.store_bytes();
    for args in [vec!["audit-closures"], vec!["audit-closures", "--json"]] {
        let o = s.f.run(&args);
        assert_eq!(o.code, 0, "{}", o.both());
    }
    assert_eq!(
        s.f.store_bytes(),
        before,
        "audit-closures must not write the store"
    );
    assert_eq!(
        s.f.status(&s.none),
        "done",
        "audit never reopens a legacy row"
    );
    assert_eq!(s.f.status(&s.cited), "done");
}

#[test]
fn unparseable_store_is_non_zero_not_an_empty_audit() {
    let f = Fixture::new("auditbad");
    f.add("t");
    std::fs::write(f.done_path(), "[[task]\nnot toml at all").unwrap();
    for args in [vec!["audit-closures"], vec!["audit-closures", "--json"]] {
        let o = f.run(&args);
        assert_ne!(
            o.code,
            0,
            "load failure must be non-zero, got: {}",
            o.both()
        );
    }
}

#[test]
fn empty_done_set_yields_valid_json_and_success() {
    let f = Fixture::new("auditempty");
    f.add("t");
    let o = f.run(&["audit-closures", "--json"]);
    assert_eq!(o.code, 0, "{}", o.both());
    let v: serde_json::Value =
        serde_json::from_str(o.stdout.trim()).expect("valid JSON even when empty");
    assert!(class_of(&v, "zzzzzzzz").is_none());
}
