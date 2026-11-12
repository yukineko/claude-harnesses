//! Hook entry points: SessionStart / UserPromptSubmit (layer 2, inject) and
//! Stop (layer 3, enforce).

use serde_json::json;

use harness_core::hook::HookInput;

use crate::config;
use crate::detect::{self, Target};
use crate::lang::Lang;
use crate::measure::{self, Outcome};
use crate::state::{Session, BLOCK};
use crate::transcript;

pub const DIRECTIVE: &str = "answerlang: no language configured. Ask the user once with AskUserQuestion which language answers should use; offer the auto-detected language as the recommended option, then run `answerlang set --language <choice>`.";

pub fn language_line(lang: &str) -> String {
    format!(
        "Answers and conclusions: write them in {lang}. Keep code, identifiers, commit messages and verbatim quotes (gate messages, test output) in their original form."
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InjectEvent {
    SessionStart,
    UserPromptSubmit,
}

impl InjectEvent {
    fn name(self) -> &'static str {
        match self {
            InjectEvent::SessionStart => "SessionStart",
            InjectEvent::UserPromptSubmit => "UserPromptSubmit",
        }
    }
}

/// Say, visibly, that injection did not run — silence would read as "nothing
/// to inject".
fn inject_not_run(event: InjectEvent, why: &str) -> ! {
    let msg = format!("answerlang: language injection did NOT run ({why}).");
    eprintln!("{msg}");
    println!(
        "{}",
        json!({
            "systemMessage": msg,
            "hookSpecificOutput": {"hookEventName": event.name(), "additionalContext": msg}
        })
    );
    std::process::exit(0)
}

/// Layer 2. A panic inside is reported as "injection did NOT run" (never as
/// silent success).
pub fn inject(event: InjectEvent) -> ! {
    let r = std::panic::catch_unwind(|| inject_body(event));
    match r {
        Ok(()) => std::process::exit(0),
        Err(_) => inject_not_run(event, "internal error"),
    }
}

fn inject_body(event: InjectEvent) {
    let raw = match harness_core::hook::try_read_stdin() {
        Ok(s) => s,
        Err(e) => inject_not_run(event, &format!("stdin could not be read: {e}")),
    };
    let hook = HookInput::parse(&raw);
    if hook.is_none() {
        eprintln!(
            "answerlang: {} payload could not be parsed; prompt-based detection skipped",
            event.name()
        );
    }
    let loaded = config::load();
    let cfg = &loaded.config;
    if !cfg.enabled {
        return;
    }
    let session = Session::resolve(hook.as_ref());
    let auto = cfg.language == "auto";

    // Step 1 of auto detection.
    let prompt_lang = match (event, hook.as_ref()) {
        (InjectEvent::UserPromptSubmit, Some(h)) => {
            let d = crate::lang::detect_prompt(&h.prompt);
            match &session {
                Some(s) => {
                    if let Err(e) = detect::record_prompt(s, d) {
                        eprintln!(
                            "answerlang: prompt language not recorded ({e}); Stop will not see it"
                        );
                    }
                }
                None => eprintln!("answerlang: no usable session id; prompt language not recorded"),
            }
            d
        }
        _ => match &session {
            Some(s) if auto => match detect::recorded_prompt(s) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("answerlang: {e}");
                    None
                }
            },
            _ => None,
        },
    };

    if loaded.present && !cfg.inject {
        return;
    }
    let (target, _) = detect::resolve(&cfg.language, prompt_lang, session.as_ref());
    let mut parts: Vec<String> = Vec::new();
    if !loaded.present {
        parts.push(DIRECTIVE.to_string());
    }
    match &target {
        Target::Known(l) => parts.push(language_line(l.name())),
        Target::Unknown(name) => parts.push(language_line(name)),
        Target::Undetermined => {
            eprintln!("answerlang: target language undetermined (prompt, macOS UI language and LC_ALL/LC_MESSAGES/LANG gave nothing); no language line injected");
        }
    }
    if parts.is_empty() {
        return;
    }
    println!(
        "{}",
        json!({
            "hookSpecificOutput": {
                "hookEventName": event.name(),
                "additionalContext": parts.join("\n")
            }
        })
    );
}

// ---------------------------------------------------------------- Stop ----

fn emit_message(msg: &str) -> ! {
    println!("{}", json!({ "systemMessage": msg }));
    std::process::exit(0)
}

fn emit_block(reason: &str) -> ! {
    println!("{}", json!({ "decision": "block", "reason": reason }));
    std::process::exit(0)
}

fn clear_marker(session: Option<&Session>) {
    if let Some(s) = session {
        if let Err(e) = s.remove(BLOCK) {
            eprintln!("answerlang: block marker not cleared ({e})");
        }
    }
}

