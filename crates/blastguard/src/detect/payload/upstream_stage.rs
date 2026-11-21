//! The upstream stage of a line that pipes into a shell or interpreter: is it a
//! literal emitter (`echo` / `printf`, possibly behind an exec wrapper), and
//! when a wrapper is in front of it, does the line resolve the wrapper as
//! written. Split out of `payload.rs` for the file-growth cap.

use super::super::{is_exec_wrapper, normalized_command, wrapped_command_heads, Decision};

/// True when `stage` emits only text spelled in its own arguments: its
/// command, behind any exec-wrapper chain, is `echo` or `printf`. Every head
/// candidate [`wrapped_command_heads`] yields must be a wrapper or one of
/// those two, so an ambiguous chain resolves to "not a literal emitter" (the
/// restrictive side). A stage that is not a simple command (`(cat f)`,
/// `{ cat f; }`) has no such head and is likewise not a literal emitter.
///
/// The one compound that is NOT judged here: a brace group whose members are
/// all plain `echo` / evaluable `printf` joined by `;`, a newline or `&&`
/// (`{ echo a; echo b; } | bash`). Its upstream stage is the closing `}`, which
/// this function reports as not an emitter, so the caller skips this check
/// when [`super::brace_feeder::group_feeder_texts`] computed the group's output
/// and judges that output (the concatenation, each member's literals and printf
/// readings) as the program instead. Every other group keeps the Ask.
pub(super) fn stage_emits_only_its_arguments(stage: &str) -> bool {
    let tokens: Vec<&str> = stage.split_whitespace().collect();
    let heads = wrapped_command_heads(&tokens);
    let Some(&last) = heads.last() else {
        return false;
    };
    let is_emitter =
        |idx: usize| matches!(normalized_command(tokens[idx]).as_str(), "echo" | "printf");
    is_emitter(last)
        && heads
            .iter()
            .all(|&idx| is_emitter(idx) || is_exec_wrapper(&normalized_command(tokens[idx])))
}

/// The Ask for an upstream `stage` that runs through an exec wrapper (`env
/// echo "ls"`, `command printf "ls"`) when the line is not one bare-wrapper
/// pipeline: `env` / `command` run what PATH or a function gives, so the
/// literal the stage prints is the program only when
/// [`crate::detect::readings::bare_wrappers_as_written`] holds for `cmd` (the
/// top-level line is that pipeline, see `b5::bare_wrapper_pipeline`).
/// `applies` is false in a `-c` / stdin body and when a brace group feeds the
/// reader (judged by its own rule); then, and for a stage with no wrapper in
/// front of its command, or a line that is that pipeline, it is `Allow` (no
/// objection). Raise-only, like every rule it is folded into.
pub(super) fn unresolved_wrapper_ask(
    shape: &str,
    stage: &str,
    cmd: &str,
    applies: bool,
) -> Decision {
    let upstream: Vec<&str> = stage.split_whitespace().collect();
    if !applies
        || wrapped_command_heads(&upstream).len() <= 1
        || crate::detect::readings::bare_wrappers_as_written(cmd)
    {
        return Decision::Allow;
    }
    Decision::ask(format!(
        "{shape}, and the stage piping into it (`{}`) runs through an exec wrapper, and the line \
is not a single pipeline that starts with it (other statements, a compound command or a `-c` / \
stdin body could set `PATH` or define a function named `env`, `command`, `echo` or `printf`) — \
blastguard cannot tell what the wrapper runs, so it cannot read the program",
        stage.trim()
    ))
}
