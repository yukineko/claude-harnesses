//! `git apply` / `git am` fed a patch through a here-document write the
//! patch's files into the working tree, and a later command can run them
//! (`git apply <<'EOF' … EOF` then `sh run.sh`). The body is not data here:
//! the patch's added lines (`+rm -rf ~`) become the file's text, minus the
//! `+` diff prefix. Judged as written, the `+` hides the command from every
//! shell rule, so `sh run.sh` was Allow (backlog 42741ea3).
//!
//! What this module does, for a command that contains such a reader:
//!   * builds a SHELL VIEW of the whole command in which every body line that
//!     starts with `+` or a space (added and context lines: the text the file
//!     ends up with) loses that one prefix character. The caller judges the view
//!     with every shell rule, and the verdicts are combined `Deny > Ask >
//!     Allow` with the verdict of the command as written;
//!   * REFUSES (`Ask`) in exactly these shapes, because the patch cannot be
//!     read from the command text:
//!       - the reader's segment comes right after a plain `|` (an unquoted `|`
//!         immediately before it; `a || git apply` also lands here, which is
//!         only stricter);
//!       - the reader's line has no here-document but contains `<` (a `<` file
//!         or a here-string);
//!       - the reader's line has more than one here-document, or the
//!         here-document's delimiter cannot be read;
//!       - the here-document body has no patch header (`diff `, `--- `,
//!         `+++ `, `@@ `, `Subject:`, `From `).
//!
//! KNOWN GAPS (this module does NOT make these safe; they may stay Allow, and
//! none of them is covered by the refusals above):
//!   * `|&` (stdout+stderr pipe): the segment after it is not marked as piped,
//!     so `printf … |& git apply` is not refused;
//!   * a reader inside a subshell / group: `cat p | (git apply)` is not
//!     refused, because the segment after `(` is not marked as piped;
//!   * a pipe at the end of a line with the reader on the next line: segments
//!     are computed per physical line, so the pipe is not seen;
//!   * `<<-`: the shell strips leading TABs from the body, but this module
//!     strips only one `+` or space prefix, so a body line like `\t+rm -rf ~`
//!     is not turned into the text the file will hold;
//!   * here-documents are found on the reader's whole LINE, not its segment:
//!     in `cat <<'A' >f; git apply < x.patch` the `cat` body is taken as the
//!     reader's patch, so the `<` refusal is not raised, and when that body has
//!     a patch header nothing is refused (x.patch itself is never read);
//!   * `git apply x.patch` / `git am x.mbox` naming a file: the command text
//!     does not contain that file and blastguard never reads it (a residual,
//!     like any file written by an earlier tool call);
//!   * a `git` that is not a plain word of the segment (quoted, or produced
//!     by a variable or substitution): recognition below works on the segment's
//!     words and these forms have no test here, so treat them as unhandled.
//!
//! The reader is recognised per shell segment: a `git` word followed later in
//! the segment by the word `apply` or `am` (global options such as `-C dir` /
//! `-c k=v` / `--git-dir x` are skipped by that rule being loose: any later
//! word counts). Over-recognition costs only a stricter answer.

use super::heredoc::{here_document_openers, segment_closes_here_document};
use super::vars::quote_aware_words;
use crate::model::Decision;

/// What the patch readers of one command write, and what they refuse.
pub(super) struct PatchReading {
    /// The whole command, with the prefix character (`+` or ` `) removed from
    /// every line of each body this module took as a patch. Other lines are
    /// unchanged (see KNOWN GAPS for bodies it does not take, or takes wrongly).
    pub(super) shell_view: String,
    /// Why some reader's input could not be read as a patch: a refusal to
    /// guess, judged as `Ask`. `None` when none of the refusal shapes in the
    /// module doc matched — which is NOT proof that every reader was read
    /// (the KNOWN GAPS leave readers unread with `None`).
    pub(super) refusal: Option<String>,
}

