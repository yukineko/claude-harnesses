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
//!   * EXCEPT, per file, the lines of a `diff --git` section whose target is a
//!     data file (`.md`, `.txt`, …: `heredoc::is_data_file_target`) or a
//!     non-shell code file (`.rs`, `.go`, …:
//!     `heredoc::is_non_shell_code_file_target`) — the heredoc-body-by-target-
//!     language rule applied to the file the patch writes (F4 of
//!     tests/backlog_20261116_git_apply_patch_known_gaps.rs: a README.md patch adding the text `rm -rf ~` was Deny). Those lines stay
//!     in the view AS WRITTEN (their `+` kept), so only the as-written text is
//!     judged for them. This needs ALL of:
//!       - the here-document delimiter is quoted, not `<<-`, and the body is
//!         closed (the shell passes it to git verbatim);
//!       - the whole body parses as plain git sections ([`git_patch_sections`]):
//!         `diff --git a/P b/P` with the same `P` (no quote, backslash, space,
//!         tab), only `index …` / `new file mode 100644` extended headers,
//!         `--- /dev/null` or `--- a/P`, `+++ b/P`, then hunks with no further
//!         `--- ` / `+++ ` / `diff ` line. Anything else — a missing or other
//!         `+++` path, `a/X b/Y` with X != Y, rename/copy, mode change, binary
//!         patch, mbox, traditional patch, an unknown line — leaves EVERY
//!         section of that body in today's (stripped) view;
//!       - every line outside the patch bodies is inert: each reader line is
//!         the reader alone with no `$`, backtick, `\` or `>`; every other line
//!         passes `heredoc::later_commands_cannot_run_stored_message` (only
//!         `ls`/`cat`/… and `git log|status|show`), so no command can run the
//!         written file (`sh README.md`, `sh *.md`, `make`, a `trap` before);
//!       - no line outside the patch bodies contains the file's name (even
//!         `cat README.md` keeps that section stripped).
//!
//!     A `.sh`, `.py` or extension-less target is always stripped. RESIDUAL:
//!     a LATER, separate tool call that runs the written data file is judged
//!     on its own text, as for `cat > notes.txt <<'EOF'`;
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

use super::heredoc::{
    here_document_delimiter_is_quoted, here_document_openers, is_data_file_target,
    is_non_shell_code_file_target, later_commands_cannot_run_stored_message,
    segment_closes_here_document,
};
use super::vars::quote_aware_words;
use crate::model::Decision;

