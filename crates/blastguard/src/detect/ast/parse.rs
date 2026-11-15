//! Parse stage of `detect::ast`: pre-scan (`MAX_PARSE_BYTES` /
//! `MAX_PARSE_DEPTH`) and brush-parser 0.4.0.
//!
//! Skeleton: not implemented. Every input is `Err(Cause::NotImplemented)`;
//! the `ast-parse` task replaces this.

use super::Cause;

/// Parse `command` into a brush-parser program, or say why it cannot be.
pub(super) fn parse(_command: &str) -> Result<brush_parser::ast::Program, Cause> {
    Err(Cause::NotImplemented)
}
