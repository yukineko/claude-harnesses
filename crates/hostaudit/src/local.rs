//! Local Claude Code settings audit L1–L3 (canon §6, CONTRACT §9).

use crate::config::Ctx;
use crate::finding::{self, Finding};
use crate::redact;
use harness_core::verdict::Required;
use serde_json::Value;
use std::path::PathBuf;

const BROAD: [&str; 4] = ["Bash", "Bash(*)", "Bash(:*)", "*"];

fn files(ctx: &Ctx) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for base in [ctx.home.join(".claude"), ctx.cwd.join(".claude")] {
        for name in ["settings.json", "settings.local.json"] {
            let p = base.join(name);
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Register every `env` value of a settings object as a secret (canon §8).
fn register_env(v: &Value) {
    if let Some(env) = v["env"].as_object() {
        for val in env.values() {
            match val {
                Value::String(s) => redact::register_secret(s),
                other => redact::register_secret(&other.to_string()),
            }
        }
    }
}

fn evaluate(t: &str, v: &Value) -> Vec<Finding> {
    let mut out = Vec::new();
    // L1
    out.push(match &v["permissions"]["allow"] {
        Value::Null => Finding::clean("L1", t, vec![]),
        Value::Array(a) => {
            let hits: Vec<String> = a
                .iter()
                .filter_map(|e| e.as_str())
                .filter(|e| BROAD.contains(e))
                .map(|e| format!("permissions.allow contains {e:?}"))
                .collect();
            if hits.is_empty() {
                Finding::clean("L1", t, vec![])
            } else {
                Finding::violation(
                    "L1",
                    t,
                    hits,
                    &format!(
                        "Remove the blanket entry from permissions.allow in {t} and allow specific commands instead (e.g. \"Bash(git status:*)\")"
                    ),
                )
            }
        }
        other => Finding::undetermined(
            "L1",
            t,
            vec![],
            &format!("permissions.allow is not an array: {}", other),
        ),
    });
    // L2
    out.push(match &v["permissions"]["defaultMode"] {
        Value::String(m) if m == "bypassPermissions" => Finding::violation(
            "L2",
            t,
            vec!["permissions.defaultMode = \"bypassPermissions\"".into()],
            &format!("Remove permissions.defaultMode from {t} (or set it to \"default\")"),
        ),
        Value::Null | Value::String(_) => Finding::clean("L2", t, vec![]),
        other => Finding::undetermined(
            "L2",
            t,
            vec![],
            &format!("permissions.defaultMode is not a string: {other}"),
        ),
    });
    // L3
    out.push(match &v["skipDangerousModePermissionPrompt"] {
        Value::Bool(true) => Finding::violation(
            "L3",
            t,
            vec!["skipDangerousModePermissionPrompt = true".into()],
            &format!("Remove skipDangerousModePermissionPrompt from {t} (or set it to false)"),
        ),
        _ => Finding::clean("L3", t, vec![]),
    });
    out
}

pub fn audit(ctx: &Ctx) -> Vec<Finding> {
    let mut out = Vec::new();
    for p in files(ctx) {
        let t = p.display().to_string();
        let rules = ["L1", "L2", "L3"];
        match harness_core::boundary::read_to_string(&p).require() {
            Required::Determined(None) => {
                for r in rules {
                    out.push(Finding::not_applicable(
                        r,
                        &t,
                        vec![],
                        "file does not exist",
                    ));
                }
            }
            Required::Blocked(why) => {
                for r in rules {
                    out.push(Finding::undetermined(
                        r,
                        &t,
                        vec![],
                        &format!("unreadable: {why}"),
                    ));
                }
            }
            Required::Determined(Some(text)) => match serde_json::from_str::<Value>(&text) {
                Ok(v) if v.is_object() => {
                    register_env(&v);
                    out.extend(evaluate(&t, &v));
                }
                Ok(_) => {
                    for r in rules {
                        out.push(Finding::undetermined(
                            r,
                            &t,
                            vec![],
                            "settings is not a JSON object",
                        ));
                    }
                }
                Err(e) => {
                    for r in rules {
                        out.push(Finding::undetermined(
                            r,
                            &t,
                            vec![],
                            &format!("invalid JSON: {e}"),
                        ));
                    }
                }
            },
        }
    }
    out
}

/// `hostaudit local [--json]`.
pub fn run_local(ctx: &Ctx, json: bool) -> i32 {
    let f = audit(ctx);
    finding::finish(ctx, &f, json)
}
