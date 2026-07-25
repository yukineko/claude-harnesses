//! Tiny declarative schema engine — no external JSON-Schema crate.
//!
//! Schemas are assembled from [`Field`] descriptors and validated with the
//! pure [`validate`] function, which accumulates [`Violation`]s rather than
//! short-circuiting. This makes the full error set available to the caller
//! who wants to re-ask the LLM with a precise prompt.

/// The set of JSON value types that a field may declare.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    String,
    Number,
    Bool,
    Array,
    /// Accept an object value. Reserved for schemas that embed sub-objects
    /// inline without a named per-element items slice.
    #[allow(dead_code)]
    Object,
    /// Accept any JSON value without a type check.
    #[allow(dead_code)]
    Any,
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ty::String => write!(f, "string"),
            Ty::Number => write!(f, "number"),
            Ty::Bool => write!(f, "bool"),
            Ty::Array => write!(f, "array"),
            Ty::Object => write!(f, "object"),
            Ty::Any => write!(f, "any"),
        }
    }
}

/// A single field descriptor inside a schema.
#[derive(Debug, Clone)]
pub struct Field {
    pub name: &'static str,
    /// The expected JSON type for this field.
    pub ty: Ty,
    /// Whether absence of this key is a violation.
    pub required: bool,
    /// If non-empty, the string value must be one of these.
    pub enum_values: &'static [&'static str],
    /// When `ty == Array` and this slice is non-empty, each element (which
    /// must be an object) is recursively validated against these sub-fields.
    pub items: &'static [Field],
}

/// A named schema — wraps a list of top-level [`Field`]s.
#[derive(Debug, Clone)]
pub struct Schema {
    pub name: String,
    pub fields: Vec<Field>,
}

/// A single validation failure — the `path` locates the offending value and
/// `problem` describes what was wrong. These are the atoms of the re-ask error.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Violation {
    pub path: String,
    pub problem: String,
}

