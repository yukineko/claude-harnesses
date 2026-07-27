//! Tiny declarative schema engine — no external JSON-Schema crate.
//!
//! Schemas are assembled from [`Field`] descriptors and validated with the
//! pure [`validate_report`] function, which accumulates findings rather than
//! short-circuiting. This makes the full error set available to the caller
//! who wants to re-ask the LLM with a precise prompt.
//!
//! # Three answers per declared check, not two
//!
//! A `Vec<Violation>` has exactly two states — empty and non-empty — so it
//! collapses three genuinely different outcomes into one "empty" answer:
//!
//! 1. the check **ran and passed**,
//! 2. the check was **deliberately not run** (`Ty::Any`, `required: false` on an
//!    absent field, an undeclared extra field, an `Array` with no `items`),
//! 3. the check **could not be run to a conclusion** (a declared constraint the
//!    engine cannot apply to the value it was handed).
//!
//! [`Report`] keeps those apart: [`Report::violations`] (ran, failed),
//! [`Report::waived`] (deliberately not run, with the declaration that makes it
//! deliberate) and [`Report::undetermined`] (could not run). [`Report::verdict`]
//! resolves them onto `harness_core`'s three-valued [`Verdict`], where
//! `Undetermined` blocks exactly like `Violation` and only an observed-empty
//! finding set can mint a `Clean`.
//!
//! [`validate`] remains as the two-valued adapter for callers that consume a
//! plain `Vec<Violation>`; it folds `undetermined` onto the **restricted** side
//! (see its docs), never the permissive one.

use harness_core::verdict::Verdict;

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
    /// When this slice is non-empty it declares a per-element schema, and that
    /// declaration is a constraint the engine must *apply*: each element of the
    /// value (which must be an object) is recursively validated against these
    /// sub-fields. A value that is not an array cannot have the constraint
    /// applied at all, so it resolves to [`Undetermined`] — not to a silent
    /// pass. Leaving this slice empty is the way to declare "the elements are
    /// deliberately not inspected" (recorded as [`Waived`]).
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

/// A declared check the engine **could not run to a conclusion**.
///
/// This is not a statement about the payload (it may well be fine) — it is a
/// refusal to guess: a constraint was declared, the engine could not apply it,
/// and "could not check" is not "passed". It resolves to the restricted side
/// everywhere (`Verdict::Undetermined` from [`Report::verdict`], exit 2 from the
/// CLI, a `Violation`-shaped entry in the two-valued [`validate`] adapter).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Undetermined {
    pub path: String,
    pub problem: String,
}

/// A declared check that was **deliberately not performed**, together with the
/// declaration that makes skipping it intentional.
///
/// Waivers are the opposite of [`Undetermined`]: the schema author wrote down
/// that this check does not apply (`required: false` on an absent field,
/// `Ty::Any`, an `Array` with no `items`, a field the schema never declared), so
/// nothing is wrong. They are recorded — not dropped — because a verdict that
/// stays silent about them claims a completeness it does not have: "checked and
/// clean" and "never checked" would otherwise be the same empty set downstream.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Waived {
    pub path: String,
    pub reason: String,
}

/// The full outcome of a validation run: what failed, what could not be
/// determined, and what was deliberately not checked.
///
/// Constructible only by [`validate_report`] (the fields are private and there
/// is no `Default`), so an empty `Report` is always the record of a run that
/// actually happened — never a value someone minted to stand in for one.
#[must_use = "a validation report must be resolved (see `verdict`), never computed and dropped"]
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    violations: Vec<Violation>,
    undetermined: Vec<Undetermined>,
    waived: Vec<Waived>,
}

impl Report {
    /// A run that has recorded nothing yet. Private: only [`validate_report`]
    /// starts one, so "empty report" always means "the validator ran".
    fn started() -> Self {
        Report {
            violations: Vec::new(),
            undetermined: Vec::new(),
            waived: Vec::new(),
        }
    }

    /// Merge a sub-report (array element recursion) into this one.
    fn absorb(&mut self, mut other: Report) {
        self.violations.append(&mut other.violations);
        self.undetermined.append(&mut other.undetermined);
        self.waived.append(&mut other.waived);
    }

