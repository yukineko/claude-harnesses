//! The opinion of the B.5 layer on a line the parser rejects (backlog
//! 596ef57d, user ruling 2026-11-21 on R-fakeeof), split out of `b5.rs`
//! (file-growth cap).
//!
//! Ruling 2026-11-18 (b): a parse failure is not a B.5 shape, so the layer has
//! no opinion and the legacy verdict stands. Narrow exception (spec wins,
//! 2026-11-21): a line left unparsable AFTER a here-document has closed
//! resolves to Ask. The text past the terminator is outside the body, so what
//! runs there is unknown; it must not be Allow while undetermined.

use super::*;

/// This layer's opinion of `text` whose analysis failed with `cause`.
pub(super) fn parse_failure_opinion(text: &str, cause: &Cause) -> Decision {
    if matches!(cause, Cause::Parse(_)) && unparsable_after_closed_heredoc(text) {
        return undetermined(
            "the line cannot be parsed after a here-document closed, so what follows its terminator is not known",
        );
    }
    Decision::Allow
}

/// Whether some proper prefix of `text`, cut at a line end, parses (so its
/// here-document is terminated) and holds a here-document, while `text` as a
/// whole does not parse: the failure lies after a here-document closed. The
/// parser decides both facts; no terminator is searched for in the text. An
/// unterminated here-document does not parse in any prefix, so it is not this
/// shape (it keeps the no-opinion reading).
fn unparsable_after_closed_heredoc(text: &str) -> bool {
    if !text.contains("<<") {
        return false;
    }
    text.match_indices('\n').any(|(i, _)| {
        let prefix = &text[..i];
        match analyze(prefix) {
            Analysis::Known(s) => s.commands.iter().any(|c| {
                c.redirects
                    .iter()
                    .any(|r| matches!(r.target, RedirTarget::HereDoc(_)))
            }),
            Analysis::Undetermined(_) => false,
        }
    })
}
