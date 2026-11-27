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
//! - an array of 9+ strings (file lists) drops middle items and puts ONE marker
//!   string in their place (still an array of strings);
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
}

/// Arrays of strings at least this long are shrunk by dropping middle items.
const ITEM_ELISION_MIN_LEN: usize = 9;

fn ser_len(v: &Value) -> usize {
    v.to_string().chars().count()
}

/// Shrink `v` so its compact serialization is at most `budget_chars`, keeping
/// the shape. Never returns something larger than the input.
pub fn shrink_value(v: &Value, budget_chars: usize) -> Shrunk {
    let before = ser_len(v);
    let out = shrink_node(v, budget_chars);
    let after = ser_len(&out);
    if after > before {
        return Shrunk {
            value: v.clone(),
            fits: before <= budget_chars,
        };
    }
    Shrunk {
        fits: after <= budget_chars,
        value: out,
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
/// included) fits `budget`.
fn shrink_string_value(s: &str, budget: usize) -> Value {
    let mut b = budget.saturating_sub(2);
    for _ in 0..8 {
        let t = shrink_str(s, b);
        let ser = Value::String(t.clone()).to_string().chars().count();
        if ser <= budget || b == 0 {
            return Value::String(t);
        }
        b = b.saturating_sub(ser - budget);
    }
    Value::String(shrink_str(s, 0))
}

fn shrink_node(v: &Value, budget: usize) -> Value {
    if ser_len(v) <= budget {
        return v.clone();
    }
    match v {
        Value::String(s) => shrink_string_value(s, budget),
        Value::Array(items)
            if items.len() >= ITEM_ELISION_MIN_LEN && items.iter().all(Value::is_string) =>
        {
            shrink_string_items(items, budget)
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
                            shrink_node(c, level)
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
                            shrink_node(c, level)
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

/// Keep head and tail items of an all-string array within `budget`, replacing
/// the dropped middle by one truthful marker string.
fn shrink_string_items(items: &[Value], budget: usize) -> Value {
    let n = items.len();
    let marker = |k: usize| Value::String(format!("…[elided {k} of {n} items]…"));
    let marker_cost = ser_len(&marker(n)) + 1;
    // `[` + `]`, plus the marker and its comma.
    let mut avail = budget.saturating_sub(2 + marker_cost);
    let (mut head, mut tail) = (0usize, 0usize);
    let mut from_head = true;
    loop {
        if head + tail >= n {
            break;
        }
        let idx = if from_head { head } else { n - 1 - tail };
        let cost = ser_len(&items[idx]) + 1;
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
    let mut out: Vec<Value> = items[..head].to_vec();
    out.push(marker(n - head - tail));
    out.extend_from_slice(&items[n - tail..]);
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
}