    /// Checks that ran and failed.
    #[must_use]
    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }

    /// Declared checks that could not be run to a conclusion.
    #[must_use]
    pub fn undetermined(&self) -> &[Undetermined] {
        &self.undetermined
    }

    /// Checks that were deliberately not performed, each with its declaration.
    #[must_use]
    pub fn waived(&self) -> &[Waived] {
        &self.waived
    }

    /// Resolve the report onto the shared three-valued gate verdict.
    ///
    /// `Violation` outranks `Undetermined`, which outranks `Clean` — the
    /// ordering [`Verdict::worst_of`] fixes repo-wide. Both non-clean answers
    /// block; the `Clean` is minted by `harness_core` only from the findings
    /// actually collected here, so it cannot be forged by this crate.
    // No `#[must_use]` here: `Verdict` already carries one, with a better
    // message ("a gate verdict must be acted on, never computed and dropped").
    pub fn verdict(&self) -> Verdict {
        Verdict::worst_of(
            self.violations
                .iter()
                .map(|v| Verdict::violation(format!("{}: {}", v.path, v.problem)))
                .chain(
                    self.undetermined
                        .iter()
                        .map(|u| Verdict::undetermined(format!("{}: {}", u.path, u.problem))),
                ),
        )
    }

    /// Two-valued adapter: the findings a `Vec<Violation>` consumer must act on.
    ///
    /// Every [`Undetermined`] is folded in as a `Violation`-shaped entry, because
    /// a caller that only understands "empty vs non-empty" must see "could not
    /// check" on the **restricted** side. Waivers are dropped here — they carry
    /// no obligation, and a two-valued consumer has nowhere to put them.
    #[must_use]
    pub fn into_violations(self) -> Vec<Violation> {
        let mut out = self.violations;
        out.extend(self.undetermined.into_iter().map(|u| Violation {
            path: u.path,
            problem: u.problem,
        }));
        out
    }
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
/// `path`, and return the two-valued finding list.
///
/// This is the adapter kept for consumers that branch on
/// `violations.is_empty()`. It is [`validate_report`] with the waivers dropped
/// and every [`Undetermined`] folded in as a `Violation`-shaped entry — a
/// two-valued consumer must see "could not check" on the restricted side, since
/// its only other option would be to read it as "passed".
///
/// Prefer [`validate_report`] where the three answers can be kept apart.
///
/// This function is **pure** — it has no side effects.
pub fn validate(value: &serde_json::Value, fields: &[Field], path: &str) -> Vec<Violation> {
    validate_report(value, fields, path).into_violations()
}

