#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fourth adversarial verification (94119fa05): direct probes of
//! `harness_core::shrink` against its module docstring ("keep every key,
//! nesting level and value type").

use harness_core::shrink::{shrink_str, shrink_value};
use serde_json::{json, Value};

fn ser_len(v: &Value) -> usize {
    v.to_string().chars().count()
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Recursively: same keys, same value kinds, and every array element has the
/// kind of the input array's elements (homogeneous arrays stay homogeneous).
fn same_shape(a: &Value, b: &Value, path: &str) -> Result<(), String> {
    if kind(a) != kind(b) {
        return Err(format!("{path}: {} -> {}", kind(a), kind(b)));
    }
    match (a, b) {
        (Value::Object(ma), Value::Object(mb)) => {
            let ka: Vec<_> = ma.keys().collect();
            let kb: Vec<_> = mb.keys().collect();
            if ka != kb {
                return Err(format!("{path}: keys {ka:?} -> {kb:?}"));
            }
            for (k, va) in ma {
                same_shape(va, &mb[k], &format!("{path}.{k}"))?;
            }
            Ok(())
        }
        (Value::Array(xa), Value::Array(xb)) => {
            if let Some(first) = xa.first() {
                for (i, e) in xb.iter().enumerate() {
                    if kind(e) != kind(first) {
                        return Err(format!(
                            "{path}[{i}]: element kind {} in an array of {} (value {})",
                            kind(e),
                            kind(first),
                            e
                        ));
                    }
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// 3000 content-block objects: the item-drop path inserts a bare STRING into
/// an array of objects, so the element type changes ("value type" kept is the
/// docstring claim; an MCP `content` array of blocks would not validate).
#[test]
fn object_array_elision_keeps_element_kind() {
    let items: Vec<Value> = (0..3000)
        .map(|i| json!({"type": "text", "text": format!("match line {i:05} in some file")}))
        .collect();
    let v = json!({ "content": items });
    let r = shrink_value(&v, 8192);
    same_shape(&v, &r.value, "$").unwrap();
}

/// Same for an array of numbers.
#[test]
fn number_array_elision_keeps_element_kind() {
    let v = json!({ "ids": (0..20_000).collect::<Vec<u32>>() });
    let r = shrink_value(&v, 8192);
    same_shape(&v, &r.value, "$").unwrap();
}

/// 500 numeric-only objects (~110 chars each, 55k total) under a 50k budget:
/// fair share (~98) is >= MIN_ITEM_BUDGET so items are "trimmed" instead of
/// dropped, but numbers cannot shrink, so the result stays over budget even
/// though dropping ~10% of items would fit.
#[test]
fn unshrinkable_items_fit_by_dropping() {
    let items: Vec<Value> = (0..500)
        .map(|i| {
            json!({"a": 1_000_000 + i, "b": 2_000_000 + i, "c": 3_000_000 + i,
                   "d": 4_000_000 + i, "e": 5_000_000 + i, "f": 6_000_000 + i,
                   "g": 7_000_000 + i, "h": 8_000_000 + i, "i": 9_000_000 + i,
                   "j": true, "k": false})
        })
        .collect();
    let v = Value::Array(items);
    let before = ser_len(&v);
    let r = shrink_value(&v, 50_000);
    let after = ser_len(&r.value);
    assert!(
        r.fits && after <= 50_000,
        "before={before} after={after} fits={}",
        r.fits
    );
}

/// Never larger than the input, and fits, on a mix of escape-heavy and plain
/// fields (sanity of the binary search with a non-uniform string).
#[test]
fn mixed_escape_string_fits_and_keeps_marker() {
    let s = "a".repeat(30_000) + &"\u{1}".repeat(30_000);
    let v = json!({"stdout": s, "stderr": "", "interrupted": false, "isImage": false});
    let r = shrink_value(&v, 8192);
    let after = ser_len(&r.value);
    assert!(r.fits && after <= 8192, "after={after}");
    same_shape(&v, &r.value, "$").unwrap();
    let out = r.value["stdout"].as_str().unwrap();
    assert!(
        out.contains("elided"),
        "no marker: {}",
        &out[..80.min(out.len())]
    );
    assert!(out.starts_with('a') && out.ends_with('\u{1}'));
}

/// Bash object with BOTH stdout and stderr huge: keys/types kept, within
/// budget, each keeps a marker.
#[test]
fn bash_both_streams_huge() {
    let v = json!({
        "stdout": "out line\n".repeat(20_000),
        "stderr": "err line\n".repeat(20_000),
        "interrupted": false,
        "isImage": false,
    });
    let r = shrink_value(&v, 8192);
    same_shape(&v, &r.value, "$").unwrap();
    assert!(r.fits && ser_len(&r.value) <= 8192, "{}", ser_len(&r.value));
    assert!(r.value["stdout"].as_str().unwrap().contains("elided"));
    assert!(r.value["stderr"].as_str().unwrap().contains("elided"));
}

/// Multibyte / astral chars: no panic, output is valid, within budget.
#[test]
fn multibyte_and_astral_strings() {
    let s = "日本語🦀\u{301}".repeat(30_000);
    let v = json!({"stdout": s, "stderr": "", "interrupted": false, "isImage": false});
    let r = shrink_value(&v, 4096);
    assert!(r.fits, "{}", ser_len(&r.value));
    same_shape(&v, &r.value, "$").unwrap();
    let t = shrink_str(&"🦀".repeat(10_000), 100);
    assert!(t.chars().count() <= 100);
}

/// Tiny budgets must not panic and never grow.
#[test]
fn tiny_budgets_never_panic_or_grow() {
    let v = json!({"stdout": "x".repeat(1000), "stderr": "y", "interrupted": false, "isImage": false,
                   "list": (0..50).map(|i| format!("f{i}")).collect::<Vec<_>>()});
    let before = ser_len(&v);
    for b in 0..200 {
        let r = shrink_value(&v, b);
        assert!(ser_len(&r.value) <= before, "budget {b} grew");
        assert!(r.value["interrupted"] == json!(false));
    }
}

/// Bash `isImage: true`: stdout holds the image data. Trimming it with a text
/// marker corrupts the image while keeping the shape — the value must be kept
/// verbatim (or not rewritten at all), not mangled.
#[test]
fn image_payload_not_mangled() {
    let b64 = "iVBORw0KGgoAAAANSUhEUgAA".repeat(4_000);
    let v = json!({"stdout": format!("data:image/png;base64,{b64}"), "stderr": "",
                   "interrupted": false, "isImage": true});
    let r = shrink_value(&v, 8192);
    let out = r.value["stdout"].as_str().unwrap();
    assert!(
        r.value == v || !out.contains("elided"),
        "base64 image mangled: ...{}...",
        &out[out.find('\n').unwrap_or(0)..][..60]
    );
}
