use super::{
    basename, git_subcommand_index, is_code_interpreter, is_shell, quote_aware_words,
    split_segments_with_separators, strip_version_suffix, SegmentSep, SeparatedSegment,
};

/// True when this segment opens a HERE-DOCUMENT (`<<WORD`, `<<-WORD`,
/// `<<'WORD'`), so the lines that follow it are a data body rather than
/// commands.
///
/// `<<<` is a here-STRING, which is deliberately excluded: its operand is on
/// the same line and no following line is consumed, so the next segment really
/// is code.
///
/// Quote-aware, because a `<<` inside quotes is data (`echo "a << b"`).
/// True when a here-document's delimiter is QUOTED (`<<'TAG'`, `<<"TAG"`,
/// `<<\TAG`), which is the shell telling itself to pass the body through
/// verbatim: no parameter expansion, no command substitution, no backticks.
///
/// This is not a heuristic, it is the documented behaviour of the construct,
/// and it decides whether a `$` in the body is a shell expansion or ordinary
/// program text. Measured 2026-09-09: 28 of this operator's here-documents were
/// being withheld from Allow because their Python bodies contained a `$` — a
/// regex end-anchor, or a `${…}` inside a Rust format string being edited — in
/// a `<<'PY'` heredoc where the shell substitutes nothing at all. Reading those
/// as "the text is a template" was simply false about the shell.
///
/// Returns false for an unquoted delimiter and for a segment with no
/// here-document at all: both of those are the side where expansion CAN happen,
/// which is the restrictive answer for every caller.
pub(super) fn here_document_delimiter_is_quoted(seg: &str) -> bool {
    let chars: Vec<char> = seg.chars().collect();
    let (mut in_s, mut in_d) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' && !in_d {
            in_s = !in_s;
            i += 1;
            continue;
        }
        if c == '"' && !in_s {
            in_d = !in_d;
            i += 1;
            continue;
        }
        if !in_s && !in_d && c == '<' && chars.get(i + 1) == Some(&'<') {
            if chars.get(i + 2) == Some(&'<') {
                i += 3;
                continue;
            }
            // Skip `-` (`<<-TAG`, the tab-stripping form) and any spaces, then
            // look at the first character of the delimiter itself.
            let mut j = i + 2;
            while matches!(chars.get(j), Some('-') | Some(' ') | Some('\t')) {
                j += 1;
            }
            return matches!(chars.get(j), Some('\'') | Some('"') | Some('\\'));
        }
        i += 1;
    }
    false
}

pub(super) fn opens_here_document(seg: &str) -> bool {
    let chars: Vec<char> = seg.chars().collect();
    let (mut in_s, mut in_d) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' && !in_d {
            in_s = !in_s;
            i += 1;
            continue;
        }
        if c == '"' && !in_s {
            in_d = !in_d;
            i += 1;
            continue;
        }
        if !in_s && !in_d && c == '<' && chars.get(i + 1) == Some(&'<') {
            if chars.get(i + 2) == Some(&'<') {
                // Here-string: consume all three so the trailing pair of this
                // `<<<` is not re-read as a here-document opener.
                i += 3;
                continue;
            }
            return true;
        }
        i += 1;
    }
    false
}

/// One here-document opener: the delimiter word it declares, and whether the
/// `<<-` spelling was used (which lets the closing line be indented with TABS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HereDocOpener {
    pub(super) delimiter: String,
    pub(super) strips_tabs: bool,
}

/// Every here-document this segment OPENS, in the order the shell will consume
/// their bodies.
///
/// Quote-aware for the same reason [`opens_here_document`] is: a `<<` inside
/// quotes is data. `<<<` is a here-STRING and opens no body, so all three
/// characters are consumed.
///
/// The delimiter word may be written bare (`<<EOF`), single- or double-quoted
/// (`<<'EOF'`, `<<"EOF"`) or backslash-escaped (`<<\EOF`); all four spell the
/// same delimiter, and only the quoting changes whether the BODY is expanded —
/// a distinction [`here_document_delimiter_is_quoted`] makes elsewhere and this
/// function deliberately does not need.
pub(super) fn here_document_openers(seg: &str) -> Vec<HereDocOpener> {
    let chars: Vec<char> = seg.chars().collect();
    let (mut in_s, mut in_d) = (false, false);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' && !in_d {
            in_s = !in_s;
            i += 1;
            continue;
        }
        if c == '"' && !in_s {
            in_d = !in_d;
            i += 1;
            continue;
        }
        if !in_s && !in_d && c == '<' && chars.get(i + 1) == Some(&'<') {
            if chars.get(i + 2) == Some(&'<') {
                i += 3;
                continue;
            }
            i += 2;
            let strips_tabs = chars.get(i) == Some(&'-');
            if strips_tabs {
                i += 1;
            }
            while chars.get(i) == Some(&' ') || chars.get(i) == Some(&'\t') {
                i += 1;
            }
            let mut delimiter = String::new();
            let mut quote: Option<char> = None;
            while let Some(&ch) = chars.get(i) {
                match quote {
                    Some(q) => {
                        i += 1;
                        if ch == q {
                            quote = None;
                        } else {
                            delimiter.push(ch);
                        }
                    }
                    None => {
                        if ch == '\'' || ch == '"' {
                            quote = Some(ch);
                            i += 1;
                        } else if ch == '\\' {
                            i += 1;
                            if let Some(&esc) = chars.get(i) {
                                delimiter.push(esc);
                                i += 1;
                            }
                        } else if ch.is_whitespace()
                            || ch == ';'
                            || ch == '|'
                            || ch == '&'
                            || ch == '>'
                            || ch == '<'
                            || ch == ')'
                        {
                            break;
                        } else {
                            delimiter.push(ch);
                            i += 1;
                        }
                    }
                }
            }
            // An unterminated quote means the delimiter was never finished on
            // this segment; refusing to record it keeps the body OPEN, which is
            // the conservative direction (see `here_document_layout`).
            if quote.is_none() && !delimiter.is_empty() {
                out.push(HereDocOpener {
                    delimiter,
                    strips_tabs,
                });
            } else {
                out.push(HereDocOpener {
                    delimiter: String::new(),
                    strips_tabs: false,
                });
            }
            continue;
        }
        i += 1;
    }
    out
}

