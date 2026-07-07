//! Data model shared across subcommands: the task decomposition the LLM produces,
//! the schedule the engine computes, and the generic hook-input envelope.

use serde::{Deserialize, Serialize};

/// How a task may be executed. The LLM classifies; the engine enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    /// Independent, parallel-eligible (subject to file-conflict analysis).
    #[default]
    Parallel,
    /// Must run alone on the main line (shared files / design decisions).
    Serial,
    /// Requires an approval gate (deploy, shared infra). Never auto-run.
    Gated,
    /// A reversible spike/probe whose value is learning, not a deliverable.
    /// Scheduled on its own track and never placed on the auto-merge path.
    Experiment,
}

/// A declared, deterministic verification check: a shell command plus the
/// observable condition that means it passed. Lets the verifier run a machine
/// oracle instead of judging `done_criteria` prose by eye. All fields permissive;
/// a Task with no `checks` (the default) behaves exactly as before.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Check {
    /// Shell command to run (via `sh -c`) from the task's cwd/worktree.
    pub cmd: String,
    /// Expected exit code. `None` means "expect 0".
    #[serde(default)]
    pub expect_exit: Option<i64>,
    /// If set, the combined stdout+stderr must contain this substring.
    #[serde(default)]
    pub expect_substring: Option<String>,
}

/// One unit of work in a decomposition.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// Files (or globs) the task is expected to touch. Drives conflict analysis.
    #[serde(default)]
    pub touched_files: Vec<String>,
    /// Ids of tasks that must complete before this one.
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub class: Class,
    #[serde(default)]
    pub suggested_model: Option<String>,
    #[serde(default)]
    pub done_criteria: Option<String>,
    /// Optional size hint (xs|s|m|l|xl) for downstream tools. Free-form and
    /// permissive: unknown or missing values are accepted and ignored here.
    #[serde(default)]
    pub size: Option<String>,
    /// Symbols (functions/classes) the task is expected to edit — finer than
    /// `touched_files`. The engine does not act on these; carried through so the
    /// skill can forward them to a worker without losing them across `state init`.
    #[serde(default)]
    pub target_symbols: Vec<String>,
    /// A command that reproduces/validates the task's outcome (the TDD anchor).
    /// Like `size`, the engine treats this as a permissive passthrough.
    #[serde(default)]
    pub reproduction_tests: Option<String>,
    /// Self-assessed confidence the task is well-scoped and completable (high|medium|low).
    /// The engine carries this through; SKILL.md uses it to gate clarification and
    /// re-verification. Unknown values are accepted and ignored.
    #[serde(default)]
    pub confidence: Option<String>,
    /// Classification of the change (fix|feature|chore|...). Free-form and
    /// permissive: unknown or missing values are accepted and ignored here.
    /// Used to decide whether a fix/feature transition requires an FP oracle.
    #[serde(default)]
    pub kind: Option<String>,
    /// Declared deterministic verification checks (see [`Check`]). Empty by
    /// default; when present, the verifier can run them as a machine oracle
    /// instead of eyeballing `done_criteria`. Engine passthrough — no scheduling
    /// behavior changes.
    #[serde(default)]
    pub checks: Vec<Check>,
    /// Expected tool-call trajectory the worker should follow (the shape
    /// `trajectoryeval` consumes: `{mode: "strict"|"unordered"|"subsequence",
    /// steps: [{tool: "<ToolName>"}]}`). Free-form `serde_json::Value` and fully
    /// permissive: the engine never validates or acts on its shape, only carries
    /// it through so SKILL.md's Phase 6 can feed it to `trajectoryeval check`
    /// alongside the extracted actual trajectory. Absent by default — existing
    /// decompositions without it are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_trajectory: Option<serde_json::Value>,
}

impl Task {
    /// True only when `kind` is present and names a fix or feature (case-insensitive).
    /// Chore/other/absent classifications return false.
    pub fn requires_fp_oracle(&self) -> bool {
        matches!(
            self.kind.as_deref().map(str::to_ascii_lowercase).as_deref(),
            Some("fix") | Some("feature")
        )
    }
}

/// The full plan the interpreter agent emits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decomposition {
    #[serde(default)]
    pub goal: String,
    pub tasks: Vec<Task>,
}

