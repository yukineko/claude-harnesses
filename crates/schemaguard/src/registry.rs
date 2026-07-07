//! Named schema registry — maps well-known schema names to their [`Schema`]
//! descriptors. Add new schemas here; the rest of the codebase discovers them
//! via [`get`] / [`names`].

use crate::schema::{Field, Schema, Ty};

// ── static field slices ──────────────────────────────────────────────────────

// decomposition → tasks items
//
// `required` here is deliberately kept in lockstep with condukt's
// `model::Task` struct: only `id` lacks `#[serde(default)]` there (every
// other field is `#[serde(default)]`, so absence deserializes to a default
// rather than a serde error). Marking `title`/`class`/`done_criteria` as
// schema-required here — while `model::Task` treats them as optional — would
// make this precheck reject decompositions that condukt's own parser has
// always accepted, breaking the "valid input is byte-identical" guarantee
// once this schema is wired into condukt's parse boundary.
static DECOMPOSITION_TASK_FIELDS: &[Field] = &[
    Field {
        name: "id",
        ty: Ty::String,
        required: true,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "title",
        ty: Ty::String,
        required: false,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "class",
        ty: Ty::String,
        required: false,
        enum_values: &["parallel", "serial", "gated"],
        items: &[],
    },
    Field {
        name: "done_criteria",
        ty: Ty::String,
        required: false,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "suggested_model",
        ty: Ty::String,
        required: false,
        enum_values: &["haiku", "sonnet", "opus"],
        items: &[],
    },
    Field {
        name: "confidence",
        ty: Ty::String,
        required: false,
        enum_values: &["high", "medium", "low"],
        items: &[],
    },
];

// decomposition top-level fields
static DECOMPOSITION_FIELDS: &[Field] = &[
    Field {
        name: "goal",
        ty: Ty::String,
        required: true,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "tasks",
        ty: Ty::Array,
        required: true,
        enum_values: &[],
        items: DECOMPOSITION_TASK_FIELDS,
    },
];

// episode top-level fields
static EPISODE_FIELDS: &[Field] = &[
    Field {
        name: "title",
        ty: Ty::String,
        required: true,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "model",
        ty: Ty::String,
        required: true,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "pass",
        ty: Ty::Bool,
        required: true,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "class",
        ty: Ty::String,
        required: false,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "role",
        ty: Ty::String,
        required: false,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "cost_usd",
        ty: Ty::Number,
        required: false,
        enum_values: &[],
        items: &[],
    },
];

// playbook top-level fields
static PLAYBOOK_FIELDS: &[Field] = &[
    Field {
        name: "title",
        ty: Ty::String,
        required: true,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "done_criteria",
        ty: Ty::String,
        required: false,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "class",
        ty: Ty::String,
        required: false,
        enum_values: &[],
        items: &[],
    },
];

// scout-measure top-level fields
static SCOUT_MEASURE_FIELDS: &[Field] = &[
    Field {
        name: "title",
        ty: Ty::String,
        required: true,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "lens",
        ty: Ty::String,
        required: true,
        enum_values: &["L1", "L2", "L3", "L4", "L5"],
        items: &[],
    },
    Field {
        name: "severity",
        ty: Ty::String,
        required: true,
        enum_values: &["high", "medium", "low"],
        items: &[],
    },
    Field {
        name: "effort",
        ty: Ty::String,
        required: true,
        enum_values: &["xs", "s", "m", "l", "xl"],
        items: &[],
    },
    Field {
        name: "evidence",
        ty: Ty::String,
        required: true,
        enum_values: &[],
        items: &[],
    },
];

// verdict top-level fields — matches condukt's consensus::Verdict shape
static VERDICT_FIELDS: &[Field] = &[
    Field {
        name: "candidate",
        ty: Ty::String,
        required: true,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "pass",
        ty: Ty::Bool,
        required: true,
        enum_values: &[],
        items: &[],
    },
    Field {
        name: "group",
        ty: Ty::String,
        required: false,
        enum_values: &[],
        items: &[],
    },
];

// ── public API ───────────────────────────────────────────────────────────────

/// All registered schema names in a stable order (used by `schemaguard list`).
pub fn names() -> Vec<&'static str> {
    vec![
        "decomposition",
        "episode",
        "playbook",
        "scout-measure",
        "verdict",
    ]
}