/// True when this segment is the line that CLOSES the given here-document.
///
/// The match is exact on purpose. `bash` ends a here-document only on a line
/// holding the delimiter and nothing else — `EOF ` with a trailing space does
/// NOT close it — so anything short of exact equality must leave the body open.
/// Reading a near-miss as a close would hand the segments after it back to the
/// resolver as code when the shell is still treating them as data, which is the
/// permissive direction and precisely what this scan exists to avoid.
/// `<<-` is the one documented relaxation: it strips LEADING TABS (not spaces)
/// from the closing line.
pub(super) fn segment_closes_here_document(seg: &str, opener: &HereDocOpener) -> bool {
    if opener.delimiter.is_empty() {
        return false;
    }
    let line = if opener.strips_tabs {
        seg.trim_start_matches('\t')
    } else {
        seg
    };
    line == opener.delimiter
}

/// The BODY of the here-document opened on the first line of `cmd` — the lines
/// strictly between the opener line and its closing delimiter line — when, and
/// only when, `stage` (one pipe stage of `cmd`) is the stage on that first line
/// that opens it and the shape leaves no doubt about where the body ends.
/// Otherwise `None`, and the caller keeps treating everything after the first
/// newline as the program (the restrictive over-approximation).
///
/// Backlog aafeaa05: `payload::analyze_interpreter_stdin_exec` took
/// "everything after the first newline" as the program of
/// `python3 - <<'PY'`, so a quoted, non-redirect `>` in an ordinary shell
/// command AFTER the `PY` terminator (`echo '>'`, `grep '^>' x`) was reported
/// as "the program text contains a `>`" of that program (user ruling R2's
/// Ask, a Deny in a subagent). The lines after the terminator are not the
/// program: the shell stops feeding stdin at the delimiter line, and those
/// lines are judged as ordinary shell by every other rule of `detect_bash`
/// (a real redirect there, `echo x > .git/hooks/pre-commit`, is still denied
/// by those rules — pinned by tests/quoted_heredoc_python_gt_fp_20261107.rs).
///
/// Restrictive on every doubt; `None` unless ALL of these hold:
///   * `cmd` has no whitespace other than space, tab and newline (bash keeps a
///     `\r` in the delimiter word, so a `PY` line would not close `<<'PY'\r`);
///   * the first line has balanced quotes, does not end in `\` (a
///     continuation), and contains exactly one `<<` — counted textually, not
///     quote-aware, so a here-string `<<<` or a second opener also refuses;
///   * that one opener has a non-empty, QUOTED delimiter. With an unquoted
///     delimiter bash processes backslash-newline inside the body, so a
///     delimiter-looking line need not close it;
///   * `stage` contains `<<` and is a substring of the first line;
///   * some later line is exactly the delimiter (leading tabs stripped for
///     `<<-`), see [`segment_closes_here_document`];
///   * the text after that closing line contains no `<<` at all (textually),
///     so no later stage can open a second here-document whose body this
///     narrowing would hide from a stage that merely looks the same.
pub(super) fn first_line_quoted_here_document_body<'a>(
    cmd: &'a str,
    stage: &str,
) -> Option<&'a str> {
    if cmd
        .chars()
        .any(|c| c.is_whitespace() && !matches!(c, ' ' | '\t' | '\n'))
    {
        return None;
    }
    let (first, after_first) = cmd.split_once('\n')?;
    if !quotes_balance(first) || first.ends_with('\\') || first.matches("<<").count() != 1 {
        return None;
    }
    if !here_document_delimiter_is_quoted(first) {
        return None;
    }
    let openers = here_document_openers(first);
    let [opener] = openers.as_slice() else {
        return None;
    };
    if opener.delimiter.is_empty() {
        return None;
    }
    let stage = stage.trim();
    if !stage.contains("<<") || !first.contains(stage) {
        return None;
    }
    let mut offset = 0;
    for line in after_first.split('\n') {
        if segment_closes_here_document(line, opener) {
            let rest = &after_first[offset + line.len()..];
            if rest.contains("<<") {
                return None;
            }
            // The body without the newline that precedes the closing line.
            return Some(after_first[..offset].strip_suffix('\n').unwrap_or(""));
        }
        offset += line.len() + 1;
    }
    None
}

/// Which segments are here-document BODY (data the shell never executes), and
/// whether any here-document is left OPEN at the end of the line.
///
/// Returns `(is_body, first_unclosed_opener_index)`.
///
/// Why this is worth modelling rather than treating every here-document as an
/// opaque tail: a body is the one shape where text that reads exactly like an
/// assignment never ran at all, so the resolver must not believe it — and, once
/// the delimiter line has been SEEN, the segments after it are ordinary code
/// and there is no longer any reason to disbelieve them. Collapsing both into
/// "a here-document appeared, distrust everything after it" made
/// `P=a` / `cat <<'EOF'` / … / `EOF` / `P=/etc/fstab` / `echo x > "$P"`
/// unresolvable, so the verdict could not name the file the shell truncates
/// even though the line says it plainly. Pinned by
/// tests/redirect_target_resolution.rs.
///
/// Both failure directions stay conservative: an opener whose delimiter cannot
/// be read, and a delimiter line that never arrives, both leave the body open
/// to the end of the line, which is exactly the old coarse behaviour.
pub(super) fn here_document_layout(segs: &[SeparatedSegment]) -> (Vec<bool>, Option<usize>) {
    let mut is_body = vec![false; segs.len()];
    let mut pending: Vec<HereDocOpener> = Vec::new();
    let mut opener_idx: Option<usize> = None;
    for idx in 0..segs.len() {
        if pending.is_empty() {
            let opened = here_document_openers(&segs[idx].text);
            if !opened.is_empty() {
                pending = opened;
                opener_idx = Some(idx);
            }
            continue;
        }
        is_body[idx] = true;
        if segment_closes_here_document(&segs[idx].text, &pending[0]) {
            pending.remove(0);
            if pending.is_empty() {
                opener_idx = None;
            }
        }
    }
    (is_body, opener_idx)
}

