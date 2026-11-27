#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Third adversarial verification (d0e467ee5): direct probes of
//! `harness_core::shrink` against its module docstring.

use harness_core::shrink::{shrink_str, shrink_value};
use serde_json::{json, Value};

fn ser_len(v: &Value) -> usize {
    v.to_string().chars().count()
}

/// Docstring: "a field never grows". Probe: a list of 9 empty strings (28
/// chars) under a tight budget must not be replaced by a longer marker array.
/// (Passes at d0e467ee5; kept as a regression check.)
#[test]
fn field_never_grows_with_nine_empty_strings() {
    let v = json!({"a": "x".repeat(1000), "l": ["", "", "", "", "", "", "", "", ""]});
    let before = ser_len(&v["l"]);
    let r = shrink_value(&v, 40);
    let after = ser_len(&r.value["l"]);
    assert!(
        after <= before,
        "field `l` grew {before} -> {after}: {}",
        r.value["l"]
    );
}

/// A string whose JSON escaping at least doubles its length (control chars,
/// `\r`, `\n`) collapses to "" in `shrink_string_value`: no head, no tail, no
/// marker. Docstring promises "head and tail, with a truthful elision marker".
#[test]
fn escape_heavy_string_is_not_emptied() {
    let s = "\u{1b}[K\r".repeat(20_000);
    let v = json!({ "stdout": s });
    let r = shrink_value(&v, 2_000);
    let out = r.value["stdout"].as_str().unwrap();
    assert!(!out.is_empty(), "stdout emptied (fits={})", r.fits);
    assert!(out.contains("elided"), "no elision marker: {out:?}");
}

/// Nine huge strings (each larger than the whole budget): the item-elision
/// path keeps zero items and the array becomes only the marker; no content
/// survives although a per-string trim would fit.
#[test]
fn nine_huge_strings_keep_some_content() {
    let items: Vec<String> = (0..9)
        .map(|i| format!("item{i} ") + &"y".repeat(20_000))
        .collect();
    let v = json!(items);
    let r = shrink_value(&v, 5_000);
    assert!(r.fits);
    let arr = r.value.as_array().unwrap();
    assert!(
        arr.iter().any(|x| x.as_str().unwrap().starts_with("item")),
        "no item content kept: {}",
        r.value
    );
}

/// Control: eight huge strings (< 9, fair split path) fit and each keeps head.
#[test]
fn control_eight_huge_strings_fit_and_keep_heads() {
    let items: Vec<String> = (0..8)
        .map(|i| format!("item{i} ") + &"y".repeat(20_000))
        .collect();
    let r = shrink_value(&json!(items), 5_000);
    assert!(r.fits, "{}", ser_len(&r.value));
    let arr = r.value.as_array().unwrap();
    assert_eq!(arr.len(), 8);
    for (i, x) in arr.iter().enumerate() {
        assert!(x.as_str().unwrap().starts_with(&format!("item{i}")));
    }
}

/// Control: numbers, booleans and nulls are never changed; keys kept.
#[test]
fn control_scalars_untouched() {
    let v = json!({"s": "z".repeat(50_000), "n": 12345678901_i64, "f": 1.5, "b": true, "z": null,
                   "nested": {"k": [1, 2, 3, false, null]}});
    let r = shrink_value(&v, 1_000);
    for k in ["n", "f", "b", "z", "nested"] {
        assert_eq!(r.value[k], v[k], "{k} changed");
    }
}

/// Control: the marker cannot exceed a tiny string budget (plain-prefix path).
#[test]
fn control_tiny_budget_string_within_budget() {
    for b in 0..40 {
        let t = shrink_str(&"q".repeat(1000), b);
        assert!(t.chars().count() <= b, "budget {b}: {}", t.chars().count());
    }
}
