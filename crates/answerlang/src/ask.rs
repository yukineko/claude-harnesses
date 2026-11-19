//! PreToolUse hook on `AskUserQuestion` (`answerlang ask`): deny a question
//! whose prose (every `question` / `header` and every option `label` /
//! `description`; `preview` is excluded as code/mock-up) is not written in the
//! configured answer language, so the model rewrites it before the user sees
//! the dialog. Measured exactly like the Stop gate (`measure::judge`).
//!
//! Fail-closed: an unreadable stdin, an unparseable payload, a missing or
//! malformed `tool_input`, a prompt-language record that cannot be read in
//! `auto` mode, or a panic all DENY. The operator escape is
//! `answerlang set --enforce off`. A PreToolUse deny does not loop the way a
//! Stop block does (the model just issues a new tool call), so no re-entry
//! marker is kept.
//!
//! Not judged (allowed silently, with a `systemMessage` where the user would
//! otherwise read silence as "checked"): `enabled`/`enforce` off, no config
//! file, a language with no measurable script, an undetermined target, and a
//! payload whose `tool_name` is not `AskUserQuestion` (the hooks.json matcher
//! never routes one here; it is a misconfiguration, noted on stderr).

use serde_json::{json, Value};

use harness_core::hook::HookInput;

use crate::config;
use crate::detect::{self, Target};
use crate::lang::Lang;
use crate::measure::{self, Outcome};
use crate::state::Session;

const TOOL: &str = "AskUserQuestion";

fn emit_deny(reason: &str) -> ! {
    println!(
        "{}",
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason
            }
        })
    );
    std::process::exit(0)
}

fn emit_message(msg: &str) -> ! {
    println!("{}", json!({ "systemMessage": msg }));
    std::process::exit(0)
}

fn undetermined(why: &str) -> ! {
    emit_deny(&format!(
        "answerlang: the question-language check could not run ({why}), so this AskUserQuestion is denied. Re-issue it with a well-formed question written in the user's language."
    ))
}

/// Entry point. A panic inside is a deny (fail-closed), never a silent pass.
pub fn ask() -> ! {
    let r = std::panic::catch_unwind(ask_body);
    match r {
        Ok(()) => std::process::exit(0),
        Err(_) => undetermined("internal error (panic)"),
    }
}

/// Collect the prose to judge. `Err` = the payload shape is undeterminable.
pub(crate) fn question_text(tool_input: Option<&Value>) -> Result<String, String> {
    let Some(input) = tool_input else {
        return Err("tool_input is missing".into());
    };
    let Some(questions) = input.get("questions") else {
        return Err("tool_input.questions is missing".into());
    };
    let Some(questions) = questions.as_array() else {
        return Err("tool_input.questions is not an array".into());
    };
    let mut parts: Vec<String> = Vec::new();
    let field = |obj: &Value, key: &str, at: &str, out: &mut Vec<String>| -> Result<(), String> {
        match obj.get(key) {
            None | Some(Value::Null) => Ok(()),
            Some(Value::String(s)) => {
                out.push(s.clone());
                Ok(())
            }
            Some(_) => Err(format!("{at}.{key} is not a string")),
        }
    };
    for (i, q) in questions.iter().enumerate() {
        let at = format!("questions[{i}]");
        if !q.is_object() {
            return Err(format!("{at} is not an object"));
        }
        field(q, "question", &at, &mut parts)?;
        field(q, "header", &at, &mut parts)?;
        match q.get("options") {
            None | Some(Value::Null) => {}
            Some(Value::Array(opts)) => {
                for (j, o) in opts.iter().enumerate() {
                    let oat = format!("{at}.options[{j}]");
                    if !o.is_object() {
                        return Err(format!("{oat} is not an object"));
                    }
                    field(o, "label", &oat, &mut parts)?;
                    field(o, "description", &oat, &mut parts)?;
                }
            }
            Some(_) => return Err(format!("{at}.options is not an array")),
        }
    }
    Ok(parts.join("\n"))
}

fn ask_body() {
    let raw = match harness_core::hook::try_read_stdin() {
        Ok(s) => s,
        Err(e) => undetermined(&format!("stdin could not be read: {e}")),
    };
    let loaded = config::load();
    let cfg = &loaded.config;
    if !cfg.enabled || !cfg.enforce {
        return;
    }
    if !loaded.present {
        emit_message(&format!(
            "answerlang: no answer language configured ({} absent), so the AskUserQuestion language check did not run.",
            loaded.path.display()
        ))
    }
    let Some(hook) = HookInput::parse(&raw) else {
        undetermined("the PreToolUse payload could not be parsed")
    };
    if hook.tool_name != TOOL {
        eprintln!(
            "answerlang ask: tool_name {:?} is not {TOOL}; not judged (check the hooks.json matcher)",
            hook.tool_name
        );
        return;
    }
    let session = Session::resolve(Some(&hook));
    let prompt_lang = if cfg.language == "auto" {
        match session.as_ref().map(detect::recorded_prompt) {
            Some(Ok(l)) => l,
            Some(Err(e)) => {
                undetermined(&format!("the target language could not be determined: {e}"))
            }
            None => None,
        }
    } else {
        None
    };
    let (target, _) = detect::resolve(&cfg.language, prompt_lang, session.as_ref());
    let lang: Lang = match target {
        Target::Known(l) => l,
        Target::Unknown(name) => emit_message(&format!(
            "answerlang: enforcement is unavailable for language {name:?} (no known script to measure); the AskUserQuestion language check did not run."
        )),
        Target::Undetermined => emit_message(
            "answerlang: the target language is undetermined, so the AskUserQuestion language check did not run. Set it with `answerlang set --language <name>`.",
        ),
    };
    let text = match question_text(hook.tool_input.as_ref()) {
        Ok(t) => t,
        Err(e) => undetermined(&e),
    };
    match measure::judge(&text, lang, cfg.min_share) {
        Outcome::TooShort(_) | Outcome::Clean(_) => {}
        Outcome::Violation { counts, share } => {
            let name = lang.name();
            emit_deny(&format!(
                "answerlang: this AskUserQuestion is not written in {name}: only {pct:.0}% of the {letters} prose letters in its questions, headers and option labels/descriptions are in {name} script (minimum {min:.0}%). Rewrite the questions and options in {name} and ask again. Keep code, identifiers and verbatim quotes in their original form.",
                pct = share * 100.0,
                letters = counts.letters,
                min = cfg.min_share * 100.0,
            ))
        }
    }
}