/// Per segment of [`split_segments_with_separators`]`(cmd)`: `Some(k)` when the
/// segment lies wholly inside the body (closing delimiter line included) of the
/// `k`-th CLOSED, QUOTED-delimiter here-document of the line, `None` when it is
/// the line's own shell text. An empty vector means "no segment is body": the
/// caller keeps ONE cwd walk over every segment, exactly as before this
/// existed.
///
/// Backlog 547cf2f6: the cwd walk ([`segment_cwd_states`] and the per-segment
/// loop of [`detect_bash`]) counted the `(`/`)` of such a body as subshell
/// groups of the line, so one unbalanced `)` in a Rust body
/// (`.filter(|p| !before.contains(p)) {` is fine, a lone `)` line is not)
/// marked the line unbalanced, and the literal `cd /abs/wt` before the opener
/// then made every later relative redirect "after a cd blastguard could not
/// statically resolve". The bash manual: with a quoted delimiter the body is
/// not expanded, and the body is the reader's stdin — the shell never parses
/// it, so it holds no groups and runs no `cd` of the line. The caller walks
/// each body on its own (it is still judged as code, as if it ran) and the
/// line past it resumes from the cwd in force at the opener.
///
/// Every doubt answers the empty vector (one walk, the old behaviour), because
/// calling LINE text "body" would hide a real `cd` or `(` from the walk:
///   * any whitespace other than space, tab or newline anywhere in `cmd`
///     (`\r` or a no-break space changes where bash ends the delimiter word);
///   * a non-body line with unbalanced quotes, a trailing `\`, or (on a line
///     that opens a here-document) any of `\`, `#`, backtick, `$(`, `<(`,
///     `>(`, `((`, `[[` — constructs where the `<<` may be no opener at all
///     (a comment, an arithmetic shift, an escaped `\<<`) or where the body may
///     start elsewhere;
///   * a line opening more than one here-document, an empty delimiter, an
///     UNQUOTED delimiter, or a here-document never closed;
///   * a segment that straddles a body boundary or a segmentation that does not
///     reproduce `cmd` exactly (the segmenter lost sync with the shell, e.g. a
///     body apostrophe opened a quote that ran past the delimiter line).
pub(super) fn quoted_here_document_body_walks(cmd: &str) -> Vec<Option<usize>> {
    quoted_here_document_body_walks_checked(cmd).unwrap_or_default()
}

fn quoted_here_document_body_walks_checked(cmd: &str) -> Option<Vec<Option<usize>>> {
    if !cmd.contains("<<") {
        return None;
    }
    if cmd
        .chars()
        .any(|c| c.is_whitespace() && !matches!(c, ' ' | '\t' | '\n'))
    {
        return None;
    }
    // Byte spans [start, end) of each body: from the line after the opener's
    // line to the end of the closing delimiter line (newline excluded).
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut pending: Option<HereDocOpener> = None;
    let mut body_start = 0usize;
    let mut offset = 0usize;
    for raw in cmd.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line_start = offset;
        offset += raw.len();
        if let Some(opener) = &pending {
            let candidate = if opener.strips_tabs {
                line.trim_start_matches('\t')
            } else {
                line
            };
            if candidate == opener.delimiter {
                spans.push((body_start, line_start + line.len()));
                pending = None;
            }
            continue;
        }
        if !quotes_balance(line) || line.ends_with('\\') {
            return None;
        }
        let openers = here_document_openers(line);
        if openers.is_empty() {
            continue;
        }
        let doubtful = ["\\", "#", "`", "$(", "<(", ">(", "((", "[["];
        if doubtful.iter().any(|d| line.contains(d))
            || openers.len() != 1
            || openers[0].delimiter.is_empty()
            || !here_document_delimiter_is_quoted(line)
        {
            return None;
        }
        pending = openers.into_iter().next();
        body_start = offset;
    }
    if pending.is_some() || spans.is_empty() {
        return None;
    }
    let segs = split_segments_with_separators(cmd);
    let mut out = Vec::with_capacity(segs.len());
    let mut pos = 0usize;
    for seg in &segs {
        let (start, end) = (pos, pos + seg.text.len());
        if cmd.get(start..end) != Some(seg.text.as_str()) {
            return None;
        }
        let mut walk = None;
        for (k, &(s, e)) in spans.iter().enumerate() {
            if start >= s && end <= e {
                walk = Some(k);
            } else if start < e && end > s {
                return None;
            }
        }
        out.push(walk);
        pos = end
            + match seg.sep_after {
                SegmentSep::Conditional => 2,
                SegmentSep::EndOfLine => 0,
                _ => 1,
            };
    }
    if pos != cmd.len() {
        return None;
    }
    Some(out)
}

/// Per segment of [`split_segments_with_separators`]`(cmd)`: true when the
/// segment is a BODY line of a QUOTED-delimiter here-document whose opener
/// segment hands it to a non-shell code interpreter (`python3 - <<'PY'`).
///
/// Backlog 4a1cf1ad: the line-level redirect scan read such a body as shell,
/// so a `>` in a Python comment (`# writes \`<k(r)>/claims.json\``) was a
/// truncating redirect with an undeterminable target and was DENIED — before
/// [`interpreter_code_verdict`] could apply user ruling R2 (2026-10-14: an
/// undeterminable `>` in program text is an Ask carrying [`RECOMMEND_NO`],
/// never Deny and never Allow). With a quoted delimiter the shell does not
/// parse the body at all.
///
/// Restrictive on every doubt: an opener segment that opens more than one
/// here-document, an unquoted delimiter, or a command word that is not plainly
/// a code interpreter (a shell, `env`, `sudo`, an assignment, …) marks nothing.
pub(super) fn interpreter_program_body_segments(cmd: &str) -> Vec<bool> {
    program_body_segments_fed_to(cmd, |w| is_code_interpreter(w) && !is_shell(w))
}