/// The bounded block shared by violations and undetermined checks.
///
/// * `active == Some(false)` (first stop of a chain): record the marker, block.
/// * `active == Some(true)` (re-entry): marker present → clear it and allow
///   with a `systemMessage` ("failed twice"); marker absent (another gate's
///   block set `stop_hook_active`) → record it and block; marker unreadable,
///   not recordable, or no usable session id → allow with an explicit
///   `systemMessage` (a block could never be bounded).
/// * `active == None` (payload unparseable, so `stop_hook_active` — the only
///   evidence of a re-entry — is unknown): always block, and record the marker
///   so that a following PARSEABLE re-entry can take the bounded allow. This
///   is unbounded only while every payload stays unparseable (restrictive
///   side, CLAUDE.md §3).
fn bounded_block(session: Option<&Session>, active: Option<bool>, reason: &str, detail: &str) -> ! {
    let twice = |extra: &str| -> String {
        format!("answerlang: the answer-language check failed twice in a row ({detail}); this stop is allowed without a passing check.{extra}")
    };
    match active {
        Some(false) => {
            if let Some(s) = session {
                if let Err(e) = s.write(BLOCK, "blocked") {
                    eprintln!("answerlang: block marker not recorded ({e})");
                }
            }
            emit_block(reason)
        }
        Some(true) => {
            let Some(s) = session else {
                emit_message(&format!(
                    "answerlang: the answer-language check did not pass ({detail}) and there is no usable session id to bound a re-block, so this stop is allowed WITHOUT a passing check."
                ))
            };
            match s.read(BLOCK) {
                Ok(Some(_)) => {
                    let extra = match s.remove(BLOCK) {
                        Ok(()) => String::new(),
                        Err(e) => format!(" (block marker could not be cleared: {e})"),
                    };
                    emit_message(&twice(&extra))
                }
                Ok(None) => match s.write(BLOCK, "blocked") {
                    Ok(()) => emit_block(reason),
                    Err(e) => emit_message(&format!(
                        "answerlang: the answer-language check did not pass ({detail}), but the block marker could not be recorded ({e}), so a re-block could never be bounded; this stop is allowed WITHOUT a passing check."
                    )),
                },
                Err(e) => emit_message(&format!(
                    "answerlang: the answer-language check did not pass ({detail}) and the block marker could not be read ({e}); this stop is allowed WITHOUT a passing check."
                )),
            }
        }
        None => {
            if let Some(s) = session {
                if let Err(e) = s.write(BLOCK, "blocked") {
                    eprintln!("answerlang: block marker not recorded ({e})");
                }
            }
            emit_block(reason)
        }
    }
}

/// Layer 3 entry: read stdin (a read error goes through harness-core's
/// fail-closed stdin policy), then run the check under `run_guarded` so a
/// panic blocks (bounded) instead of passing silently.
pub fn stop() -> ! {
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
    let raw = harness_core::gate::run::read_stop_payload_guarded(
        "answerlang",
        interactive,
        harness_core::hook::try_read_stdin(),
    );
    let hook = HookInput::parse(&raw);
    let stop_hook_active = hook.as_ref().is_some_and(|h| h.stop_hook_active);
    harness_core::gate::run::run_guarded("answerlang", interactive, stop_hook_active, move || {
        stop_body(hook)
    });
    std::process::exit(0)
}

fn stop_body(hook: Option<HookInput>) -> ! {
    // Operator escapes first (panic-free), per run_guarded's caller contract.
    let loaded = config::load();
    let cfg = &loaded.config;
    if !cfg.enabled || !cfg.enforce {
        std::process::exit(0)
    }
    let session = Session::resolve(hook.as_ref());
    if !loaded.present {
        clear_marker(session.as_ref());
        emit_message(&format!(
            "answerlang: no answer language configured ({} absent), so the answer-language check did not run. Run `answerlang set --language <name>` to enable it.",
            loaded.path.display()
        ))
    }
    let Some(input) = hook else {
        bounded_block(
            session.as_ref(),
            None,
            "answerlang: the Stop hook payload could not be parsed, so the answer-language check could not run. Check that your final answer is in the configured language, then stop again.",
            "Stop payload unparseable",
        )
    };
    let active = Some(input.stop_hook_active);

    let prompt_lang = if cfg.language == "auto" {
        match session.as_ref().map(detect::recorded_prompt) {
            Some(Ok(l)) => l,
            Some(Err(e)) => bounded_block(
                session.as_ref(),
                active,
                &format!("answerlang: the target language could not be determined: {e}. Restate your final answer in the user's language, then stop again."),
                &e,
            ),
            None => None,
        }
    } else {
        None
    };
    let (target, _) = detect::resolve(&cfg.language, prompt_lang, session.as_ref());
    let lang: Lang = match target {
        Target::Known(l) => l,
        Target::Unknown(name) => {
            clear_marker(session.as_ref());
            emit_message(&format!(
                "answerlang: enforcement is unavailable for language {name:?} (no known script to measure); the answer-language check did not run."
            ))
        }
        Target::Undetermined => {
            eprintln!("answerlang: target language undetermined (prompt, macOS UI language and LC_ALL/LC_MESSAGES/LANG gave nothing)");
            clear_marker(session.as_ref());
            emit_message("answerlang: the target language is undetermined (no prompt script, macOS UI language or locale determined it), so the answer-language check did not run. Set it with `answerlang set --language <name>`.")
        }
    };

    let text = match transcript::last_assistant_text(&input.transcript_path) {
        Ok(t) => t,
        Err(e) => bounded_block(
            session.as_ref(),
            active,
            &format!("answerlang: the answer-language check could not run: {e}. Make sure your final answer is in {}, then stop again.", lang.name()),
            &e,
        ),
    };
    match measure::judge(&text, lang, cfg.min_share) {
        Outcome::TooShort(_) | Outcome::Clean(_) => {
            clear_marker(session.as_ref());
            std::process::exit(0)
        }
        Outcome::Violation { counts, share } => {
            let name = lang.name();
            let reason = format!(
                "answerlang: your final answer is not in {name}: only {pct:.0}% of its {letters} prose letters are in {name} script (minimum {min:.0}%). Restate your answer and conclusions in {name}. Keep code, identifiers, commit messages and verbatim quotes (gate messages, test output) in their original form.",
                pct = share * 100.0,
                letters = counts.letters,
                min = cfg.min_share * 100.0,
            );
            let detail = format!("final answer not in {name}");
            bounded_block(session.as_ref(), active, &reason, &detail)
        }
    }
}