/// Returns the name of the JSON type tag for the given value, for error messages.
fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Validate `value` (an object) against `fields`, prefixing every path with
/// `path`. Unknown extra fields are silently allowed.
///
/// This function is **pure** — it has no side effects.
pub fn validate(value: &serde_json::Value, fields: &[Field], path: &str) -> Vec<Violation> {
    let mut violations: Vec<Violation> = Vec::new();

    let obj = match value.as_object() {
        Some(o) => o,
        None => {
            violations.push(Violation {
                path: path.to_string(),
                problem: format!("expected object, got {}", json_type_name(value)),
            });
            return violations;
        }
    };

    for field in fields {
        let field_path = if path.is_empty() {
            field.name.to_string()
        } else {
            format!("{}.{}", path, field.name)
        };

        let val = obj.get(field.name);

        // Required check
        if val.is_none() {
            if field.required {
                violations.push(Violation {
                    path: field_path,
                    problem: "required field missing".to_string(),
                });
            }
            continue;
        }

        let val = val.unwrap();

        // Type check
        let type_ok = match &field.ty {
            Ty::String => val.is_string(),
            Ty::Number => val.is_number(),
            Ty::Bool => val.is_boolean(),
            Ty::Array => val.is_array(),
            Ty::Object => val.is_object(),
            Ty::Any => true,
        };

        if !type_ok {
            violations.push(Violation {
                path: field_path.clone(),
                problem: format!("expected {}, got {}", field.ty, json_type_name(val)),
            });
            // No point doing further checks on a mistyped value.
            continue;
        }

        // Enum check. A declared `enum_values` set is a constraint that must be
        // *applied*, so a value the constraint cannot be evaluated against (a
        // non-string, reachable when a caller pairs `enum_values` with `Ty::Any`
        // or a numeric type) is a violation — not a silent skip. "Could not
        // check" is not "passed".
        if !field.enum_values.is_empty() {
            let allowed = field
                .enum_values
                .iter()
                .map(|v| format!("\"{}\"", v))
                .collect::<Vec<_>>()
                .join(", ");
            match val.as_str() {
                Some(s) => {
                    if !field.enum_values.contains(&s) {
                        violations.push(Violation {
                            path: field_path.clone(),
                            problem: format!("'{}' not in [{}]", s, allowed),
                        });
                    }
                }
                None => violations.push(Violation {
                    path: field_path.clone(),
                    problem: format!(
                        "enum constraint [{}] cannot be applied: value is not a string, got {}",
                        allowed,
                        json_type_name(val)
                    ),
                }),
            }
        }

        // Recurse into array elements when items schema is provided
        if field.ty == Ty::Array && !field.items.is_empty() {
            if let Some(arr) = val.as_array() {
                for (i, elem) in arr.iter().enumerate() {
                    let elem_path = format!("{}[{}]", field_path, i);
                    let mut sub = validate(elem, field.items, &elem_path);
                    violations.append(&mut sub);
                }
            }
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── helpers ──────────────────────────────────────────────────────────────

    static SIMPLE_FIELDS: &[Field] = &[
        Field {
            name: "name",
            ty: Ty::String,
            required: true,
            enum_values: &[],
            items: &[],
        },
        Field {
            name: "age",
            ty: Ty::Number,
            required: false,
            enum_values: &[],
            items: &[],
        },
        Field {
            name: "active",
            ty: Ty::Bool,
            required: false,
            enum_values: &[],
            items: &[],
        },
        Field {
            name: "role",
            ty: Ty::String,
            required: true,
            enum_values: &["admin", "user", "guest"],
            items: &[],
        },
    ];

    static ITEM_FIELDS: &[Field] = &[Field {
        name: "id",
        ty: Ty::String,
        required: true,
        enum_values: &[],
        items: &[],
    }];

    static ARRAY_FIELD: &[Field] = &[Field {
        name: "items",
        ty: Ty::Array,
        required: true,
        enum_values: &[],
        items: ITEM_FIELDS,
    }];

    static ANY_AND_OBJECT_FIELDS: &[Field] = &[
        Field {
            name: "metadata",
            ty: Ty::Any,
            required: false,
            enum_values: &[],
            items: &[],
        },
        Field {
            name: "extra",
            ty: Ty::Object,
            required: false,
            enum_values: &[],
            items: &[],
        },
    ];

    // ── test cases ────────────────────────────────────────────────────────────

    #[test]
    fn all_valid_returns_empty() {
        let v = json!({"name": "Alice", "role": "admin", "age": 30, "active": true});
        let violations = validate(&v, SIMPLE_FIELDS, "");
        assert!(
            violations.is_empty(),
            "expected no violations, got {:?}",
            violations
        );
    }

    #[test]
    fn required_field_missing() {
        // Missing both "name" and "role"
        let v = json!({"age": 25});
        let violations = validate(&v, SIMPLE_FIELDS, "");
        assert!(violations
            .iter()
            .any(|vi| vi.path == "name" && vi.problem.contains("required")));
        assert!(violations
            .iter()
            .any(|vi| vi.path == "role" && vi.problem.contains("required")));
    }

    #[test]
    fn type_mismatch_string_expected() {
        let v = json!({"name": 42, "role": "admin"});
        let violations = validate(&v, SIMPLE_FIELDS, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path == "name" && vi.problem.contains("expected string")),
            "got: {:?}",
            violations
        );
    }

    #[test]
    fn type_mismatch_bool_expected() {
        let v = json!({"name": "Alice", "role": "admin", "active": "yes"});
        let violations = validate(&v, SIMPLE_FIELDS, "");
        assert!(violations
            .iter()
            .any(|vi| vi.path == "active" && vi.problem.contains("expected bool")));
    }

    #[test]
    fn enum_violation() {
        let v = json!({"name": "Bob", "role": "superadmin"});
        let violations = validate(&v, SIMPLE_FIELDS, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path == "role" && vi.problem.contains("not in")),
            "got: {:?}",
            violations
        );
    }

    #[test]
    fn enum_valid_value_passes() {
        let v = json!({"name": "Carol", "role": "guest"});
        let violations = validate(&v, SIMPLE_FIELDS, "");
        assert!(violations.is_empty(), "got: {:?}", violations);
    }

    #[test]
    fn nested_array_element_violation() {
        // items[1] is missing "id"
        let v = json!({"items": [{"id": "a"}, {"not_id": "b"}]});
        let violations = validate(&v, ARRAY_FIELD, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path == "items[1].id" && vi.problem.contains("required")),
            "got: {:?}",
            violations
        );
    }

    #[test]
    fn nested_array_all_valid() {
        let v = json!({"items": [{"id": "a"}, {"id": "b"}]});
        let violations = validate(&v, ARRAY_FIELD, "");
        assert!(violations.is_empty(), "got: {:?}", violations);
    }

    #[test]
    fn unknown_extra_fields_are_allowed() {
        let v = json!({"name": "Dan", "role": "user", "extra_unknown_field": true});
        let violations = validate(&v, SIMPLE_FIELDS, "");
        assert!(violations.is_empty(), "got: {:?}", violations);
    }

    #[test]
    fn path_prefix_is_prepended() {
        let v = json!({"other": "missing required role and name"});
        let violations = validate(&v, SIMPLE_FIELDS, "root");
        assert!(violations.iter().any(|vi| vi.path.starts_with("root.")));
    }

    #[test]
    fn non_object_top_level_produces_violation() {
        let v = json!([1, 2, 3]);
        let violations = validate(&v, SIMPLE_FIELDS, "");
        assert!(!violations.is_empty());
        assert!(violations[0].problem.contains("expected object"));
    }

    #[test]
    fn any_type_accepts_all_values() {
        // Ty::Any and Ty::Object are valid field types — verify they don't produce
        // false violations on correctly-typed values.
        let v = json!({"metadata": 42, "extra": {"key": "val"}});
        let violations = validate(&v, ANY_AND_OBJECT_FIELDS, "");
        assert!(violations.is_empty(), "got {:?}", violations);
    }

    static ANY_WITH_ENUM_FIELDS: &[Field] = &[Field {
        name: "mode",
        ty: Ty::Any,
        required: true,
        enum_values: &["fast", "slow"],
        items: &[],
    }];

    #[test]
    fn enum_on_non_string_value_is_a_violation_not_a_silent_pass() {
        // A declared constraint that cannot be applied must resolve restrictively.
        // Previously the enum check sat inside `if let Some(s) = val.as_str()` with
        // no else arm, so a non-string value skipped the constraint entirely and
        // validated clean — "could not check" silently became "passed".
        //
        // Not reachable through the five registered schemas today (every field
        // carrying `enum_values` is `Ty::String`, so the type check rejects first),
        // but `validate`/`Field` are public API, so an external caller pairing
        // `enum_values` with `Ty::Any`/`Ty::Number` hits it.
        let v = json!({"mode": 42});
        let violations = validate(&v, ANY_WITH_ENUM_FIELDS, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path == "mode" && vi.problem.contains("not a string")),
            "a non-string value under an enum constraint must violate, got: {:?}",
            violations
        );
    }

    #[test]
    fn enum_on_matching_string_still_passes_under_any() {
        // Control arm: the restrictive change must not reject legitimate values.
        let v = json!({"mode": "fast"});
        let violations = validate(&v, ANY_WITH_ENUM_FIELDS, "");
        assert!(violations.is_empty(), "got: {:?}", violations);
    }

    #[test]
    fn object_type_rejects_non_object() {
        let v = json!({"extra": "not an object"});
        let violations = validate(&v, ANY_AND_OBJECT_FIELDS, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path == "extra" && vi.problem.contains("expected object")),
            "got {:?}",
            violations
        );
    }
}