/// The patch reading of `cmd`, or `None` when no segment is recognised as a
/// `git apply` / `git am` reader (a `git` that is not a plain word is not
/// recognised; see KNOWN GAPS).
pub(super) fn read_patch_readers(cmd: &str) -> Option<PatchReading> {
    let lines: Vec<&str> = cmd.split('\n').collect();
    let mut view: Vec<String> = lines.iter().map(|l| (*l).to_string()).collect();
    let mut found = false;
    let mut refusals: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let segments = line_segments(line);
        let readers: Vec<&(bool, String)> = segments
            .iter()
            .filter(|(_, seg)| is_git_patch_reader(seg))
            .collect();
        if readers.is_empty() {
            i += 1;
            continue;
        }
        found = true;
        let openers = here_document_openers(line);
        if openers.len() > 1 {
            refusals.push(
                "git apply/am with more than one here-document on one line: cannot tell which body is its patch"
                    .to_string(),
            );
            i += 1;
            continue;
        }
        for (after_pipe, seg) in &readers {
            if *after_pipe {
                refusals.push(format!(
                    "git apply/am reads its patch from a pipe (`{}`): the patch cannot be read here",
                    seg.trim()
                ));
            }
        }
        let Some(opener) = openers.first() else {
            // No here-document on the line. A `<` (file or here-string) feeds the
            // patch from outside the text, or from a string blastguard cannot
            // unquote; refuse. Otherwise git reads a named file: left as is.
            if line.contains('<') {
                refusals.push(
                    "git apply/am reads its patch through `<` (not a here-document): the patch cannot be read here"
                        .to_string(),
                );
            }
            i += 1;
            continue;
        };
        if opener.delimiter.is_empty() {
            refusals.push(
                "git apply/am here-document delimiter cannot be read: the patch cannot be read here"
                    .to_string(),
            );
            i += 1;
            continue;
        }
        // The body runs to the first exact delimiter line; an unclosed body runs
        // to the end of the command, as the shell reads it.
        let body_end = (i + 1..lines.len())
            .find(|&j| segment_closes_here_document(lines[j], opener))
            .unwrap_or(lines.len());
        let body = &lines[i + 1..body_end];
        if !body.iter().any(|l| is_patch_header(l)) {
            refusals.push(
                "git apply/am here-document body has no patch header (`diff `, `--- `, `+++ `, `@@ `, `Subject:`, `From `): cannot tell what it writes"
                    .to_string(),
            );
        }
        for (k, l) in body.iter().enumerate() {
            if l.starts_with('+') || l.starts_with(' ') {
                view[i + 1 + k] = l[1..].to_string();
            }
        }
        i = body_end + 1;
    }
    if !found {
        return None;
    }
    let refusal = if refusals.is_empty() {
        None
    } else {
        Some(refusals.join("; "))
    };
    Some(PatchReading {
        shell_view: view.join("\n"),
        refusal,
    })
}

/// True when this segment runs `git` with a later word `apply` or `am`.
fn is_git_patch_reader(seg: &str) -> bool {
    let words = quote_aware_words(seg);
    words.iter().enumerate().any(|(g, w)| {
        let base = w.rsplit('/').next().unwrap_or(w);
        base == "git" && words[g + 1..].iter().any(|x| x == "apply" || x == "am")
    })
}

/// A line a patch (or an mbox) carries as a header; a body without one is not
/// read as a patch.
fn is_patch_header(line: &str) -> bool {
    ["diff ", "--- ", "+++ ", "@@ ", "Subject:", "From "]
        .iter()
        .any(|p| line.starts_with(p))
}

/// Splits one physical line at unquoted `|`, `&`, `;`, `(` and `)`. Each piece
/// carries whether the separator character right before it was `|` (the flag
/// is true after `|` and `||`; it is false after `|&`, because the `&` is
/// itself a separator and resets it, and after `(`, `;`, `&`, `)`; and it is
/// per physical line, so a trailing `|` does not reach the next line). Quotes
/// and backslash escapes are kept as text; only the split is computed here.
fn line_segments(line: &str) -> Vec<(bool, String)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut cur = String::new();
    let (mut in_s, mut in_d) = (false, false);
    let mut after_pipe = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if !in_s && c == '\\' {
            cur.push(c);
            if let Some(&n) = chars.get(i + 1) {
                cur.push(n);
            }
            i += 2;
            continue;
        }
        if c == '\'' && !in_d {
            in_s = !in_s;
        } else if c == '"' && !in_s {
            in_d = !in_d;
        } else if !in_s && !in_d && matches!(c, '|' | '&' | ';' | '(' | ')') {
            out.push((after_pipe, std::mem::take(&mut cur)));
            after_pipe = c == '|';
            i += 1;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    out.push((after_pipe, cur));
    out
}

