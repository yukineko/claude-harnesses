//! `condukt agents check` — the orchestrator's stalled / undelivered worker
//! check, as a deterministic exit code instead of prose judgment.
//!
//! Shells out to `overwatch subagents --session <sid> --stall-secs N --json`
//! (looked up on PATH) and prints `{"clean":bool,"blocking":[...]}`. Exit 0
//! only when overwatch's stdout parsed and every SELECTED agent is `done` or
//! `progressing`. Selection: `--agent` ids plus, with `--run`, the
//! `agent_id` / `verifier_agent_id` recorded in run state; with neither, every
//! agent overwatch reports.
//!
//! Fail-closed (CLAUDE.md section 3): overwatch missing / unspawnable, killed
//! by a signal, an exit status other than 0 or 1, unparseable stdout, a
//! non-null `error` field, a missing `agents` array, a selected id absent from
//! the output (reported as state `undetermined`), `--run` selecting zero ids or
//! an unreadable run state — all exit 1 with `clean:false`. When specific
//! agents are selected an overwatch exit 1 caused by UNSELECTED agents does not
//! block. Inconsistent output is undetermined too: an empty agents array,
//! duplicate agent ids, exit 1 with no non-ok agent anywhere, or a body
//! `clean` that disagrees with the exit code.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use clap::Subcommand;
use serde_json::{json, Value};

use crate::config::Config;
use crate::state::RunState;

#[derive(Subcommand)]
pub enum AgentsAction {
    /// Classify the session's recorded sub-agents via `overwatch subagents`
    /// and exit non-zero unless every selected one is done or progressing.
    Check {
        /// Session id whose recorded agents are checked.
        #[arg(long)]
        session: String,
        /// Select the agent ids recorded in this run's state (`agent_id` and
        /// `verifier_agent_id` of every task). Zero recorded ids is not clean.
        #[arg(long)]
        run: Option<String>,
        /// Select this agent id (repeatable). Absent from overwatch's output
        /// means `undetermined`, not clean.
        #[arg(long)]
        agent: Vec<String>,
        /// Stall window in seconds, passed through to overwatch.
        #[arg(long, default_value_t = 600)]
        stall_secs: u64,
    },
}

/// Pure verdict over overwatch's captured result. `Err` = undeterminable.
fn evaluate(
    exit: Option<i32>,
    stdout: &[u8],
    selected: Option<&BTreeSet<String>>,
) -> Result<Vec<Value>, String> {
    let code = exit.ok_or("overwatch was terminated by a signal")?;
    if code != 0 && code != 1 {
        return Err(format!("overwatch exited {code}"));
    }
    let v: Value =
        serde_json::from_slice(stdout).map_err(|e| format!("overwatch stdout is not JSON: {e}"))?;
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        return Err(format!("overwatch reported an error: {err}"));
    }
    let agents = v
        .get("agents")
        .and_then(Value::as_array)
        .ok_or("overwatch output has no `agents` array")?;
    if agents.is_empty() {
        return Err("overwatch listed no agents (an empty set is not clean)".into());
    }
    let mut seen = BTreeSet::new();
    for a in agents {
        let id = a["agent_id"]
            .as_str()
            .ok_or("agent entry without agent_id")?;
        if !seen.insert(id) {
            return Err(format!("duplicate agent_id '{id}' in overwatch output"));
        }
    }
    if let Some(c) = v.get("clean").filter(|c| !c.is_null()) {
        let body_clean = c.as_bool().ok_or("overwatch `clean` is not a boolean")?;
        if body_clean != (code == 0) {
            return Err(format!(
                "overwatch body clean={body_clean} disagrees with exit {code}"
            ));
        }
    }
    let is_ok = |a: &Value| matches!(a["state"].as_str(), Some("done" | "progressing"));
    if code == 1 && agents.iter().all(is_ok) {
        return Err("overwatch exited 1 but no agent is non-ok".into());
    }
    let Some(sel) = selected else {
        return Ok(agents.iter().filter(|a| !is_ok(a)).cloned().collect());
    };
    let mut blocking = Vec::new();
    for id in sel {
        match agents.iter().find(|a| a["agent_id"].as_str() == Some(id)) {
            Some(a) if is_ok(a) => {}
            Some(a) => blocking.push(a.clone()),
            None => blocking.push(json!({"agent_id": id, "state": "undetermined",
                "reason": "agent id absent from overwatch output"})),
        }
    }
    Ok(blocking)
}

fn run_ids(cfg: &Config, cwd: &Path, run: &str) -> Result<BTreeSet<String>, String> {
    let st = RunState::load(cfg, cwd, run).map_err(|e| format!("run state: {e:#}"))?;
    let ids: BTreeSet<String> = st
        .tasks
        .iter()
        .flat_map(|t| [t.agent_id.clone(), t.verifier_agent_id.clone()])
        .flatten()
        .collect();
    if ids.is_empty() {
        return Err(format!("run '{run}' has no recorded agent ids"));
    }
    Ok(ids)
}

fn undetermined(why: &str) -> ! {
    let out = json!({"clean": false, "blocking": [], "error": why});
    println!("{out}");
    std::process::exit(1)
}

pub fn run(cfg: &Config, cwd: &Path, action: AgentsAction) -> anyhow::Result<()> {
    let AgentsAction::Check {
        session,
        run,
        agent,
        stall_secs,
    } = action;
    let mut sel: BTreeSet<String> = agent.into_iter().collect();
    let selecting = run.is_some() || !sel.is_empty();
    if let Some(r) = &run {
        match run_ids(cfg, cwd, r) {
            Ok(ids) => sel.extend(ids),
            Err(why) => undetermined(&why),
        }
    }
    let out = match Command::new("overwatch")
        .args(["subagents", "--session", &session, "--stall-secs"])
        .arg(stall_secs.to_string())
        .arg("--json")
        .output()
    {
        Ok(o) => o,
        Err(e) => undetermined(&format!("cannot run overwatch: {e}")),
    };
    match evaluate(out.status.code(), &out.stdout, selecting.then_some(&sel)) {
        Ok(blocking) => {
            let clean = blocking.is_empty();
            println!("{}", json!({"clean": clean, "blocking": blocking}));
            if !clean {
                std::process::exit(1);
            }
            Ok(())
        }
        Err(why) => undetermined(&why),
    }
}