/// Look up a schema by name. Returns `None` for unknown names.
pub fn get(name: &str) -> Option<Schema> {
    match name {
        "decomposition" => Some(Schema {
            name: "decomposition".to_string(),
            fields: DECOMPOSITION_FIELDS.to_vec(),
        }),
        "episode" => Some(Schema {
            name: "episode".to_string(),
            fields: EPISODE_FIELDS.to_vec(),
        }),
        "playbook" => Some(Schema {
            name: "playbook".to_string(),
            fields: PLAYBOOK_FIELDS.to_vec(),
        }),
        "scout-measure" => Some(Schema {
            name: "scout-measure".to_string(),
            fields: SCOUT_MEASURE_FIELDS.to_vec(),
        }),
        "verdict" => Some(Schema {
            name: "verdict".to_string(),
            fields: VERDICT_FIELDS.to_vec(),
        }),
        _ => None,
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::validate;
    use serde_json::json;

    // ── decomposition ──────────────────────────────────────────────────────

    #[test]
    fn decomposition_valid() {
        let schema = get("decomposition").unwrap();
        let v = json!({
            "goal": "Build a feature",
            "tasks": [
                {
                    "id": "t1",
                    "title": "Implement API",
                    "class": "parallel",
                    "done_criteria": "tests pass",
                    "suggested_model": "sonnet",
                    "confidence": "high"
                }
            ]
        });
        let violations = validate(&v, &schema.fields, "");
        assert!(
            violations.is_empty(),
            "expected valid, got {:?}",
            violations
        );
    }

    #[test]
    fn decomposition_missing_task_id() {
        // `id` is the only truly required per-task field (matches condukt's
        // `model::Task`, where every other field is `#[serde(default)]`).
        let schema = get("decomposition").unwrap();
        let v = json!({
            "goal": "Build a feature",
            "tasks": [
                {
                    "class": "serial",
                    "done_criteria": "done"
                }
            ]
        });
        let violations = validate(&v, &schema.fields, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path.contains("id") && vi.problem.contains("required")),
            "expected id-missing violation, got {:?}",
            violations
        );
    }

    #[test]
    fn decomposition_task_with_only_id_is_valid() {
        // `title`/`class`/`done_criteria` are optional per-task fields —
        // condukt's `model::Task` fills them with defaults when absent, so
        // the schema must accept a task with only `id` present.
        let schema = get("decomposition").unwrap();
        let v = json!({
            "goal": "Build a feature",
            "tasks": [{"id": "t1"}]
        });
        let violations = validate(&v, &schema.fields, "");
        assert!(violations.is_empty(), "got {:?}", violations);
    }

    #[test]
    fn decomposition_invalid_class_enum() {
        let schema = get("decomposition").unwrap();
        let v = json!({
            "goal": "Build a feature",
            "tasks": [
                {
                    "id": "t1",
                    "title": "Do something",
                    "class": "bogus",
                    "done_criteria": "done"
                }
            ]
        });
        let violations = validate(&v, &schema.fields, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path.contains("class") && vi.problem.contains("not in")),
            "expected class enum violation, got {:?}",
            violations
        );
    }

    // ── episode ────────────────────────────────────────────────────────────

    #[test]
    fn episode_valid() {
        let schema = get("episode").unwrap();
        let v = json!({
            "title": "My session",
            "model": "claude-sonnet",
            "pass": true
        });
        let violations = validate(&v, &schema.fields, "");
        assert!(violations.is_empty(), "got {:?}", violations);
    }

    #[test]
    fn episode_missing_pass() {
        let schema = get("episode").unwrap();
        let v = json!({
            "title": "My session",
            "model": "claude-sonnet"
        });
        let violations = validate(&v, &schema.fields, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path == "pass" && vi.problem.contains("required")),
            "got {:?}",
            violations
        );
    }

    // ── playbook ───────────────────────────────────────────────────────────

    #[test]
    fn playbook_valid() {
        let schema = get("playbook").unwrap();
        let v = json!({"title": "My playbook"});
        let violations = validate(&v, &schema.fields, "");
        assert!(violations.is_empty(), "got {:?}", violations);
    }

    #[test]
    fn playbook_missing_title() {
        let schema = get("playbook").unwrap();
        let v = json!({"done_criteria": "all tests pass"});
        let violations = validate(&v, &schema.fields, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path == "title" && vi.problem.contains("required")),
            "got {:?}",
            violations
        );
    }

    // ── scout-measure ──────────────────────────────────────────────────────

    #[test]
    fn scout_measure_valid() {
        let schema = get("scout-measure").unwrap();
        let v = json!({
            "title": "DB perf",
            "lens": "L2",
            "severity": "high",
            "effort": "m",
            "evidence": "slow query log shows P99 > 500ms"
        });
        let violations = validate(&v, &schema.fields, "");
        assert!(violations.is_empty(), "got {:?}", violations);
    }

    #[test]
    fn scout_measure_bad_lens() {
        let schema = get("scout-measure").unwrap();
        let v = json!({
            "title": "DB perf",
            "lens": "L9",
            "severity": "high",
            "effort": "m",
            "evidence": "something"
        });
        let violations = validate(&v, &schema.fields, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path == "lens" && vi.problem.contains("not in")),
            "got {:?}",
            violations
        );
    }

    // ── verdict ────────────────────────────────────────────────────────────

    #[test]
    fn verdict_valid() {
        let schema = get("verdict").unwrap();
        let v = json!({
            "candidate": "worker-a",
            "pass": true,
            "group": "g1"
        });
        let violations = validate(&v, &schema.fields, "");
        assert!(violations.is_empty(), "got {:?}", violations);
    }

    #[test]
    fn verdict_missing_pass() {
        let schema = get("verdict").unwrap();
        let v = json!({"candidate": "worker-a"});
        let violations = validate(&v, &schema.fields, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path == "pass" && vi.problem.contains("required")),
            "got {:?}",
            violations
        );
    }

    #[test]
    fn verdict_wrong_typed_pass() {
        let schema = get("verdict").unwrap();
        let v = json!({"candidate": "worker-a", "pass": "yes"});
        let violations = validate(&v, &schema.fields, "");
        assert!(
            violations
                .iter()
                .any(|vi| vi.path == "pass" && vi.problem.contains("expected bool")),
            "got {:?}",
            violations
        );
    }

    #[test]
    fn verdict_group_may_be_absent() {
        let schema = get("verdict").unwrap();
        let v = json!({"candidate": "worker-a", "pass": false});
        let violations = validate(&v, &schema.fields, "");
        assert!(violations.is_empty(), "got {:?}", violations);
    }

    // ── names() / get() ────────────────────────────────────────────────────

    #[test]
    fn names_returns_five_schemas() {
        assert_eq!(names().len(), 5);
    }

    #[test]
    fn unknown_name_returns_none() {
        assert!(get("nonexistent_xyz").is_none());
    }

    #[test]
    fn all_names_are_gettable() {
        for name in names() {
            assert!(
                get(name).is_some(),
                "schema '{}' listed but not gettable",
                name
            );
        }
    }
}
