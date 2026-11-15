//! Static evaluator of `detect::ast`: AST -> [`Script`].
//!
//! Skeleton: not implemented. [`evaluate`] is `Undetermined(NotImplemented)`
//! and [`program_source`] is `Some(ProgramSource::Undetermined(..))` for every
//! input — never a literal, never `None`, never an empty `Script`.

use super::{Analysis, Cause, ProgramSource, SimpleCommand};

#[cfg(doc)]
use super::Script;

/// Evaluate a parsed program.
pub(super) fn evaluate(_command: &str, _program: &brush_parser::ast::Program) -> Analysis {
    Analysis::Undetermined(Cause::NotImplemented)
}

/// See [`super::program_source`].
pub(super) fn program_source(_cmd: &SimpleCommand) -> Option<ProgramSource> {
    Some(ProgramSource::Undetermined(
        "program_source: not implemented".to_string(),
    ))
}