/// [`interpreter_program_body_segments`] narrowed to PYTHON readers (`python`,
/// `python2`, `python3`, versioned spellings). Python has no syntax that runs
/// a shell command from a backtick (Python 2's backticks are `repr`), unlike
/// Perl, Ruby and PHP, where `` `cmd` `` IS a shell command substitution. So in
/// a Python program body a backtick-headed "command word" is never a command
/// word the shell runs (FP2, backlog ef015a66).
pub(super) fn python_program_body_segments(cmd: &str) -> Vec<bool> {
    program_body_segments_fed_to(cmd, |w| strip_version_suffix(w) == "python")
}

/// The shared scan of [`interpreter_program_body_segments`] and
/// [`python_program_body_segments`]; `reader` judges the basename of the
/// opener segment's first word.
pub(super) fn program_body_segments_fed_to(cmd: &str, reader: impl Fn(&str) -> bool) -> Vec<bool> {
    let segs = split_segments_with_separators(cmd);
    let mut out = vec![false; segs.len()];
    let mut idx = 0;
    while idx < segs.len() {
        let openers = here_document_openers(&segs[idx].text);
        if openers.is_empty() {
            idx += 1;
            continue;
        }
        // Only a NEWLINE ends the opener's line; anything after a `;`/`&&`/`|`
        // on that line is shell code, so such a line marks nothing.
        let program_text = openers.len() == 1
            && segs[idx].ends_at_newline
            && here_document_delimiter_is_quoted(&segs[idx].text)
            && segs[idx]
                .text
                .split_whitespace()
                .next()
                .is_some_and(|w| reader(basename(w)));
        let opener = &openers[0];
        let mut body = idx + 1;
        while body < segs.len() && !segment_closes_here_document(&segs[body].text, opener) {
            body += 1;
        }
        // Marked only when the body is CLOSED by its delimiter line and no
        // body segment spans a newline (an unbalanced quote in an
        // UNTERMINATED body, e.g. a Python `# don't`, still merges lines —
        // the splitters read a body line by line only once its delimiter
        // line is found, see `here_doc_body_spans`).
        let closed = body < segs.len();
        let clean = segs[idx + 1..body.min(segs.len())]
            .iter()
            .all(|s| !s.text.contains('\n'));
        if program_text && closed && clean {
            for flag in &mut out[idx + 1..body] {
                *flag = true;
            }
        }
        idx = body + 1;
    }
    out
}

/// The command with the BODY of its here-document removed, when — and only
/// when — that here-document is effectively the WHOLE command and its body is
/// fed to a data-only reader (backlog fa1fce21 / 7037df97). Otherwise `cmd` is
/// returned unchanged, so every rule judges the body as code, exactly as it
/// would any other line.
///
/// This is an ALLOWLIST of command shapes, not a list of dangers. Earlier
/// versions tried to name the ways a body could still run (a later `sh x.txt`,
/// a redefined `cat`) and each list was bypassed by a spelling it did not
/// model (`sh *.txt`, `sh x''.txt`, `eval 'ca''t() …'`, `. ./defs.sh`). The
/// shape rule needs no model of spelling: if anything else runs in the same
/// command, the body is not removed.
///
/// The body is removed only when ALL of these hold:
///   * every line BEFORE the opener is blank, a comment (`#` first), `set -e`,
///     `set -eu`, `set -euo pipefail`, `cd <literal path>`,
///     `mkdir -p <literal path>…` or `git add <literal path>…`. A literal path is letters, digits and
///     `/ . _ - + , @ % : =`, with `~` allowed only as its first character;
///     a `git add` path may also be one wholly quoted word the shell expands
///     nothing in (see [`permitted_prefix_line`]).
///     Anything else — an assignment, `eval`, `source`, `.`, a function, an
///     alias, `PATH`, any other command — keeps the body;
///   * the opener line is one physical line with balanced quotes, no `$`, no
///     backtick, a single here-document with a plain delimiter
///     ([`plain_here_document_delimiter`]) and a data-only consumer
///     ([`here_document_consumer_is_data`]: `cat`, `tee` to data files,
///     `git commit -F -`, with output redirects only to data files). The opener
///     line may begin with `cd <literal path> && ` and/or
///     `git add <literal path>… && ` heads ([`opener_consumer`]), the
///     same-line spelling of a permitted `cd` / `git add` line before it; the
///     consumer check then reads the part after the last such `&&`;
///   * the body is CLOSED by an exact delimiter line
///     ([`segment_closes_here_document`]);
///   * the delimiter is quoted, OR the body contains no `$`, backtick or
///     backslash (with an unquoted delimiter the shell expands those);
///   * every line AFTER the closing delimiter is blank or a comment. A later
///     line could reach the written file by any spelling, so no later command
///     is allowed at all;
///   * no line outside the body contains whitespace other than space, tab and
///     newline. A carriage return in particular: bash keeps a `\r` in the
///     delimiter word, so `<<'EOF'\r` is closed only by a line `EOF\r`, while
///     [`here_document_openers`] ends the word at it.
///
/// What is kept when the body is removed: the opener line and the closing
/// delimiter line, plus the permitted lines around them. So the opener's
/// redirect targets are still judged, with the `cd` in force.
///
/// OVER-BLOCKS, accepted: anything after the close (`…EOF` then
/// `git add notes.md`) or before the opener (`X=1`) makes the body judged as
/// code, so a body that merely mentions a destructive command is denied or
/// asked there. That is the restrictive direction.
///
/// RESIDUALS, not covered:
///   * a data file written here can be executed by a LATER, separate tool
///     call. That call is judged on its own text, and this gate never re-reads
///     the file;
///   * the Bash tool starts every call in a fresh shell, so a function or alias
///     from an earlier call does not persist, but that shell IS initialised
///     from the user's profile. A profile that redefines `cat`, `tee` or `git`
///     is trusted, and out of scope here.
pub(super) fn strip_data_here_document_bodies(cmd: &str) -> std::borrow::Cow<'_, str> {
    strip_here_document_body(cmd, OutputTargets::DataFilesOnly)
}