/// What the patch readers of one command write, and what they refuse.
pub(super) struct PatchReading {
    /// The whole command, with the prefix character (`+` or ` `) removed from
    /// every line of each body this module took as a patch, except the lines
    /// of a data / non-shell-code file section kept as written (module doc).
    /// Other lines are unchanged (see KNOWN GAPS for bodies it does not take,
    /// or takes wrongly).
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
    let mut bodies: Vec<PatchBody> = Vec::new();
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
        // Per-file sections only for a QUOTED, non-`<<-`, CLOSED body (the
        // shell passes it to git verbatim); any other shape is undetermined.
        let sections = if !opener.strips_tabs
            && here_document_delimiter_is_quoted(line)
            && body_end < lines.len()
        {
            git_patch_sections(body)
        } else {
            None
        };
        bodies.push(PatchBody {
            reader_line: i,
            start: i + 1,
            end: body_end,
            sections,
        });
        i = body_end + 1;
    }
    if !found {
        return None;
    }
    // Every line outside the patch bodies and their closing delimiter lines:
    // the commands that run around the readers.
    let in_body_or_close = |j: usize| bodies.iter().any(|b| (b.start..=b.end).contains(&j));
    let reader_lines: Vec<usize> = bodies.iter().map(|b| b.reader_line).collect();
    let skeleton: Vec<&str> = (0..lines.len())
        .filter(|&j| !in_body_or_close(j))
        .map(|j| lines[j])
        .collect();
    let skeleton_inert = (0..lines.len()).filter(|&j| !in_body_or_close(j)).all(|j| {
        if reader_lines.contains(&j) {
            reader_line_is_plain(lines[j])
        } else {
            later_commands_cannot_run_stored_message(lines[j])
        }
    });
    for b in &bodies {
        let rel_keep: Vec<bool> = match &b.sections {
            Some(secs) if skeleton_inert => {
                let mut keep = vec![false; b.end - b.start];
                for s in secs {
                    let named = skeleton.iter().any(|l| l.contains(s.file_name.as_str()));
                    if s.data && !named {
                        for k in s.range.clone() {
                            keep[k] = true;
                        }
                    }
                }
                keep
            }
            _ => vec![false; b.end - b.start],
        };
        for (k, keep) in rel_keep.iter().enumerate() {
            let l = lines[b.start + k];
            if !keep && (l.starts_with('+') || l.starts_with(' ')) {
                view[b.start + k] = l[1..].to_string();
            }
        }
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

/// One here-document body taken as a reader's patch: line indices into the
/// command (`start..end` is the body, `end` its closing delimiter line).
struct PatchBody {
    reader_line: usize,
    start: usize,
    end: usize,
    /// The per-file sections, or `None` when the body is not wholly a git
    /// diff this module can read (then every section keeps today's view).
    sections: Option<Vec<PatchSection>>,
}

/// One `diff --git a/P b/P` section of a patch body.
struct PatchSection {
    /// Its lines, relative to the body start.
    range: std::ops::Range<usize>,
    /// The last path component of `P`.
    file_name: String,
    /// `P` is a data file ([`is_data_file_target`]) or a non-shell code file
    /// ([`is_non_shell_code_file_target`]): `.md`, `.txt`, `.rs`, … — never
    /// `.sh`, `.py` or extension-less.
    data: bool,
}

/// The per-file sections of `body`, or `None` when any part of it is not a
/// plain git diff: the body must start with `diff --git `, and each section
/// must be exactly `diff --git a/P b/P` (same `P`, no quote, backslash, space
/// or tab), then only `index …` / `new file mode 100644` lines, then
/// `--- /dev/null` or `--- a/P`, then `+++ b/P`, then hunks (`@@ ` headers and
/// lines starting ` `, `+`, `-`, `\`, or empty) with no further `--- ` /
/// `+++ ` line (which could start another file's header) and no `diff ` line
/// other than the next section's. Renames, copies, mode changes, binary
/// patches, mbox mails and traditional patches all answer `None`.
fn git_patch_sections(body: &[&str]) -> Option<Vec<PatchSection>> {
    if !body.first()?.starts_with("diff --git ") {
        return None;
    }
    let starts: Vec<usize> = (0..body.len())
        .filter(|&k| body[k].starts_with("diff --git "))
        .collect();
    let mut out = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(body.len());
        let sec = &body[s..e];
        let rest = sec[0].strip_prefix("diff --git ")?;
        if rest.contains(['"', '\\', '\t']) {
            return None;
        }
        let path = match rest.split(' ').collect::<Vec<_>>().as_slice() {
            [a, b] => {
                let (a, b) = (a.strip_prefix("a/")?, b.strip_prefix("b/")?);
                (a == b && !a.is_empty()).then_some(a)?
            }
            _ => return None,
        };
        let mut k = 1;
        while k < sec.len() && (sec[k].starts_with("index ") || sec[k] == "new file mode 100644") {
            k += 1;
        }
        let minus = *sec.get(k)?;
        if minus != "--- /dev/null" && minus != format!("--- a/{path}") {
            return None;
        }
        if *sec.get(k + 1)? != format!("+++ b/{path}") {
            return None;
        }
        if !sec.get(k + 2)?.starts_with("@@ ") {
            return None;
        }
        let hunks_ok = sec[k + 2..].iter().all(|l| {
            !l.starts_with("--- ")
                && !l.starts_with("+++ ")
                && !l.starts_with("diff ")
                && (l.is_empty() || l.starts_with(['@', ' ', '+', '-', '\\']))
                && (!l.starts_with('@') || l.starts_with("@@ "))
        });
        if !hunks_ok {
            return None;
        }
        let file_name = path.rsplit('/').next().unwrap_or(path).to_string();
        out.push(PatchSection {
            range: s..e,
            file_name,
            data: is_data_file_target(path) || is_non_shell_code_file_target(path),
        });
    }
    Some(out)
}