/// A set of task ids with no pairwise file conflict — safe to run concurrently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    pub parallel: Vec<String>,
}

/// The deterministic schedule: ordered parallel batches plus the serial/gated lists.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Schedule {
    /// Parallel-eligible work, in dependency order. Each batch runs after the
    /// previous one's tasks are done; tasks within a batch run concurrently.
    pub batches: Vec<Batch>,
    /// Tasks forced onto the main line (class=serial or touching a shared glob),
    /// in dependency order.
    pub serial: Vec<String>,
    /// Tasks that require an approval gate; never scheduled for auto-run.
    pub gated: Vec<String>,
    /// Experiment/spike tasks: reversible probes scheduled on their own track
    /// and never placed on the auto-merge path (batches/serial).
    pub experiment: Vec<String>,
    /// Non-fatal notes (e.g. "task X touches a shared path -> serial").
    pub warnings: Vec<String>,
}

/// Generic hook-input envelope. Every Claude Code hook event posts a JSON object
/// on stdin; all fields are optional so one struct absorbs every event. Most
/// fields are unused today but kept to document the envelope and for future hooks.
#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
pub struct HookInput {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub hook_event_name: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

impl HookInput {
    /// Parse hook stdin; any malformed input yields defaults (never panics).
    pub fn parse(raw: &str) -> Self {
        serde_json::from_str(raw).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_without_size_still_parses() {
        // Back-compat: decompositions emitted before `size` existed must load.
        let dec: Decomposition = serde_json::from_str(
            r#"{"goal":"g","tasks":[{"id":"a","touched_files":["src/a.rs"]}]}"#,
        )
        .expect("decomposition without size should parse");
        assert_eq!(dec.tasks.len(), 1);
        assert_eq!(dec.tasks[0].size, None);
    }

    #[test]
    fn task_with_size_is_populated() {
        let dec: Decomposition =
            serde_json::from_str(r#"{"goal":"g","tasks":[{"id":"a","size":"m"}]}"#)
                .expect("decomposition with size should parse");
        assert_eq!(dec.tasks[0].size.as_deref(), Some("m"));
    }

    #[test]
    fn task_without_agentic_fields_defaults_empty() {
        // Back-compat: decompositions emitted before the agentic fields existed.
        let dec: Decomposition =
            serde_json::from_str(r#"{"goal":"g","tasks":[{"id":"a"}]}"#).unwrap();
        assert!(dec.tasks[0].target_symbols.is_empty());
        assert_eq!(dec.tasks[0].reproduction_tests, None);
    }

    #[test]
    fn task_carries_target_symbols_and_reproduction_tests() {
        let dec: Decomposition = serde_json::from_str(
            r#"{"goal":"g","tasks":[{"id":"a","target_symbols":["foo","Bar"],"reproduction_tests":"cargo test -p x"}]}"#,
        )
        .expect("decomposition with agentic fields should parse");
        assert_eq!(dec.tasks[0].target_symbols, vec!["foo", "Bar"]);
        assert_eq!(
            dec.tasks[0].reproduction_tests.as_deref(),
            Some("cargo test -p x")
        );
    }

    #[test]
    fn task_without_confidence_defaults_none() {
        // Back-compat: decompositions emitted before `confidence` existed must load.
        let dec: Decomposition =
            serde_json::from_str(r#"{"goal":"g","tasks":[{"id":"a"}]}"#).unwrap();
        assert_eq!(dec.tasks[0].confidence, None);
    }

    #[test]
    fn task_with_confidence_is_populated() {
        let dec: Decomposition =
            serde_json::from_str(r#"{"goal":"g","tasks":[{"id":"a","confidence":"low"}]}"#)
                .expect("decomposition with confidence should parse");
        assert_eq!(dec.tasks[0].confidence.as_deref(), Some("low"));
    }

    #[test]
    fn task_without_kind_still_parses() {
        // Back-compat: decompositions emitted before `kind` existed must load.
        let dec: Decomposition = serde_json::from_str(
            r#"{"goal":"g","tasks":[{"id":"a","touched_files":["src/a.rs"]}]}"#,
        )
        .expect("decomposition without kind should parse");
        assert_eq!(dec.tasks.len(), 1);
        assert_eq!(dec.tasks[0].kind, None);
    }

    #[test]
    fn task_with_kind_is_populated() {
        let dec: Decomposition =
            serde_json::from_str(r#"{"goal":"g","tasks":[{"id":"a","kind":"fix"}]}"#)
                .expect("decomposition with kind should parse");
        assert_eq!(dec.tasks[0].kind.as_deref(), Some("fix"));
    }

    #[test]
    fn requires_fp_oracle_only_for_fix_or_feature() {
        let fix = Task {
            kind: Some("fix".to_string()),
            ..Default::default()
        };
        assert!(fix.requires_fp_oracle());

        let feature = Task {
            kind: Some("FEATURE".to_string()),
            ..Default::default()
        };
        assert!(feature.requires_fp_oracle());

        let chore = Task {
            kind: Some("chore".to_string()),
            ..Default::default()
        };
        assert!(!chore.requires_fp_oracle());

        let none = Task {
            kind: None,
            ..Default::default()
        };
        assert!(!none.requires_fp_oracle());
    }

    #[test]
    fn task_with_experiment_class_parses() {
        let dec: Decomposition =
            serde_json::from_str(r#"{"goal":"g","tasks":[{"id":"x","class":"experiment"}]}"#)
                .expect("decomposition with experiment class should parse");
        assert_eq!(dec.tasks[0].class, Class::Experiment);
    }

    #[test]
    fn task_without_checks_defaults_empty() {
        // Back-compat: decompositions emitted before `checks` existed must load
        // and yield an empty vec.
        let dec: Decomposition = serde_json::from_str(
            r#"{"goal":"g","tasks":[{"id":"a","touched_files":["src/a.rs"]}]}"#,
        )
        .expect("decomposition without checks should parse");
        assert_eq!(dec.tasks.len(), 1);
        assert!(dec.tasks[0].checks.is_empty());
    }

    #[test]
    fn task_with_checks_round_trips() {
        let dec: Decomposition = serde_json::from_str(
            r#"{"goal":"g","tasks":[{"id":"a","checks":[{"cmd":"cargo test -p x","expect_exit":0,"expect_substring":"ok"}]}]}"#,
        )
        .expect("decomposition with checks should parse");
        assert_eq!(dec.tasks[0].checks.len(), 1);
        let check = &dec.tasks[0].checks[0];
        assert_eq!(check.cmd, "cargo test -p x");
        assert_eq!(check.expect_exit, Some(0));
        assert_eq!(check.expect_substring.as_deref(), Some("ok"));

        // Round-trip: serialize then deserialize and compare structurally.
        let json = serde_json::to_string(check).expect("check should serialize");
        let back: Check = serde_json::from_str(&json).expect("check should deserialize");
        assert_eq!(&back, check);
    }

    #[test]
    fn task_without_expected_trajectory_defaults_none() {
        // Back-compat: decompositions emitted before `expected_trajectory` existed
        // must load, and the field defaults to None.
        let dec: Decomposition = serde_json::from_str(
            r#"{"goal":"g","tasks":[{"id":"a","touched_files":["src/a.rs"]}]}"#,
        )
        .expect("decomposition without expected_trajectory should parse");
        assert_eq!(dec.tasks.len(), 1);
        assert_eq!(dec.tasks[0].expected_trajectory, None);
    }

    #[test]
    fn task_with_expected_trajectory_round_trips() {
        let dec: Decomposition = serde_json::from_str(
            r#"{"goal":"g","tasks":[{"id":"a","expected_trajectory":{"mode":"strict","steps":[{"tool":"Read"}]}}]}"#,
        )
        .expect("decomposition with expected_trajectory should parse");
        let traj = dec.tasks[0]
            .expected_trajectory
            .as_ref()
            .expect("expected_trajectory should be Some");
        assert_eq!(
            traj,
            &serde_json::json!({"mode":"strict","steps":[{"tool":"Read"}]})
        );

        // Round-trip: serialize then deserialize and compare structurally.
        let json = serde_json::to_string(&dec.tasks[0]).expect("task should serialize");
        let back: Task = serde_json::from_str(&json).expect("task should deserialize");
        assert_eq!(back.expected_trajectory, dec.tasks[0].expected_trajectory);
    }
}