/// Which output targets a here-document opener may write the body to for
/// [`strip_here_document_body`] to remove that body.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum OutputTargets {
    /// Only DATA files or `/dev/null` ([`is_data_file_target`]). The removal
    /// then applies before EVERY rule ([`strip_data_here_document_bodies`]).
    DataFilesOnly,
    /// Any literal target. Used ONLY for the redirect view of the line-level
    /// `>` / `>>` scans ([`redirect_view_without_quoted_here_document_body`]);
    /// every other rule still reads the body.
    AnyTarget,
}

/// The text the line-level truncating (`>`) and append (`>>`) redirect scans
/// of [`detect_bash`] read (backlog 1fd5a731): `cmd` with the body of a
/// QUOTED-delimiter here-document removed when that here-document is
/// effectively the whole command and its reader only stores or prints it.
///
/// The bash manual: with a quoted delimiter "the lines in the here-document
/// are not expanded", and the body is the reader's stdin, so a `>` in it is
/// never a redirect. Without this view `cat > src/x.rs <<'EOF'` whose body held
/// the Rust doc comment `/// corrupt => \`Err\`` was denied as the redirect
/// ``> `Err` ``.
///
/// The shape rule is [`strip_data_here_document_bodies`]'s (permitted
/// lines before the opener, one opener line, a reader on the closed
/// `cat`/`tee`/`git commit -F -` list, a CLOSED body, only blank or comment
/// lines after it), where the opener-line conditions (no `$`/backtick, no `|`,
/// `;` or `&`) apply to the consumer text, with four differences:
///   * the delimiter MUST be quoted (`'EOF'`, `"EOF"`, `\EOF`). An unquoted
///     one lets the shell expand `$(…)` and backticks in the body, so the body
///     stays in the view;
///   * the opener's output redirect may name ANY literal target, not only a
///     data file. Its target is still judged: the opener line stays in the
///     view, so `> src/x.rs` goes through every redirect axis as before;
///   * when the reader stores the body in no file
///     ([`here_document_reader_stores_nothing`]: bare `cat`, `git commit -F -`,
///     no output redirect), ANY lines may follow the closing delimiter. They
///     stay in the view and are judged like every other line;
///   * the consumer text is [`redirect_view_consumer`]'s: besides the heads
///     [`opener_consumer`] splits, further `cd <one word>` heads split at `;`
///     or `&&` are peeled, even with an unresolvable operand like `"$WT"`. The
///     `$`/backtick check then applies only to the remaining reader text, not
///     to the whole opener line. The `cd` heads stay in the view and are judged
///     like every other line; a head that is not `cd <word>`, or whose operand
///     holds a backtick or `$(`, is not peeled and the line stays refused.
///
/// Every other rule (the per-segment loop, the interpreter and egress scans)
/// still reads the full text, body included.
pub(super) fn redirect_view_without_quoted_here_document_body(
    cmd: &str,
) -> std::borrow::Cow<'_, str> {
    strip_here_document_body(cmd, OutputTargets::AnyTarget)
}

/// The shared shape rule of [`strip_data_here_document_bodies`] and
/// [`redirect_view_without_quoted_here_document_body`]; see those for which
/// `targets` each passes and what that changes.
pub(super) fn strip_here_document_body(
    cmd: &str,
    targets: OutputTargets,
) -> std::borrow::Cow<'_, str> {
    let unchanged = std::borrow::Cow::Borrowed(cmd);
    if !cmd.contains("<<") {
        return unchanged;
    }
    let lines: Vec<&str> = cmd.split_inclusive('\n').collect();
    let text = |l: &str| l.strip_suffix('\n').unwrap_or(l).to_string();
    let odd_whitespace = |l: &str| {
        l.chars()
            .any(|c| c.is_whitespace() && !matches!(c, ' ' | '\t' | '\n'))
    };
    let inert = |l: &str| {
        let t = l.trim_matches([' ', '\t']);
        t.is_empty() || t.starts_with('#')
    };
    // Lines before the opener.
    let mut idx = 0;
    while idx < lines.len() {
        let l = text(lines[idx]);
        if odd_whitespace(&l) {
            return unchanged;
        }
        if !(inert(&l) || permitted_prefix_line(&l)) {
            break;
        }
        idx += 1;
    }
    let Some(opener_line) = lines.get(idx).map(|l| text(l)) else {
        return unchanged;
    };
    let openers = here_document_openers(&opener_line);
    // The redirect view removes a body only when the shell cannot expand it.
    let redirect_view_of_quoted =
        targets == OutputTargets::AnyTarget && here_document_delimiter_is_quoted(&opener_line);
    if targets == OutputTargets::AnyTarget && !redirect_view_of_quoted {
        return unchanged;
    }
    // Quoted delimiter, redirect view: further `cd <word>` heads (split at `;`
    // or `&&`) are peeled too, even with an unresolvable operand like "$WT".
    // They run BEFORE the reader and never see the body, and they stay in the
    // command text so every rule still judges them; `$` / backtick are then
    // refused only in the remaining reader text. [`opener_consumer`]'s own
    // contract is unchanged.
    let consumer = if redirect_view_of_quoted {
        redirect_view_consumer(&opener_line)
    } else {
        opener_consumer(&opener_line)
    };
    let expansion_free = if redirect_view_of_quoted {
        !consumer.contains(['$', '`'])
    } else {
        !opener_line.contains(['$', '`'])
    };
    if openers.len() != 1
        || openers[0].delimiter.is_empty()
        || !expansion_free
        || !quotes_balance(&opener_line)
        || !here_document_consumer_is_data(consumer, targets)
    {
        return unchanged;
    }
    // The body, up to the first exact delimiter line.
    let Some(close) = (idx + 1..lines.len())
        .find(|&j| segment_closes_here_document(&text(lines[j]), &openers[0]))
    else {
        return unchanged;
    };
    let body: String = lines[idx + 1..close].concat();
    if !here_document_delimiter_is_quoted(&opener_line) && body.contains(['$', '`', '\\']) {
        return unchanged;
    }
    // Lines after the close. The redirect view may keep LATER commands when the
    // reader stores the body nowhere a later command could run it from
    // ([`here_document_reader_stores_nothing`]): those lines stay in the view
    // and every redirect rule still judges them; only the body leaves.
    let later_commands_ok =
        targets == OutputTargets::AnyTarget && here_document_reader_stores_nothing(consumer);
    if lines[close + 1..]
        .iter()
        .any(|l| odd_whitespace(&text(l)) || !(later_commands_ok || inert(&text(l))))
    {
        return unchanged;
    }
    let mut out: String = lines[..=idx].concat();
    out.push_str(&lines[close..].concat());
    std::borrow::Cow::Owned(out)
}

