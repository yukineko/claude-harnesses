//! Shape-preserving shrink of a JSON value to a character budget.
//!
//! Used by PostToolUse hooks that rewrite `updatedToolOutput`: Claude Code
//! rejects a replacement whose shape differs from the tool's output, so the
//! rewrite must keep every key, nesting level and value type. The whole value is
//! walked recursively (objects and arrays) with ONE shared budget:
//!
//! - budget is measured on the compact serialization (`Value::to_string`);
//! - a container splits its budget max-min fairly over its children: children
//!   already smaller than the fair level are kept whole (a field never grows),
//!   only the larger ones are shrunk to the level;
//! - a string is shrunk by characters, keeping head and tail, with a truthful
//!   elision marker (`elided E chars of N`); under a tiny budget it degrades to
//!   a plain head prefix;
//! - a string's budget is measured on its JSON-ESCAPED length (control chars
//!   and `\r` expand 2-6x), found by search, so an escape-heavy string still
//!   keeps head, tail and the marker rather than collapsing to `""`;
//! - an array of 9+ items (any element type) first trims every item to a fair
//!   share (at least 64 chars); if the trimmed items still do not fit (the
//!   share was below the minimum, or the items cannot shrink, e.g. numeric
//!   objects), it keeps a head and a tail of items and drops the middle. An
//!   array whose items are ALL strings gets ONE truthful marker string
//!   (`elided K of N items`) in the gap, so it stays an array of strings. Any
//!   other array gets NO inserted element (that would change an element's value
//!   type): the drop is only counted in [`Shrunk::elided_items`] and the caller
//!   MUST report it (e.g. in `additionalContext`);
//! - image payloads (see [`is_image_payload`]) are never touched: a text marker
//!   spliced into base64 corrupts the image;
//! - numbers, booleans and nulls are never changed.
//!
//! The result reports `fits`: whether the rewrite is within the budget. A
//! `false` means "shrunk as far as the shape allows but still over" — callers
//! MUST say so rather than silently pass the output through.

use serde_json::Value;

/// A shrink result.
pub struct Shrunk {
    pub value: Value,
    /// `true` iff `value`'s compact serialization is within the budget.
    pub fits: bool,
    /// Items dropped from non-string arrays WITHOUT a marker element (the
    /// element type must not change). `> 0` means the caller has to say so.
    pub elided_items: usize,
}

/// Arrays at least this long may drop middle items (see module docs).
const ITEM_ELISION_MIN_LEN: usize = 9;

/// Smallest per-item budget (serialized chars) worth keeping an item for;
/// below it, items are dropped instead of mangled.
const MIN_ITEM_BUDGET: usize = 64;

fn ser_len(v: &Value) -> usize {
    v.to_string().chars().count()
}

/// `true` for an object that carries image data: a Read result
/// `{type:"image", ...}`, any object with a string `base64` field, or a Bash
/// result with `isImage: true` whose payload LOOKS like image data (a long
/// string field whose first 256 chars hold no whitespace, as base64 / a
/// `data:` URI does; plain multi-line text under a stray `isImage` flag is
/// still text). Such a payload is binary, not text: never shrunk or counted
/// as text.
pub fn is_image_payload(v: &Value) -> bool {
    let Value::Object(m) = v else {
        return false;
    };
    if m.get("type").and_then(Value::as_str) == Some("image")
        || m.get("base64").is_some_and(Value::is_string)
    {
        return true;
    }
    m.get("isImage") == Some(&Value::Bool(true))
        && m.values()
            .filter_map(Value::as_str)
            .any(|s| s.chars().count() > 256 && !s.chars().take(256).any(char::is_whitespace))
}

/// Shrink `v` so its compact serialization is at most `budget_chars`, keeping
/// the shape. Never returns something larger than the input.
pub fn shrink_value(v: &Value, budget_chars: usize) -> Shrunk {
    let before = ser_len(v);
    let mut elided_items = 0usize;
    let out = shrink_node(v, budget_chars, &mut elided_items);
    let after = ser_len(&out);
    if after > before {
        return Shrunk {
            value: v.clone(),
            fits: before <= budget_chars,
            elided_items: 0,
        };
    }
    Shrunk {
        fits: after <= budget_chars,
        value: out,
        elided_items,
    }
}

