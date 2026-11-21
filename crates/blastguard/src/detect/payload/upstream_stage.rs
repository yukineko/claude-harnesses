//! The upstream stage of a line that pipes into a shell or interpreter: when an
//! exec wrapper is in front of its literal emitter, does the line resolve the
//! wrapper as written. (Whether the stage is a literal emitter at all is
//! `echo_output::stage_emits_only_its_arguments`.) Split out of `payload.rs`
//! for the file-growth cap.

use super::super::{wrapped_command_heads, Decision};

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