/// The part of the opener line [`here_document_consumer_is_data`] judges: the
/// whole line, or — when the line is `cd <literal path> && <rest>` and the
/// `cd <literal path>` part is accepted by [`permitted_prefix_line`] —
/// `<rest>`. `cd X && cat <<'EOF'` is the same-line spelling of a permitted
/// `cd X` line before the opener (and a stricter one: a failed `cd` skips the
/// reader instead of running it in the old directory), so its body is data
/// exactly when the body of `cat <<'EOF'` is. Only `cd` and `git add` heads
/// (below) are split; any other prefix stays in the judged text, which the
/// consumer check then refuses
/// (`&` is shell syntax there). The opener line itself — `cd` included — stays
/// in the command, so the cwd walk and every redirect rule still see it.
///
/// A `git add <literal path>… && ` head is split the same way (backlog
/// c5b9c02b: `git add a.txt && git commit -F - <<'EOF'` had its quoted body
/// read as a redirect, ``> `.` `` in the commit message, and was DENIED). It is
/// the same-line spelling of a permitted `git add` line before the opener:
/// `git add` runs BEFORE the reader and never sees the body, so it cannot make
/// the body code. Heads chain (`cd X && git add a && git commit -F - <<'EOF'`);
/// the first head that is neither a permitted `cd` nor a permitted `git add`
/// stops the split, and the text from that head on is what gets judged (so the
/// consumer check sees its `&&` and refuses). Only `&&` is split: `;`, `||`,
/// `|` and `&` stay inside the judged text, where the consumer check refuses
/// them.
pub(super) fn opener_consumer(line: &str) -> &str {
    let mut rest = line;
    while let Some((head, tail)) = rest.split_once("&&") {
        let words: Vec<&str> = head.split([' ', '\t']).filter(|w| !w.is_empty()).collect();
        let splittable_head = matches!(words.as_slice(), ["cd", ..] | ["git", "add", ..])
            && permitted_prefix_line(head);
        if !splittable_head {
            break;
        }
        rest = tail;
    }
    rest
}

/// [`opener_consumer`], then further `cd <one word>` heads split at `&&` or
/// `;` are peeled, for the redirect view of a QUOTED-delimiter here-document
/// only (backlog 9f90b96f: `cd /tmp/wt; git commit -F- <<'MSG'`; backlog
/// 5651745b: `cd "$WT" && cat <<'EOF'`). A `cd` runs before the reader and never
/// sees the body, so it cannot make the body code; its operand may be
/// unresolvable (`"$WT"`) because the opener line, `cd` included, stays in the
/// command and the cwd walk and every redirect rule still judge it. A head
/// whose operand holds a backtick or `$(`, or that is anything but `cd <word>`,
/// stops the peeling; the remaining text is then judged (and refused) as before.
pub(super) fn redirect_view_consumer(line: &str) -> &str {
    let mut rest = opener_consumer(line);
    loop {
        let cut = [
            rest.find("&&").map(|i| (i, 2)),
            rest.find(';').map(|i| (i, 1)),
        ]
        .into_iter()
        .flatten()
        .min();
        let Some((i, len)) = cut else { return rest };
        let (head, tail) = (&rest[..i], &rest[i + len..]);
        let words = quote_aware_words(head);
        let peelable = matches!(words.as_slice(), [cd, _] if cd == "cd")
            && !head.contains(['`', '<', '>', '|', '&', '(', ')', '\\'])
            && !head.contains("$(")
            && quotes_balance(head);
        if !peelable {
            return rest;
        }
        rest = tail;
    }
}

/// True when `consumer` (the opener-line text [`opener_consumer`] returns, which
/// [`here_document_consumer_is_data`] has already accepted) hands the body to a
/// reader that leaves it in NO file: a bare `cat` with no output redirect (the
/// body goes to the tool's stdout) or `git commit -F -` with no output redirect
/// (the body becomes a commit message).
///
/// Only for the redirect view ([`OutputTargets::AnyTarget`]), where it lets
/// commands follow the closing delimiter line (FP1, backlog e0085090:
/// `git commit -F - <<'EOF'` … `EOF` then `git log -1` had a message line ending
/// in `>` read as the redirect `> EOF`). The reason the shape rule otherwise
/// forbids later commands is that one of them could execute a FILE the body was
/// written to; with no file there is nothing for it to execute. `tee` writes
/// files, and any output redirect names one, so both answer false.
pub(super) fn here_document_reader_stores_nothing(consumer: &str) -> bool {
    let words = quote_aware_words(consumer);
    if words
        .iter()
        .any(|w| strip_output_redirect_operator(w).is_some() || w.contains('>'))
    {
        return false;
    }
    matches!(words.first().map(String::as_str), Some("cat" | "git"))
}

/// True when `line` has no unterminated `'` or `"` quote.
pub(super) fn quotes_balance(line: &str) -> bool {
    let (mut in_s, mut in_d) = (false, false);
    for c in line.chars() {
        match c {
            '\'' if !in_d => in_s = !in_s,
            '"' if !in_s => in_d = !in_d,
            _ => {}
        }
    }
    !in_s && !in_d
}