/// True when the reader's line is the reader alone (one segment, so no other
/// command shares the line) with no `$`, backtick, backslash or `>` on it: no
/// expansion can add an option or a command, and no output is redirected.
fn reader_line_is_plain(line: &str) -> bool {
    line_segments(line).len() == 1 && !line.contains(['$', '`', '\\', '>'])
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

    /// One-file git patch creating `path` with the single added line `added`.
    fn new_file_patch(path: &str, added: &str) -> String {
        format!(
            "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+{added}\n"
        )
    }

    fn reader(body: &str) -> String {
        format!("git apply <<'EOF'\n{body}EOF")
    }

    fn verdict(cmd: &str) -> Decision {
        crate::detect::detect("Bash", Some(&serde_json::json!({ "command": cmd })))
    }

    /// The body line `+rm -rf ~` kept as written (not stripped into the view).
    fn kept(cmd: &str) -> bool {
        let v = view_of(cmd);
        v.contains("\n+rm -rf ~\n") && !v.contains("\nrm -rf ~\n")
    }

    /// The body line stripped into the view (today's shell view).
    fn stripped(cmd: &str) -> bool {
        view_of(cmd).contains("\nrm -rf ~")
    }

    #[test]
    fn data_and_non_shell_code_targets_keep_their_body_out_of_the_shell_view() {
        for path in [
            "README.md",
            "docs/notes.txt",
            "src/lib.rs",
            "a/b.go",
            "t.csv",
        ] {
            let cmd = reader(&new_file_patch(path, "rm -rf ~"));
            assert!(kept(&cmd), "{path}: {}", view_of(&cmd));
            assert!(refusal_of(&cmd).is_none(), "{path}");
        }
        // Inert later commands that do not name the path leave it kept.
        let cmd = format!(
            "{}\nls\ngit status",
            reader(&new_file_patch("README.md", "rm -rf ~"))
        );
        assert!(kept(&cmd), "{}", view_of(&cmd));
    }

    #[test]
    fn data_patch_with_nothing_run_is_allow_end_to_end() {
        let cmd = reader(&new_file_patch("README.md", "rm -rf ~"));
        assert_eq!(verdict(&cmd), Decision::Allow, "{cmd}");
    }

    #[test]
    fn shell_python_and_extensionless_targets_keep_todays_view() {
        for path in [
            "run.sh",
            "Makefile",
            "bin/tool",
            "x.py",
            ".git/hooks/pre-commit",
        ] {
            let cmd = reader(&new_file_patch(path, "rm -rf ~"));
            assert!(stripped(&cmd), "{path}: {}", view_of(&cmd));
        }
    }

    #[test]
    fn a_command_that_runs_or_names_the_path_keeps_todays_view() {
        let p = reader(&new_file_patch("README.md", "rm -rf ~"));
        for tail in [
            "sh README.md",
            "bash README.md",
            ". ./README.md",
            "cat README.md",
            "sh *.md",
            "make",
            "x=1",
            "cargo test",
        ] {
            let cmd = format!("{p}\n{tail}");
            assert!(stripped(&cmd), "{tail}: {}", view_of(&cmd));
        }
        // On the reader's line: another segment, or an expansion.
        for head in [
            "git apply <<'EOF'; sh README.md",
            "cd x && git apply <<'EOF'",
            "git apply $o <<'EOF'",
            "git apply <<'EOF' > out.log",
        ] {
            let cmd = format!("{head}\n{}EOF", new_file_patch("README.md", "rm -rf ~"));
            assert!(stripped(&cmd), "{head}: {}", view_of(&cmd));
        }
        // A line before the reader.
        let cmd = format!("trap 'sh README.md' EXIT\n{p}");
        assert!(stripped(&cmd), "{}", view_of(&cmd));
    }

    #[test]
    fn unclassifiable_headers_keep_todays_view() {
        let body = |s: &str| reader(s);
        let cases = [
            // missing `+++ b/<path>`
            "diff --git a/README.md b/README.md\n--- /dev/null\n@@ -0,0 +1 @@\n+rm -rf ~\n",
            // `+++` naming a different path
            "diff --git a/README.md b/README.md\n--- /dev/null\n+++ b/run.sh\n@@ -0,0 +1 @@\n+rm -rf ~\n",
            // X != Y
            "diff --git a/run.sh b/README.md\n--- a/run.sh\n+++ b/README.md\n@@ -0,0 +1 @@\n+rm -rf ~\n",
            // rename / copy
            "diff --git a/README.md b/README.md\nrename from run.sh\nrename to README.md\n--- a/README.md\n+++ b/README.md\n@@ -0,0 +1 @@\n+rm -rf ~\n",
            "diff --git a/README.md b/README.md\ncopy from run.sh\ncopy to README.md\n--- a/README.md\n+++ b/README.md\n@@ -0,0 +1 @@\n+rm -rf ~\n",
            // binary
            "diff --git a/README.md b/README.md\nGIT binary patch\nliteral 9\n+rm -rf ~\n",
            // mbox
            "From 0 Mon Sep 17 00:00:00 2001\nSubject: [PATCH] x\n\ndiff --git a/README.md b/README.md\n--- /dev/null\n+++ b/README.md\n@@ -0,0 +1 @@\n+rm -rf ~\n",
            // quoted / escaped path
            "diff --git \"a/README.md\" \"b/README.md\"\n--- /dev/null\n+++ \"b/README.md\"\n@@ -0,0 +1 @@\n+rm -rf ~\n",
            "diff --git a/READ\\ME.md b/READ\\ME.md\n--- /dev/null\n+++ b/READ\\ME.md\n@@ -0,0 +1 @@\n+rm -rf ~\n",
            // path with a space
            "diff --git a/a b.md b/a b.md\n--- /dev/null\n+++ b/a b.md\n@@ -0,0 +1 @@\n+rm -rf ~\n",
            // mode that makes the file executable
            "diff --git a/README.md b/README.md\nnew file mode 100755\n--- /dev/null\n+++ b/README.md\n@@ -0,0 +1 @@\n+rm -rf ~\n",
            // traditional (non-git) patch
            "--- /dev/null\n+++ b/README.md\n@@ -0,0 +1 @@\n+rm -rf ~\n",
            // a second `---`/`+++` pair inside one section
            "diff --git a/README.md b/README.md\n--- /dev/null\n+++ b/README.md\n@@ -0,0 +1 @@\n+hi\n--- /dev/null\n+++ b/run.sh\n@@ -0,0 +1 @@\n+rm -rf ~\n",
            // an unknown line after the hunks
            "diff --git a/README.md b/README.md\n--- /dev/null\n+++ b/README.md\n@@ -0,0 +1 @@\n+rm -rf ~\ngarbage\n",
        ];
        for c in cases {
            let cmd = body(c);
            assert!(stripped(&cmd), "{c}: {}", view_of(&cmd));
        }
        // Several files, one unclassifiable: the whole body keeps today's view.
        let multi = format!(
            "{}diff --git a/run.sh b/README.md\n--- a/run.sh\n+++ b/README.md\n@@ -0,0 +1 @@\n+x\n",
            new_file_patch("README.md", "rm -rf ~")
        );
        assert!(stripped(&reader(&multi)), "{}", view_of(&reader(&multi)));
        // Unquoted delimiter (the shell expands the body) and `<<-`.
        let p = new_file_patch("README.md", "rm -rf ~");
        assert!(stripped(&format!("git apply <<EOF\n{p}EOF")));
        assert!(stripped(&format!("git apply <<-'EOF'\n{p}EOF")));
    }

    #[test]
    fn per_file_sections_strip_only_the_shell_file() {
        let cmd = reader(&format!(
            "{}{}",
            new_file_patch("README.md", "rm -rf ~"),
            new_file_patch("run.sh", "chmod 777 /etc/hosts")
        ));
        let v = view_of(&cmd);
        assert!(v.contains("\n+rm -rf ~\n"), "{v}");
        assert!(v.contains("\nchmod 777 /etc/hosts\n"), "{v}");
    }

    #[test]
    fn shell_target_and_readme_run_stay_blocking_end_to_end() {
        // 42741ea3 shape.
        let run = format!(
            "{}\nsh run.sh",
            reader(&new_file_patch("run.sh", "chmod 777 /etc/hosts"))
        );
        assert_ne!(verdict(&run), Decision::Allow, "{run}");
        // README.md patch, then run as a script.
        for tail in ["sh README.md", "bash README.md"] {
            let cmd = format!(
                "{}\n{tail}",
                reader(&new_file_patch("README.md", "rm -rf ~"))
            );
            assert_ne!(verdict(&cmd), Decision::Allow, "{cmd}");
        }
        // System harm added to a shell-target file stays Deny with nothing run.
        let harm = reader(&new_file_patch("run.sh", "rm -rf /"));
        assert!(verdict(&harm).is_deny(), "{harm}: {:?}", verdict(&harm));
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
