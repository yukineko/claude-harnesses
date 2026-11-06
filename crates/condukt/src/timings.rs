//! `condukt state timings` rendering (moved verbatim out of `main.rs`).

use crate::state;
use anyhow::Result;

/// Wall-clock seconds spanning `[a, b]`, or `None` when the span is unmeasured.
/// Unmeasured means EITHER endpoint was never stamped (`None`) OR the endpoints
/// are out of order (`b < a`, a cannot-determine that resolves to unmeasured,
/// never a negative or a fabricated 0). A genuine same-instant phase (`b == a`)
/// is a real, measured `Some(0)` — completed-instantly is distinct from
/// never-measured, which is the whole point of the tri-state.
fn phase_span_secs(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) if b >= a => Some(b - a),
        _ => None,
    }
}

/// Render an optional measured span for the text table: `"{n}s"` when measured
/// (including a real `0s`), the explicit marker `"unmeasured"` when `None`.
fn fmt_span(secs: Option<i64>) -> String {
    match secs {
        Some(s) => format!("{s}s"),
        None => "unmeasured".to_string(),
    }
}

/// Print each task's per-phase timing breakdown. An unmeasured phase is rendered
/// as an explicit `unmeasured` marker (text) or `null` (JSON), NEVER as 0
/// seconds — so a never-measured phase and a completed-instantly phase (a real
/// measured `0`) stay distinguishable, the same tri-state norm as the fugu-router
/// duration field.
pub fn print_timings(rs: &state::RunState, json: bool) -> Result<()> {
    if json {
        let tasks: Vec<serde_json::Value> = rs
            .tasks
            .iter()
            .map(|t| {
                let worker = phase_span_secs(t.worker_started_at, t.worker_ended_at);
                let verifier = phase_span_secs(t.verifier_started_at, t.verifier_ended_at);
                let verify_to_merge = phase_span_secs(t.verifier_ended_at, t.merge_completed_at);
                serde_json::json!({
                    "id": t.id,
                    "worker_started_at": t.worker_started_at,
                    "worker_ended_at": t.worker_ended_at,
                    "verifier_started_at": t.verifier_started_at,
                    "verifier_ended_at": t.verifier_ended_at,
                    "merge_completed_at": t.merge_completed_at,
                    // Derived spans: null (not 0) when unmeasured.
                    "worker_secs": worker,
                    "verifier_secs": verifier,
                    "verify_to_merge_secs": verify_to_merge,
                })
            })
            .collect();
        let out = serde_json::json!({ "run": rs.run_id, "tasks": tasks });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("run '{}' phase timings:", rs.run_id);
        for t in &rs.tasks {
            let worker = fmt_span(phase_span_secs(t.worker_started_at, t.worker_ended_at));
            let verifier = fmt_span(phase_span_secs(t.verifier_started_at, t.verifier_ended_at));
            let verify_to_merge =
                fmt_span(phase_span_secs(t.verifier_ended_at, t.merge_completed_at));
            println!(
                "  {}  worker: {}  verifier: {}  verify→merge: {}",
                t.id, worker, verifier, verify_to_merge
            );
        }
    }
    Ok(())
}