/// Validate `value` (an object) against `fields`, prefixing every path with
/// `path`, keeping "failed" / "could not check" / "deliberately not checked"
/// as three separate answers.
///
/// Unknown extra fields are allowed — and recorded as [`Waived`], so the
/// permission stays visible instead of being indistinguishable from a field
/// that was actually inspected.
///
/// This function is **pure** — it has no side effects.
pub fn validate_report(value: &serde_json::Value, fields: &[Field], path: &str) -> Report {
    let mut report = Report::started();

    let obj = match value.as_object() {
        Some(o) => o,
        None => {
            report.violations.push(Violation {
                path: path.to_string(),
                problem: format!("expected object, got {}", json_type_name(value)),
            });
            return report;
        }
    };

    let child_path = |name: &str| -> String {
        if path.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", path, name)
        }
    };

    for field in fields {
        let field_path = child_path(field.name);

        let val = obj.get(field.name);

        // Required check
        if val.is_none() {
            if field.required {
                report.violations.push(Violation {
                    path: field_path,
                    problem: "required field missing".to_string(),
                });
            } else {
                // Declared permissive: `required: false` says absence is fine.
                // Recorded so the verdict does not imply that the type/enum/items
                // constraints declared for this field were evaluated — they were
                // not; there was no value to evaluate them against.
                report.waived.push(Waived {
                    path: field_path,
                    reason: "absent and declared optional (required: false) — its type/enum/items constraints were not evaluated".to_string(),
                });
            }
            continue;
        }

        let val = match val {
            Some(v) => v,
            // Unreachable: guarded by `val.is_none()` above. Resolved to the
            // restricted side anyway rather than assumed away, so a future edit
            // to that guard cannot turn this into a silent skip.
            None => {
                report.undetermined.push(Undetermined {
                    path: field_path,
                    problem: "value could not be read from the object".to_string(),
                });
                continue;
            }
        };

        // Type check
        let type_ok = match &field.ty {
            Ty::String => val.is_string(),
            Ty::Number => val.is_number(),
            Ty::Bool => val.is_boolean(),
            Ty::Array => val.is_array(),
            Ty::Object => val.is_object(),
            Ty::Any => {
                // Declared permissive: `Ty::Any` accepts any JSON value without
                // a type check. Recorded so "no type check ran" is visible.
                report.waived.push(Waived {
                    path: field_path.clone(),
                    reason: "declared Ty::Any — no type check performed".to_string(),
                });
                true
            }
        };

        if !type_ok {
            report.violations.push(Violation {
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
                        report.violations.push(Violation {
                            path: field_path.clone(),
                            problem: format!("'{}' not in [{}]", s, allowed),
                        });
                    }
                }
                None => report.violations.push(Violation {
                    path: field_path.clone(),
                    problem: format!(
                        "enum constraint [{}] cannot be applied: value is not a string, got {}",
                        allowed,
                        json_type_name(val)
                    ),
                }),
            }
        }

        // Recurse into array elements when an items schema is declared.
        //
        // The trigger is the *declaration* (`items` non-empty), not the declared
        // type: an `items` slice is a constraint that must be applied, exactly
        // like `enum_values` above. Gating the recursion on `field.ty ==
        // Ty::Array` used to drop the constraint in silence whenever a caller
        // paired `items` with `Ty::Any`/`Ty::Object`, and additionally split the
        // "is it an array?" judgement across two expressions (`Ty::Array =>
        // val.is_array()` and `val.as_array()`), whose `None` arm was a bare
        // no-op held harmless only by the other one. One judgement now, and a
        // value the declared items schema cannot be applied to is undetermined.
        if !field.items.is_empty() {
            match val.as_array() {
                Some(arr) => {
                    for (i, elem) in arr.iter().enumerate() {
                        let elem_path = format!("{}[{}]", field_path, i);
                        report.absorb(validate_report(elem, field.items, &elem_path));
                    }
                }
                None => report.undetermined.push(Undetermined {
                    path: field_path.clone(),
                    problem: format!(
                        "items schema ({} declared sub-field(s)) cannot be applied: value is not an array, got {}",
                        field.items.len(),
                        json_type_name(val)
                    ),
                }),
            }
        } else if field.ty == Ty::Array {
            // Declared permissive: an array with no `items` schema is a
            // deliberate "elements are not inspected".
            report.waived.push(Waived {
                path: field_path.clone(),
                reason: "no items schema declared — array elements were not inspected".to_string(),
            });
        }
    }

    // Undeclared keys. Allowing them is deliberate (the doc contract of this
    // engine), but staying silent about them would let a verdict read as "the
    // whole payload was inspected".
    for key in obj.keys() {
        if !fields.iter().any(|f| f.name == key.as_str()) {
            report.waived.push(Waived {
                path: child_path(key),
                reason: "not declared in this schema — unknown extra fields are allowed and were not inspected".to_string(),
            });
        }
    }

    report
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

    static ITEMS_UNDER_ANY_FIELDS: &[Field] = &[Field {
        name: "config",
        ty: Ty::Any,
        required: true,
        enum_values: &[],
        items: ITEM_FIELDS,
    }];

    #[test]
    fn declared_items_that_cannot_be_applied_is_not_a_silent_pass() {
        // Twin of `enum_on_non_string_value_is_a_violation_not_a_silent_pass`.
        // A declared `items` sub-schema is a constraint that must be *applied*.
        // The recursion used to be gated on `field.ty == Ty::Array`, so a caller
        // pairing `items` with `Ty::Any`/`Ty::Object` handed the engine a
        // constraint it silently dropped: the value below is an object, the
        // declared per-element check never ran, and `validate` returned an empty
        // set — "could not check" read as "passed".
        let v = json!({"config": {"id": "a"}});
        let violations = validate(&v, ITEMS_UNDER_ANY_FIELDS, "");
        assert!(
            !violations.is_empty(),
            "a declared items schema that cannot be applied must not validate clean, got: {:?}",
            violations
        );
    }

    #[test]
    fn declared_items_under_any_are_actually_applied_to_array_elements() {
        // The constraint is not merely reported as inapplicable — when the value
        // IS an array, the declared per-element schema is enforced.
        let v = json!({"config": [{"id": "a"}, {"not_id": "b"}]});
        let violations = validate(&v, ITEMS_UNDER_ANY_FIELDS, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path == "config[1].id" && vi.problem.contains("required")),
            "got: {:?}",
            violations
        );
    }

    #[test]
    fn declared_items_under_any_still_pass_on_conforming_elements() {
        // Anti-vacuity control: the restrictive change must not reject valid data.
        let v = json!({"config": [{"id": "a"}]});
        let violations = validate(&v, ITEMS_UNDER_ANY_FIELDS, "");
        assert!(violations.is_empty(), "got: {:?}", violations);
    }

    // ── the three answers, kept apart ────────────────────────────────────────

    /// An optional field carrying an enum constraint — the shape of
    /// `decomposition`'s per-task `class`, whose constraint is never evaluated
    /// when the field is absent.
    static OPTIONAL_ENUM_FIELDS: &[Field] = &[
        Field {
            name: "name",
            ty: Ty::String,
            required: true,
            enum_values: &[],
            items: &[],
        },
        Field {
            name: "role",
            ty: Ty::String,
            required: false,
            enum_values: &["admin", "user"],
            items: &[],
        },
    ];

    #[test]
    fn report_distinguishes_checked_clean_from_never_checked() {
        // Both payloads produce zero violations, but only one of them had its
        // `role` enum constraint evaluated. A `Vec<Violation>` cannot tell them
        // apart; the report can.
        let checked = validate_report(
            &json!({"name": "A", "role": "admin"}),
            OPTIONAL_ENUM_FIELDS,
            "",
        );
        let never_checked = validate_report(&json!({"name": "A"}), OPTIONAL_ENUM_FIELDS, "");

        assert!(checked.violations().is_empty());
        assert!(never_checked.violations().is_empty());
        // Same two-valued answer …
        assert_eq!(checked.violations().len(), never_checked.violations().len());
        // … different three-valued one.
        assert!(
            checked.waived().iter().all(|w| w.path != "role"),
            "the evaluated enum must not be reported as skipped, got: {:?}",
            checked.waived()
        );
        assert!(
            never_checked.waived().iter().any(|w| w.path == "role"),
            "the un-evaluated enum must be reported as skipped, got: {:?}",
            never_checked.waived()
        );
    }

    #[test]
    fn waived_checks_do_not_block() {
        // A deliberate permissive is not a violation and not an undetermined:
        // the verdict stays clean. (False positives here would reject every
        // decomposition whose tasks omit an optional field.)
        let r = validate_report(
            &json!({"name": "A", "unknown_extra": 1}),
            OPTIONAL_ENUM_FIELDS,
            "",
        );
        assert!(!r.verdict().blocks(), "got: {:?}", r);
        assert!(
            r.waived().iter().any(|w| w.path == "unknown_extra"),
            "an undeclared field is allowed, but the permission must be visible: {:?}",
            r.waived()
        );
    }

    #[test]
    fn inapplicable_items_is_undetermined_not_a_violation_and_blocks() {
        // "Could not check" is its own answer: the payload is not accused of
        // being wrong, but the verdict still blocks rather than passing.
        let r = validate_report(&json!({"config": {"id": "a"}}), ITEMS_UNDER_ANY_FIELDS, "");
        assert!(
            r.violations().is_empty(),
            "the value itself was not judged wrong, got: {:?}",
            r.violations()
        );
        assert!(
            r.undetermined().iter().any(|u| u.path == "config"),
            "got: {:?}",
            r.undetermined()
        );
        assert!(
            matches!(r.verdict(), Verdict::Undetermined(_)),
            "got: {:?}",
            r.verdict()
        );
        assert!(r.verdict().blocks());
    }

    #[test]
    fn two_valued_adapter_folds_undetermined_onto_the_restricted_side() {
        // A `Vec<Violation>` consumer has only two states, so "could not check"
        // must arrive as non-empty — the same side as a violation, never as the
        // empty set that reads as "passed".
        let r = validate_report(&json!({"config": {"id": "a"}}), ITEMS_UNDER_ANY_FIELDS, "");
        assert!(r.violations().is_empty());
        assert!(!r.into_violations().is_empty());
    }

    #[test]
    fn any_type_is_waived_not_undetermined() {
        // `Ty::Any` is a declared "do not type-check this", not a failure to
        // check: it must stay on the permissive side of the three answers.
        let r = validate_report(&json!({"metadata": 42}), ANY_AND_OBJECT_FIELDS, "");
        assert!(r.undetermined().is_empty(), "got: {:?}", r.undetermined());
        assert!(
            r.waived().iter().any(|w| w.path == "metadata"),
            "got: {:?}",
            r.waived()
        );
        assert!(!r.verdict().blocks());
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