/// The lines [`strip_data_here_document_bodies`] permits BEFORE the opener:
/// `set -e`, `set -eu`, `set -euo pipefail`, `cd <literal path>`,
/// `mkdir -p <literal path>…`, and `git add <literal path>…` (stages files; it
/// runs before the reader and never reads the body). Nothing else: a `git`
/// with global options (`git -c …`) is not `git add` here.
///
/// A `git add` path word may also be ONE wholly quoted word (FP1, backlog
/// e0085090: `git add 'a b.rs' && git commit -F - <<'EOF'`): `'…'` holding no
/// `'` or newline, or `"…"` holding none of `"`, `$`, backtick, `\`, `!` or
/// newline. The shell expands nothing in either, and `git add` only stages
/// files, so the word is as harmless as an unquoted one. Words are split
/// quote-aware ([`quote_aware_words`]); any other quoting (a quote glued to
/// unquoted text, an unbalanced quote) is not literal. `cd` and `mkdir -p`
/// keep the unquoted-only rule: a `cd` operand also moves the cwd every later
/// rule resolves against, and `mutkill_opener_consumer_splits_only_a_permitted_cd_head`
/// pins `cd "/x/y"` as not permitted.
pub(super) fn permitted_prefix_line(line: &str) -> bool {
    let quoted_literal = |w: &str| {
        let quoted = |q: char, forbidden: &[char]| {
            w.len() >= 2
                && w.starts_with(q)
                && w.ends_with(q)
                && !w[1..w.len() - 1].contains(forbidden)
        };
        quoted('\'', &['\'', '\n']) || quoted('"', &['"', '$', '`', '\\', '!', '\n'])
    };
    let literal = |w: &str| {
        let rest = w.strip_prefix('~').unwrap_or(w);
        !w.is_empty()
            && rest.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, '/' | '.' | '_' | '-' | '+' | ',' | '@' | '%' | ':' | '=')
            })
    };
    let owned = quote_aware_words(line);
    let words: Vec<&str> = owned.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["set", "-e"] | ["set", "-eu"] | ["set", "-euo", "pipefail"] => true,
        ["cd", path] => literal(path),
        ["mkdir", "-p", paths @ ..] => !paths.is_empty() && paths.iter().all(|p| literal(p)),
        ["git", "add", paths @ ..] => {
            !paths.is_empty() && paths.iter().all(|p| literal(p) || quoted_literal(p))
        }
        _ => false,
    }
}

/// File extensions treated as DATA for a here-document body: prose and
/// tabular text that no tool commonly loads as configuration or code. Formats
/// that DO commonly hold executable configuration (`json`, `toml`, `yaml`,
/// `yml`, `xml`/plist, `patch`/`diff`, which `git apply` turns into code) are
/// deliberately absent, as are `.sh`, `.py` and extension-less names (the
/// 6cf12ce9 ruling `cat > run.sh <<'EOF'`): a body headed there stays judged.
///
/// This is a statement about the NAME only. Any file can be executed
/// (`sh notes.txt`). Within one command that is covered by the shape rule in
/// [`strip_data_here_document_bodies`], which allows no command after the
/// here-document at all; a LATER, separate command that executes the file is
/// NOT covered and is a known residual of treating these bodies as data.
pub(super) const DATA_FILE_EXTENSIONS: &[&str] =
    &["txt", "md", "markdown", "rst", "adoc", "csv", "tsv", "log"];

/// `target` with one pair of matching surrounding quotes removed.
pub(super) fn unquote_data_file_target(target: &str) -> &str {
    match target.as_bytes() {
        [q @ (b'\'' | b'"'), .., last] if q == last && target.len() >= 2 => {
            &target[1..target.len() - 1]
        }
        _ => target,
    }
}

/// True when `target` (one shell word, quotes still on) names a file whose
/// content is data: `/dev/null`, or a literal path with an extension on
/// [`DATA_FILE_EXTENSIONS`]. Any expansion, glob, escape or unbalanced quote
/// answers false.
pub(super) fn is_data_file_target(target: &str) -> bool {
    let inner = unquote_data_file_target(target);
    if inner.is_empty()
        || inner.contains([
            '$', '`', '\\', '\'', '"', '*', '?', '[', ']', '{', '}', '(', ')', '<', '>', '|', ';',
            '&',
        ])
    {
        return false;
    }
    if inner == "/dev/null" {
        return true;
    }
    std::path::Path::new(inner)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            DATA_FILE_EXTENSIONS
                .iter()
                .any(|d| d.eq_ignore_ascii_case(e))
        })
}

/// True when a here-document delimiter is spelled as a plain word: `TAG`,
/// `'TAG'`, `"TAG"` or `\TAG`, where TAG is letters, digits, `_`, `.`, `-`.
/// Anything else (a glued `|sh`, `;`, a `$`) answers false, so an operator
/// cannot hide inside the delimiter word.
pub(super) fn plain_here_document_delimiter(spelling: &str) -> bool {
    let word = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    if let Some(rest) = spelling.strip_prefix('\\') {
        return word(rest);
    }
    for q in ['\'', '"'] {
        if let Some(inner) = spelling.strip_prefix(q).and_then(|s| s.strip_suffix(q)) {
            return word(inner);
        }
    }
    word(spelling)
}

/// The output-redirect operator a word starts with, and the text glued after it
/// (empty when the target is the next word). Only `>`, `>>`, `1>`, `1>>`,
/// `2>`, `2>>`; `>&`, `>|`, `&>` and input redirects are not recognised, so a
/// word carrying them falls through to the shell-syntax refusal.
pub(super) fn strip_output_redirect_operator(word: &str) -> Option<&str> {
    for op in ["1>>", "2>>", ">>", "1>", "2>", ">"] {
        if let Some(rest) = word.strip_prefix(op) {
            return Some(rest);
        }
    }
    None
}