/// Shrink a raw string to at most `budget_chars` characters (head + truthful
/// marker + tail; plain prefix when the budget cannot hold the marker). Never
/// longer than the input.
pub fn shrink_str(s: &str, budget_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n <= budget_chars {
        return s.to_string();
    }
    let marker_for = |elided: usize| format!("\n…[elided {elided} chars of {n}]…\n");
    let worst = marker_for(n).chars().count();
    let out: String = if budget_chars >= worst + 16 {
        let keep = budget_chars - worst;
        let head = keep / 2;
        let tail = keep - head;
        let elided = n - head - tail;
        let mut o: String = chars[..head].iter().collect();
        o.push_str(&marker_for(elided));
        o.extend(chars[n - tail..].iter());
        o
    } else if budget_chars >= 2 {
        let mut o: String = chars[..budget_chars - 1].iter().collect();
        o.push('…');
        o
    } else if budget_chars == 1 {
        "…".to_string()
    } else {
        String::new()
    };
    if out.chars().count() >= n {
        s.to_string()
    } else {
        out
    }
}

/// Shrink a string VALUE so its JSON serialization (quotes and escapes
/// included) fits `budget`: binary-search the largest raw-char budget whose
/// escaped result fits, so escape-heavy text keeps head, tail and marker.
fn shrink_string_value(s: &str, budget: usize) -> Value {
    let ser = |t: &str| Value::String(t.to_string()).to_string().chars().count();
    let (mut lo, mut hi) = (0usize, budget.saturating_sub(2));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if ser(&shrink_str(s, mid)) <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Value::String(shrink_str(s, lo))
}

fn shrink_node(v: &Value, budget: usize, dropped: &mut usize) -> Value {
    let size = ser_len(v);
    if size <= budget || is_image_payload(v) {
        return v.clone();
    }
    let mut local = 0usize;
    let out = shrink_node_inner(v, budget, &mut local);
    // A field never grows. (If the node is returned unchanged, none of its
    // drops happened.)
    if ser_len(&out) > size {
        v.clone()
    } else {
        *dropped += local;
        out
    }
}

fn shrink_node_inner(v: &Value, budget: usize, dropped: &mut usize) -> Value {
    match v {
        Value::String(s) => shrink_string_value(s, budget),
        Value::Array(items) if items.len() >= ITEM_ELISION_MIN_LEN => {
            shrink_long_array(items, budget, dropped)
        }
        Value::Array(items) => {
            let sizes: Vec<usize> = items.iter().map(ser_len).collect();
            let overhead = ser_len(v).saturating_sub(sizes.iter().sum());
            let level = fair_level(&sizes, budget.saturating_sub(overhead));
            Value::Array(
                items
                    .iter()
                    .zip(&sizes)
                    .map(|(c, &s)| {
                        if s <= level {
                            c.clone()
                        } else {
                            shrink_node(c, level, dropped)
                        }
                    })
                    .collect(),
            )
        }
        Value::Object(map) => {
            let sizes: Vec<usize> = map.values().map(ser_len).collect();
            let overhead = ser_len(v).saturating_sub(sizes.iter().sum());
            let level = fair_level(&sizes, budget.saturating_sub(overhead));
            Value::Object(
                map.iter()
                    .zip(&sizes)
                    .map(|((k, c), &s)| {
                        let nc = if s <= level {
                            c.clone()
                        } else {
                            shrink_node(c, level, dropped)
                        };
                        (k.clone(), nc)
                    })
                    .collect(),
            )
        }
        other => other.clone(),
    }
}

/// Max-min fair level: the largest `L` with `sum(min(size, L)) <= avail`.
fn fair_level(sizes: &[usize], avail: usize) -> usize {
    let mut sorted: Vec<usize> = sizes.to_vec();
    sorted.sort_unstable();
    let mut remaining = avail;
    let mut left = sorted.len();
    for s in sorted {
        if left == 0 {
            break;
        }
        if s <= remaining / left {
            remaining -= s;
            left -= 1;
        } else {
            return remaining / left;
        }
    }
    usize::MAX
}

