//! Minimal mirror of condukt's decomposition schema — enough to read the task
//! list, rewrite `suggested_model`, and round-trip the JSON back to condukt.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decomposition {
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub tasks: Vec<Task>,
    /// Any other top-level fields condukt emits that this router doesn't model
    /// (e.g. `linked_hypotheses`). Captured verbatim so the round-trip back to
    /// condukt is lossless instead of silently dropping them.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub touched_files: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub suggested_model: String,
    #[serde(default)]
    pub done_criteria: String,
    /// condukt-side task fields this router only passes through, never inspects
    /// (`kind`, `reproduction_tests`, `size`, `target_symbols`, `confidence`,
    /// and any future additions). Captured with `flatten` so routing preserves
    /// them — dropping `kind`/`reproduction_tests` would silently disable
    /// condukt's Fail→Pass oracle gate (it degrades to `required:false`).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (F→P): `fugu-router route` deserializes a condukt
    /// decomposition, rewrites `suggested_model`, and serializes it back. Fields
    /// this router does not explicitly model — notably `kind` and
    /// `reproduction_tests` — must survive that round-trip, otherwise condukt's
    /// F→P oracle gate silently degrades to `required:false` (it reads
    /// `task.kind`/`task.reproduction_tests`, which become `None` once stripped).
    /// Before the `#[serde(flatten)] extra` passthrough this test fails because
    /// those keys are dropped on the deserialize→serialize round-trip.
    #[test]
    fn route_preserves_oracle_fields() {
        // Same top-level + task shape condukt's interpreter emits.
        let input = r#"{
            "goal": "g",
            "linked_hypotheses": ["h1"],
            "tasks": [{
                "id": "t1",
                "title": "fix a thing",
                "class": "serial",
                "suggested_model": "sonnet",
                "done_criteria": "dc",
                "kind": "fix",
                "reproduction_tests": "cargo test -p foo -- bar",
                "size": "small",
                "target_symbols": ["foo::bar"],
                "confidence": "high"
            }]
        }"#;

        // Exercise the exact `cmd_route` shape: parse → rewrite suggested_model
        // → serialize back.
        let mut dec: Decomposition = serde_json::from_str(input).unwrap();
        dec.tasks[0].suggested_model = "haiku".to_string();
        let out = serde_json::to_string(&dec).unwrap();

        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let t = &v["tasks"][0];

        // Oracle-critical fields must survive the round-trip.
        assert_eq!(
            t["kind"], "fix",
            "kind must survive route round-trip: {out}"
        );
        assert_eq!(
            t["reproduction_tests"], "cargo test -p foo -- bar",
            "reproduction_tests must survive route round-trip: {out}"
        );
        // Other condukt-side task fields the router doesn't model must survive.
        assert_eq!(t["size"], "small", "size must survive: {out}");
        assert_eq!(
            t["target_symbols"][0], "foo::bar",
            "target_symbols must survive: {out}"
        );
        assert_eq!(t["confidence"], "high", "confidence must survive: {out}");
        // Top-level fields the router doesn't model (e.g. linked_hypotheses) too.
        assert_eq!(
            v["linked_hypotheses"][0], "h1",
            "linked_hypotheses must survive: {out}"
        );
        // The router's own rewrite still takes effect.
        assert_eq!(
            t["suggested_model"], "haiku",
            "suggested_model rewrite must persist: {out}"
        );
    }
}