/// True when the opener line `line` hands its one here-document body to a
/// program on a CLOSED list of readers that only store or print it:
///   * a bare `cat` (or `cat -`) with no file operand. It prints the body, or
///     writes it to its output redirect;
///   * `tee`, whose only operands are `-a`/`--append` and DATA files
///     ([`is_data_file_target`]);
///   * `git commit -F -` / `-F-` / `--file=-` / `--file -`, with git's global
///     options allowed before `commit`. git records the text as the commit
///     message and runs none of it.
///
/// Output redirects (`> f`, `>> f`, `2> f`, glued or not) are allowed only to a
/// DATA file or `/dev/null`: a body written to `run.sh` may be executed later,
/// so it stays judged. The redirect itself is NOT exempted from anything — the
/// opener line stays in the command, so every redirect rule still judges its
/// target.
///
/// The program word must be spelled exactly `cat`, `tee` or `git`, with no
/// path and no leading assignment or wrapper. Any other word carrying shell
/// syntax disqualifies the line: `<`/`>` outside a recognised output redirect,
/// a backslash, a backtick, `(`/`)`, `;`, `&`, `|`. So `cat <<'EOF' | sh`,
/// `cat <<'EOF'; x` and `cat <<'EOF' && x` are never data.
///
/// Every other reader, known or not (bash, sh, zsh, python3, node, perl,
/// eval, xargs, …), answers false. A false here only means the body is judged
/// as code, as it always was.
///
/// `targets` picks the output-redirect rule above. [`OutputTargets::DataFilesOnly`]
/// is the rule as stated (what [`strip_data_here_document_bodies`] passes).
/// [`OutputTargets::AnyTarget`] (only from
/// [`redirect_view_without_quoted_here_document_body`]) lets a redirect name
/// any target word free of shell syntax (`\`, backtick, `(`, `)`, `;`, `&`,
/// `|`, `<`, `>`, `$`). Everything else, including the `tee` operand rule
/// (DATA files only), is the same for both.
pub(super) fn here_document_consumer_is_data(line: &str, targets: OutputTargets) -> bool {
    let raw = quote_aware_words(line);
    let mut words: Vec<&str> = Vec::new();
    let mut saw_here_document = false;
    let mut i = 0;
    while i < raw.len() {
        let w = raw[i].as_str();
        if let Some(rest) = w.strip_prefix("<<") {
            if rest.starts_with('<') || saw_here_document {
                return false;
            }
            let rest = rest.strip_prefix('-').unwrap_or(rest);
            let spelling = if rest.is_empty() {
                i += 1;
                match raw.get(i) {
                    Some(w) => w.as_str(),
                    None => return false,
                }
            } else {
                rest
            };
            if !plain_here_document_delimiter(spelling) {
                return false;
            }
            saw_here_document = true;
            i += 1;
            continue;
        }
        if let Some(glued) = strip_output_redirect_operator(w) {
            let target = if glued.is_empty() {
                i += 1;
                match raw.get(i) {
                    Some(w) => w.as_str(),
                    None => return false,
                }
            } else {
                glued
            };
            let target_ok = match targets {
                OutputTargets::DataFilesOnly => is_data_file_target(target),
                OutputTargets::AnyTarget => {
                    !target.is_empty()
                        && !target.contains(['\\', '`', '(', ')', ';', '&', '|', '<', '>', '$'])
                }
            };
            if !target_ok {
                return false;
            }
            i += 1;
            continue;
        }
        words.push(w);
        i += 1;
    }
    if !saw_here_document
        || words
            .iter()
            .any(|w| w.contains(['<', '>', '\\', '`', '(', ')', ';', '&', '|']))
    {
        return false;
    }
    match words.first().copied() {
        Some("cat") => words[1..].iter().all(|w| *w == "-"),
        Some("tee") => words[1..]
            .iter()
            .all(|w| matches!(*w, "-a" | "--append") || is_data_file_target(w)),
        Some("git") => {
            let rest = &words[1..];
            let Some(sub) = git_subcommand_index(rest) else {
                return false;
            };
            if rest[sub] != "commit" {
                return false;
            }
            let args = &rest[sub + 1..];
            args.iter().enumerate().any(|(j, a)| match *a {
                "-F-" | "--file=-" => true,
                "-F" | "--file" => args.get(j + 1) == Some(&"-"),
                _ => false,
            })
        }
        _ => false,
    }
}

/// Backlog 9f90b96f / 5651745b: the redirect view peels `cd <word>` heads of a
/// QUOTED-delimiter here-document, and nothing else.
#[cfg(test)]
mod peel_tests {
    use super::*;

    #[test]
    fn redirect_view_peels_cd_heads_only_for_quoted_data_here_documents() {
        let view = redirect_view_without_quoted_here_document_body;
        let stripped = |c: &str| view(c).len() < c.len();
        // Quoted delimiter, data reader, cd heads (`;`, `&&`, unresolved operand).
        assert!(stripped("cd /tmp/wt; git commit -F- <<'MSG'\nA <a@b>\nMSG"));
        assert!(stripped("cd \"$WT\" && cat <<'EOF'\nv > {bar}\nEOF"));
        // Unquoted delimiter: the shell expands the body, so it stays in the view.
        assert!(!stripped("cd /tmp/wt; git commit -F- <<MSG\nA <a@b>\nMSG"));
        assert!(!stripped("cd \"$WT\" && cat <<EOF\nv > {bar}\nEOF"));
        // A reader that executes its stdin keeps the body.
        assert!(!stripped("cd /tmp/wt; sh <<'EOF'\necho > x\nEOF"));
        assert!(!stripped("cd \"$WT\" && bash <<'EOF'\necho > x\nEOF"));
        // A non-cd head, or a cd whose operand substitutes a command, is not peeled.
        assert!(!stripped("rm x; cat <<'EOF'\nv > {bar}\nEOF"));
        assert!(!stripped("cd $(evil) && cat <<'EOF'\nv > {bar}\nEOF"));
    }
}