/// Shrink an array of [`ITEM_ELISION_MIN_LEN`]+ items of any type. Every item
/// is first trimmed to the fair level (never below [`MIN_ITEM_BUDGET`]); if the
/// trimmed items fit, all are kept. Otherwise a head and a tail of the trimmed
/// items are kept, chosen greedily by their ACTUAL sizes, and the middle is
/// dropped: an all-string array gets one marker string in the gap, any other
/// array gets nothing inserted and the drop is added to `dropped`.
fn shrink_long_array(items: &[Value], budget: usize, dropped: &mut usize) -> Value {
    let n = items.len();
    let all_strings = items.iter().all(Value::is_string);
    let sizes: Vec<usize> = items.iter().map(ser_len).collect();
    // `[` + `]` and n-1 commas.
    let level = fair_level(&sizes, budget.saturating_sub(n + 1)).max(MIN_ITEM_BUDGET);
    let mut local = 0usize;
    let trimmed: Vec<Value> = items
        .iter()
        .zip(&sizes)
        .map(|(c, &s)| {
            if s <= level {
                c.clone()
            } else {
                shrink_node(c, level, &mut local)
            }
        })
        .collect();
    let tsizes: Vec<usize> = trimmed.iter().map(ser_len).collect();
    if tsizes.iter().sum::<usize>() + n < budget {
        *dropped += local;
        return Value::Array(trimmed);
    }
    let marker = |k: usize| Value::String(format!("…[elided {k} of {n} items]…"));
    // Brackets, plus (for string arrays) the marker string and its comma.
    let reserved = 2 + if all_strings {
        ser_len(&marker(n)) + 1
    } else {
        0
    };
    let mut avail = budget.saturating_sub(reserved);
    let (mut head, mut tail) = (0usize, 0usize);
    let mut from_head = true;
    while head + tail < n {
        let idx = if from_head { head } else { n - 1 - tail };
        let cost = tsizes[idx] + 1;
        if cost > avail {
            break;
        }
        avail -= cost;
        if from_head {
            head += 1;
        } else {
            tail += 1;
        }
        from_head = !from_head;
    }
    let gap = n - head - tail;
    let mut out: Vec<Value> = trimmed[..head].to_vec();
    if all_strings {
        out.push(marker(gap));
    } else {
        *dropped += gap;
    }
    out.extend_from_slice(&trimmed[n - tail..]);
    *dropped += local;
    Value::Array(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn short_fields_are_kept_whole_and_big_one_fits() {
        let v = json!({"a": "x".repeat(30), "b": "y".repeat(60_000), "n": 3});
        let r = shrink_value(&v, 5_000);
        assert!(r.fits);
        assert_eq!(r.value["a"], "x".repeat(30));
        assert_eq!(r.value["n"], 3);
        assert!(r.value["b"].as_str().unwrap().contains("elided"));
    }

    #[test]
    fn nested_and_arrays_are_walked() {
        let v = json!({"file": {"content": "z".repeat(50_000)}, "names": (0..500).map(|i| format!("/p/{i}")).collect::<Vec<_>>()});
        let r = shrink_value(&v, 4_000);
        assert!(r.fits, "{}", r.value.to_string().len());
        assert!(r.value["file"]["content"].is_string());
        assert!(r.value["names"].as_array().unwrap().len() < 500);
    }

    #[test]
    fn single_line_keeps_head_and_tail_with_truthful_marker() {
        let s = "<d>x</d>".repeat(8_000);
        let t = shrink_str(&s, 1_000);
        assert!(t.chars().count() <= 1_000);
        assert!(t.starts_with("<d>x</d>") && t.ends_with("</d>"));
        assert!(t.contains(&format!("of {}", s.chars().count())));
    }

    #[test]
    fn never_grows() {
        let s = "short";
        assert_eq!(shrink_str(s, 1), "…");
        assert_eq!(shrink_str(s, 100), s);
    }

    #[test]
    fn non_string_array_drops_items_without_inserting_an_element() {
        let v = json!({"ids": (0..20_000).collect::<Vec<u32>>()});
        let r = shrink_value(&v, 8192);
        assert!(r.fits);
        assert!(r.elided_items > 0);
        assert!(r.value["ids"].as_array().unwrap().iter().all(Value::is_u64));
    }

    #[test]
    fn image_payload_is_untouched() {
        let v = json!({"type": "image", "file": {"base64": "A".repeat(50_000)}});
        let r = shrink_value(&v, 1_000);
        assert_eq!(r.value, v);
        assert!(!r.fits);
    }
}