/// The stronger of two verdicts under `Deny > Ask > Allow`. On a tie the first
/// is kept.
pub(super) fn stronger(a: Decision, b: Decision) -> Decision {
    match (&a, &b) {
        (Decision::Deny(_), _) => a,
        (_, Decision::Deny(_)) => b,
        (Decision::Ask(_), _) => a,
        (_, Decision::Ask(_)) => b,
        _ => a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_of(cmd: &str) -> String {
        read_patch_readers(cmd)
            .map(|r| r.shell_view)
            .unwrap_or_default()
    }

    fn refusal_of(cmd: &str) -> Option<String> {
        read_patch_readers(cmd).and_then(|r| r.refusal)
    }

    #[test]
    fn no_reader_means_no_reading() {
        assert!(read_patch_readers("git log -1").is_none());
        assert!(read_patch_readers("echo \"git apply\" && ls").is_none());
        // A named patch file is not in the text: the reader is seen, the view is
        // the command itself, and nothing is refused (residual, documented above).
        let named = read_patch_readers("git apply x.patch").expect("reader seen");
        assert_eq!(named.shell_view, "git apply x.patch");
        assert!(named.refusal.is_none());
    }

    #[test]
    fn added_and_context_lines_lose_their_prefix() {
        let cmd = "git apply <<'EOF'\ndiff --git a/r.sh b/r.sh\n--- /dev/null\n+++ b/r.sh\n@@ -0,0 +1 @@\n+rm -rf ~\n EOF_CTX\n-old\nEOF\nsh r.sh";
        let v = view_of(cmd);
        assert!(v.contains("\nrm -rf ~\n"), "{v}");
        assert!(v.contains("\n++ b/r.sh\n"), "{v}");
        assert!(v.contains("\nEOF_CTX\n"), "{v}");
        assert!(v.contains("\n-old\n"), "{v}");
        assert!(v.ends_with("\nsh r.sh"), "{v}");
        assert!(refusal_of(cmd).is_none());
    }

    #[test]
    fn git_am_with_options_is_a_reader() {
        let cmd = "git -C x am <<'EOF'\nSubject: [PATCH] t\n+rm -rf ~\nEOF";
        assert!(view_of(cmd).contains("\nrm -rf ~\n"));
        assert!(refusal_of(cmd).is_none());
    }

    #[test]
    fn pipe_and_file_redirect_stdin_are_refused() {
        assert!(refusal_of("printf 'diff --git a b' | git apply").is_some());
        assert!(refusal_of("git apply < x.patch").is_some());
        assert!(refusal_of("git apply <<<'diff --git a b'").is_some());
    }

    #[test]
    fn body_without_patch_header_is_refused() {
        assert!(refusal_of("git apply <<'EOF'\nrm -rf ~\nEOF").is_some());
    }

    #[test]
    fn unclosed_body_runs_to_the_end() {
        let cmd = "git apply <<'EOF'\n+++ b/r.sh\n+rm -rf ~";
        assert!(view_of(cmd).ends_with("\nrm -rf ~"));
        assert!(refusal_of(cmd).is_none());
    }

    #[test]
    fn stronger_ranks_deny_over_ask_over_allow() {
        assert_eq!(
            stronger(Decision::Allow, Decision::ask("a")),
            Decision::ask("a")
        );
        assert_eq!(
            stronger(Decision::ask("a"), Decision::deny("d")),
            Decision::deny("d")
        );
        assert_eq!(stronger(Decision::Allow, Decision::Allow), Decision::Allow);
    }
}
