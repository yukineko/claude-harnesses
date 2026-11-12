use super::{
    analyze_shell_payload, confined_ask, glob_literal_prefix, has_glob_meta, is_redirect_token,
    is_short_flag, positional_operands, recommend_against, Ctx, Decision,
};
use crate::exclude;

mod comments;
mod inert_literal;
mod invocation;
mod osascript;
mod python_file;
mod python_semantics;
mod shell_calls;
#[cfg(test)]
mod unit_tests;
use comments::strip_hash_comments;
use inert_literal::{has_dynamic_code_execution, inert_literal_bodies};
pub(super) use invocation::analyze_code_interpreter;
use osascript::osascript_app_delete;
pub(super) use osascript::osascript_program_delete;
pub(super) use python_file::{
    python_file_body_verdict, python_redirect_view, python_stdin_body_reading,
    python_written_program_verdict,
};
pub(super) use shell_calls::python_body_segment_verdict;

/// Non-shell interpreters that run an inline program supplied as a string
/// argument (e.g. `python3 -c "…"`, `perl -e "…"`). Like a shell's `-c`, this
/// lets `find -exec <interp> -c "<payload>"` run an arbitrary destructive
/// command per match, slipping past the literal `rm` token scan.
///
/// CA-blastguard-008: recognizes versioned basenames too (e.g. `python3.12`,
/// `perl5.36`) by stripping a trailing `\d+(\.\d+)*` version suffix from the
/// token before matching against the known unversioned stems. Without this, a
/// versioned interpreter invocation (`find -exec python3.12 -c "…" \;`) slips
/// past the check entirely.
///
/// daf7611b: `deno` (`deno eval …`), `bun` (`bun -e …`) and `osascript`
/// (`osascript -e '…'`, whose `do shell script "…"` hands a string to `sh`)
/// run an inline program exactly like `node -e`, and matched no arm at all —
/// `deno eval "Deno.removeSync(…, {recursive: true})"` was Allow. They are
/// judged by the same program audit ([`analyze_code_interpreter`]).
pub(super) fn is_code_interpreter(cmd: &str) -> bool {
    matches!(
        strip_version_suffix(cmd),
        "python"
            | "python2"
            | "python3"
            | "perl"
            | "ruby"
            | "node"
            | "nodejs"
            | "php"
            | "lua"
            | "deno"
            | "bun"
            | "osascript"
    )
}

/// Strip a trailing version suffix (`\d+(\.\d+)*`, e.g. `3.12`, `5`, `2.7`)
/// from a command basename, returning the bare stem (`python3.12` -> `python`,
/// `perl5.36` -> `perl`). If there is no trailing digit run, returns `cmd`
/// unchanged.
pub(super) fn strip_version_suffix(cmd: &str) -> &str {
    let bytes = cmd.as_bytes();
    let mut end = bytes.len();
    loop {
        // Find the start of a trailing digit run.
        let mut start = end;
        while start > 0 && bytes[start - 1].is_ascii_digit() {
            start -= 1;
        }
        if start == end {
            // No trailing digits at `end` — nothing more to strip.
            break;
        }
        end = start;
        // A `.` immediately before a digit run continues the version suffix
        // (e.g. the `.12` in `python3.12`) — strip it too and keep looking.
        if end > 0 && bytes[end - 1] == b'.' {
            end -= 1;
        } else {
            break;
        }
    }
    &cmd[..end]
}

/// The inline-eval flag letters that belong to THIS interpreter.
///
/// The union scan `c|e|r|p` this replaces was a shape match, not an audit: it
/// fired on flags the interpreter never sees. Measured before the fix —
/// `python3 -m pip install -r req.txt` and `python3 manage.py -p 8000` were
/// both `Deny`, because `-r` is pip's flag and `-p` is the app's. It also fired
/// on the `-rf` sitting inside an unrelated `rm -rf` payload, which made a
/// here-string test pass for a reason that had nothing to do with here-strings.
/// Asking each interpreter about its own spelling is the audit.
fn interpreter_eval_flag_letters(cmd: &str) -> &'static str {
    match strip_version_suffix(cmd) {
        "python" => "c",
        // Perl's `-e`/`-E` supply the program. `-n`/`-p` only WRAP one, so they
        // are an eval position solely in a bundle that also carries `e`/`E`.
        "perl" => "eE",
        "ruby" => "e",
        // Node's `-p` is `--print`, which evaluates its argument.
        "node" | "nodejs" => "ep",
        "php" => "r",
        "lua" => "e",
        // Bun: `-e`/`--eval`, and `-p`/`--print` evaluates too.
        "bun" => "ep",
        "osascript" => "e",
        // Deno's inline eval is the `eval` SUBCOMMAND (and `repl --eval`), not
        // a short flag; see `interpreter_inline_eval_pos`.
        _ => "",
    }
}

/// Options of THIS interpreter that take their value as the NEXT token.
///
/// The option scan in [`interpreter_inline_eval_pos`] stops at the first
/// non-flag token, reading it as the script path. A separate-form option
/// value (`osascript -l JavaScript -e …`, `python3 -W ignore -c …`,
/// `node -r mod -e …`) is not a script path; stopping there hid the eval flag
/// behind it and the program went unread. Skipping a value can only extend
/// the scan, so a miss in this list is the old behaviour, never a new Allow.
fn interpreter_value_flags(cmd: &str) -> &'static [&'static str] {
    match strip_version_suffix(cmd) {
        "python" | "python2" | "python3" => &["-W", "-X", "-Q"],
        "node" | "nodejs" => &[
            "-r",
            "--require",
            "--import",
            "--loader",
            "--experimental-loader",
            "--input-type",
            "-C",
            "--conditions",
        ],
        "bun" => &["-r", "--preload", "--cwd", "--config"],
        "ruby" => &["-I", "-r", "-C", "-E"],
        "perl" => &["-I"],
        "php" => &["-c", "-d", "-z"],
        "lua" => &["-l"],
        "osascript" => &["-l", "-s"],
        _ => &[],
    }
}

/// True if `tok` spells an inline-eval flag for an interpreter whose own
/// eval letters are `letters` (bundled short flags included: `-ic` is `-i`+`-c`).
fn is_inline_eval_flag_for(tok: &str, letters: &str) -> bool {
    if matches!(tok, "--eval" | "--print")
        || tok.starts_with("--eval=")
        || tok.starts_with("--print=")
    {
        return true;
    }
    if is_short_flag(tok) && tok.len() > 1 {
        return tok[1..].chars().any(|c| letters.contains(c));
    }
    false
}

/// Position in `rest` of the interpreter's OWN inline-eval flag, if any.
///
/// Scans only the interpreter's own options — the tokens before the first
/// operand. `-m <module>`, a bare `-` (read the program from stdin), `--`, and
/// the first non-flag token (a script path) each hand the remaining argv to a
/// CHILD program, whose flags say nothing about how the interpreter itself was
/// invoked. Scanning all of `rest` was the defect; see
/// [`interpreter_eval_flag_letters`] for the measured false Denies.
fn interpreter_inline_eval_pos(cmd: &str, rest: &[&str]) -> Option<usize> {
    let letters = interpreter_eval_flag_letters(cmd);
    // Deno has no eval LETTER: its inline program follows the `eval`
    // subcommand, or `repl --eval`.
    let deno = strip_version_suffix(cmd) == "deno";
    if letters.is_empty() && !deno {
        return None;
    }
    let value_flags = interpreter_value_flags(cmd);
    let mut i = 0;
    while i < rest.len() {
        let tok = rest[i];
        if is_redirect_token(tok) {
            i += 1;
            continue;
        }
        if is_inline_eval_flag_for(tok, letters) {
            return Some(i);
        }
        if tok == "--" || tok == "-" {
            return None;
        }
        if value_flags.contains(&tok) {
            i += 2;
            continue;
        }
        if tok.starts_with("--") {
            i += 1;
            continue;
        }
        if is_short_flag(tok) {
            // `-m` takes a module name; everything after it is that module's
            // own argv. (Deno and osascript have no `-m`.)
            if tok[1..].contains('m') && !deno {
                return None;
            }
            i += 1;
            continue;
        }
        if deno {
            match tok {
                // `deno eval <code>`: the operand after the subcommand is the
                // program.
                "eval" => return Some(i),
                // `deno repl --eval <code>`: keep scanning its options.
                "repl" => {
                    i += 1;
                    continue;
                }
                _ => return None,
            }
        }
        // First non-flag token: the script path. The interpreter's own options
        // are over.
        return None;
    }
    None
}

/// Interpreter-language destructive calls, which have no shell spelling and so
/// are invisible to [`analyze_shell_payload`]. `shutil.rmtree('/')` deletes a
/// tree without ever naming `rm`.
/// Bare-builtin spellings are listed WITH their following punctuation (`unlink `,
/// `unlink(`) so an identifier that merely contains the word does not match.
/// Perl's `unlink glob "*"` has no dot-qualified form at all, which is why a
/// list of only `os.`/`shutil.`/`fs.` methods left it unjudged.
const INTERPRETER_DESTRUCTIVE_CALLS: &[&str] = &[
    "rmtree",
    "rm_rf",
    "remove_entry",
    "removedirs",
    "os.remove",
    // Node's fs API is usually reached through `require('fs').rmSync(…)`, so
    // the receiver is not spelled `fs.` at all — match the distinctive method
    // names on their own.
    "rmsync",
    "unlinksync",
    // Deno's `Deno.remove(path, {recursive: true})` / `Deno.removeSync`.
    "deno.remove",
    "rmdirsync",
    "fs.rm(",
    "file.delete",
    "dir.delete",
    // Bare builtins must be followed by something that OPENS AN ARGUMENT.
    // `"unlink "` on its own matched the English sentence
    // `echo 'please unlink (later)' | python3`, and the gate then named a
    // finding it had not observed — the reason claimed the program deletes
    // files when the program was prose. A reason that asserts an unobserved
    // finding is the §4 failure aimed at the human reading it.
    "unlink(",
    "unlink;",
    "unlink glob",
    "unlink $",
    "unlink @",
    "unlink \"",
    "unlink '",
    "rmdir(",
    "rmdir $",
    "rmdir \"",
    "rmdir '",
    // Ruby's receiver-qualified form. `unlink(`/`unlink "` do not match
    // `File.unlink f`, where the argument is a bare block variable — measured
    // reaching Allow on 2026-09-09 as
    // `Dir[…].each { |f| File.unlink f }` while its `FileUtils.rm_rf` twin
    // denied. Same hazard, different spelling.
    "file.unlink",
    "file.delete",
];

/// Interpreter-level calls that OVERWRITE or TRUNCATE a file the program NAMES.
///
/// Kept apart from [`INTERPRETER_DESTRUCTIVE_CALLS`] because these are not
/// destructive by themselves. Writing a file is exactly the thing the operator
/// ruled must NOT be surfaced (「よみかきに一々許可をもとめるのは…無駄」) — what
/// decides is the OBJECT, not the verb: overwriting a tracked, clean file costs
/// a `git restore`, and overwriting an Obsidian note costs the note. So a hit
/// here does not decide anything on its own; it only turns on the recoverability
/// probe over the same payload's string literals.
///
/// That is why this list being short is not the v0.2.57 mistake. A miss leaves
/// the payload exactly where it was — read, descended, and judged by every other
/// rule — rather than allowing it.
const INTERPRETER_WRITE_CALLS: &[&str] = &[
    "os.truncate(",
    ".truncate(",
    "ftruncate(",
    "copyfile(",
    "copyfileobj(",
    "shutil.copy2(",
    "o_wronly",
    "o_rdwr",
    "o_trunc",
    "o_creat",
    "writefilesync(",
    "createwritestream(",
    "file.write(",
    // `open(path, 'w')` and friends. The mode operand is matched WITH its
    // quotes and closing paren so an identifier ending in `w` cannot match.
    ",'w')",
    ", 'w')",
    ",\"w\")",
    ", \"w\")",
    ",'wb')",
    ", 'wb')",
    ",\"wb\")",
    ", \"wb\")",
    ",'w+')",
    ", 'w+')",
    ",'wb+')",
    ", 'wb+')",
    ",'w'",
    ",'wb'",
];

/// True when `candidate` names an EXISTING DIRECTORY and nothing in `payload`
/// can write into a directory operand — so the literal cannot be what the
/// program's write call overwrites.
///
/// Backlog b85f6d5d: `l.startswith('//')` beside `open(p,'w')` was denied as
/// "it writes `//`", because `//` is `/` to the probe and `/` is in no work
/// tree. `open(dir, 'w')`, `os.truncate(dir)`, `copyfile(_, dir)` and
/// `writeFileSync(dir)` all fail on a directory (EISDIR), so a directory
/// literal is not their target. `shutil.copy2(src, dir)` DOES write into a
/// directory (`dir/<basename>`), so when the program spells it this answers
/// `false` for an UNPADDED literal and the literal is probed exactly as before.
///
/// Backlog 8bb1cec8: under `shutil.copy2(` a literal with leading or trailing
/// whitespace is not that exception. `copy2` (Python) receives the string
/// verbatim, so `" / "` names the relative path `" / "`, never the directory
/// `/` the trimmed candidate resolves to; such a literal is in practice the
/// path-join operator `new / "target"` paired out of the program by
/// [`quoted_string_literals`], and probing its trimmed form reported "it
/// writes `/`" with recoverability facts about the filesystem root. A padded
/// directory literal is therefore treated like any other directory literal:
/// not a write target. `literal` is the untrimmed text; `candidate` its trim.
///
/// A path whose kind cannot be read (`metadata` fails, relative with no base)
/// is also `false`: unknown stays on the probing side.
fn names_unwritable_directory(
    literal: &str,
    candidate: &str,
    payload: &str,
    base: Option<&str>,
) -> bool {
    if payload.to_ascii_lowercase().contains("shutil.copy2(") && literal == candidate {
        return false;
    }
    crate::reversible::resolve(candidate, base)
        .and_then(|p| std::fs::metadata(p).ok())
        .is_some_and(|m| m.is_dir())
}

/// The overwrite-shaped call in `payload`, if any. Case-folded for the same
/// reason as [`interpreter_destructive_call`].
fn interpreter_write_call(payload: &str) -> Option<&'static str> {
    let lowered = payload.to_ascii_lowercase();
    INTERPRETER_WRITE_CALLS
        .iter()
        .find(|needle| lowered.contains(**needle))
        .copied()
}

/// Whether EVERY occurrence of `literal` in `payload` is positively a
/// read-only `open(...)` whose result is consumed by a read — the only case in
/// which the recoverability probe may skip that literal (backlog 658b7839).
///
/// Why: [`interpreter_write_call`] answers "does this program write anywhere",
/// not "which literal is written". Without this, `json.load(open('A'))` next to
/// an unrelated `open(other,'w')` had `A` probed and reported as the `,'w')`
/// target.
///
/// Accepted, per occurrence of the raw text `literal` (not only quoted ones —
/// any occurrence outside the shape rejects):
/// - it is quoted exactly as `'literal'` or `"literal"`, the opening quote
///   immediately preceded by bare `open(` (the char before `open` is not an
///   identifier char or `.`, so `io.open(`, `os.open(`, `fopen(`, f-strings and
///   byte/raw prefixes are all rejected);
/// - the closing quote is followed by `)` or by an explicit read mode
///   `,'r')` / `, 'r')` / `,'rb')` / `, 'rb')` (or the double-quoted twins);
/// - that open call is then consumed by a read: immediately followed by
///   `.read(`, `.readline(` or `.readlines(`, or it is the sole argument of a
///   `load(` call (`json.load(open('A'))`), `load` itself not being the tail
///   of a longer identifier.
///
/// Rejected (keeps probing): zero occurrences, `with open('A') as f:` (the
/// handle escapes and `f.name` could feed a write), `open('A').name`, any
/// write/append mode, keyword args, assignment `x='A'`, or anything else.
///
/// What this does NOT prove: that the program never writes `A` through some
/// spelling that does not contain the literal text (e.g. a path rebuilt by
/// concatenation). Such a program already evades the probe whether or not this
/// helper exists, since the probe only sees whole literals; this helper only
/// removes the misattribution and never turns a reject into an allow on its
/// own.
fn literal_is_read_only_open(literal: &str, payload: &str) -> bool {
    if literal.is_empty() {
        return false;
    }
    let bytes = payload.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut seen = 0usize;
    let mut from = 0usize;
    while let Some(rel) = payload[from..].find(literal) {
        let start = from + rel;
        let end = start + literal.len();
        seen += 1;
        // Advance one char (not one match) so overlapping occurrences are
        // inspected too.
        from = start + payload[start..].chars().next().map_or(1, char::len_utf8);

        // Quoted exactly by a matching quote pair.
        let Some(&q) = start.checked_sub(1).and_then(|i| bytes.get(i)) else {
            return false;
        };
        if (q != b'\'' && q != b'"') || bytes.get(end) != Some(&q) {
            return false;
        }
        // Bare `open(` directly before the opening quote.
        let quote_at = start - 1;
        let Some(open_at) = quote_at.checked_sub("open(".len()) else {
            return false;
        };
        if &payload.as_bytes()[open_at..quote_at] != b"open(" {
            return false;
        }
        if open_at > 0 {
            let prev = bytes[open_at - 1];
            if is_ident(prev) || prev == b'.' {
                return false;
            }
        }
        // `)` or an explicit read mode closes the call.
        let after_quote = &payload[end + 1..];
        let qc = q as char;
        let close_len = [
            ")".to_string(),
            format!(",{qc}r{qc})"),
            format!(", {qc}r{qc})"),
            format!(",{qc}rb{qc})"),
            format!(", {qc}rb{qc})"),
        ]
        .iter()
        .find(|c| after_quote.starts_with(c.as_str()))
        .map(String::len);
        let Some(close_len) = close_len else {
            return false;
        };
        let after_call = &after_quote[close_len..];
        // Consumed by a read.
        let read_after = [".read(", ".readline(", ".readlines("]
            .iter()
            .any(|r| after_call.starts_with(r));
        let load_around = after_call.starts_with(')') && payload[..open_at].ends_with("load(") && {
            let load_at = open_at - "load(".len();
            load_at == 0 || !is_ident(bytes[load_at - 1])
        };
        if !(read_after || load_around) {
            return false;
        }
    }
    seen > 0
}

/// The destructive interpreter-level call in `payload`, if any. Case-folded
/// because `FileUtils.rm_rf` and `fileutils.rm_rf` are the same hazard.
fn interpreter_destructive_call(payload: &str) -> Option<&'static str> {
    let lowered = payload.to_ascii_lowercase();
    INTERPRETER_DESTRUCTIVE_CALLS
        .iter()
        .find(|needle| lowered.contains(**needle))
        .copied()
}

/// Every [`INTERPRETER_DESTRUCTIVE_CALLS`] spelling present in `payload`
/// (case-folded). [`interpreter_destructive_call`] returns only the first; the
/// inert-literal downgrade must hold for ALL of them, or a second spelling in
/// code would ride along with the first one's string-only proof.
fn interpreter_destructive_calls(payload: &str) -> Vec<&'static str> {
    let lowered = payload.to_ascii_lowercase();
    INTERPRETER_DESTRUCTIVE_CALLS
        .iter()
        .filter(|needle| lowered.contains(**needle))
        .copied()
        .collect()
}

/// Commands whose effect cannot be undone after the fact, sitting beside the
/// `mkfs` arm that already denies for the same reason.
///
/// This list is not the unsound kind. The v0.2.57 attempt failed because it
/// enumerated EFFECTS — an open set, since a program reaches an effect without
/// naming it — and used a miss as grounds to allow. These are enumerated
/// IRREVERSIBLE OPERATIONS and used only to deny: a miss falls through to the
/// rules below exactly as before, so the list being short costs nothing it was
/// not already costing. Each entry is a distinctive program name, so quoting
/// cannot hide it once `quoted_string_literals` reaches the text.
///
/// Every one of these was measured reaching `Allow` on 2026-09-09 against
/// blastguard 0.2.58 (adversary corpus, 112 must-surface cases): the gate would
/// have wiped a partition table or expired a reflog without a word.
pub(super) fn irreversible_device_verb(cmd: &str, rest: &[&str]) -> Option<&'static str> {
    let joined = rest.join(" ");
    match cmd {
        "wipefs" => Some("wipefs erases the filesystem/partition signatures on a device"),
        "blkdiscard" => Some("blkdiscard discards every block on a device"),
        "sgdisk" if joined.contains("--zap") => {
            Some("sgdisk --zap-all destroys the GUID partition table")
        }
        "parted" | "sfdisk" | "cfdisk" | "fdisk" if joined.contains("mklabel") => {
            Some("writing a new disk label discards the existing partition table")
        }
        "mke2fs" | "mkswap" | "mkntfs" | "mkdosfs" => {
            Some("this formats a filesystem, destroying all data on the target")
        }
        "nvme" if joined.contains("format") => Some("nvme format erases the namespace's contents"),
        "cryptsetup" if joined.contains("luksErase") || joined.contains("erase") => {
            Some("cryptsetup erase destroys the LUKS key slots — the data becomes unreadable")
        }
        "lvremove" | "vgremove" | "pvremove" => {
            Some("removing an LVM volume destroys the data it holds")
        }
        "hdparm" if joined.contains("security-erase") => {
            Some("hdparm --security-erase wipes the whole drive at the firmware level")
        }
        "badblocks" if joined.contains("-w") => {
            Some("badblocks -w is a WRITE-mode test: it overwrites every block it checks")
        }
        "crontab" if rest.contains(&"-r") => {
            Some("crontab -r deletes the user's crontab with no copy kept")
        }
        "chattr" if joined.contains("+i") => Some(
            "chattr +i makes a file immutable — the gates that need to rewrite it stop working",
        ),
        _ => None,
    }
}

/// git subcommands that destroy the object store itself.
///
/// Separate from [`irreversible_device_verb`] because `git` never reaches that
/// arm — it has its own [`analyze_git`] — and because these are the specific
/// commands that invalidate the assumption [`crate::reversible`] runs on. That
/// module answers "these bytes are recoverable" with "git has a copy"; these
/// three commands are how the copy stops existing.
pub(super) fn irreversible_git_subcommand(sub: &str, args: &[&str]) -> Option<&'static str> {
    let joined = args.join(" ");
    match sub {
        "gc" if joined.contains("--prune=now") || joined.contains("--prune=all") => Some(
            "git gc --prune=now deletes unreachable objects with no grace period — the dangling \
commits that make dropped stashes, deleted branches and hard resets recoverable are gone",
        ),
        "reflog" if args.first() == Some(&"expire") && joined.contains("--expire=now") => Some(
            "git reflog expire --expire=now drops the reflog entries that are the last handle on \
commits nothing else references",
        ),
        "filter-branch" | "filter-repo" => Some(
            "rewriting history detaches every existing clone and worktree from the commits they \
are built on",
        ),
        _ => None,
    }
}

/// Ask when any positional operand names a protected gate/config path.
///
/// This is the GATE-DISABLING axis, and it is deliberately independent of
/// recoverability: `.githooks/pre-commit` is tracked by git and trivially
/// restored, and must STILL be surfaced, because a gate that got quietly
/// rewritten makes every verdict after it worthless. The operator's ruling
/// removed friction from the recoverability axis; it did not touch this one
/// (「破壊的な変更を暗黙に実施しないこと」).
///
/// `unknown_verb_protected_ask` cannot be reused for either caller: it returns a
/// flat `Allow` for anything `is_code_interpreter` matches, and it is not
/// reached at all once a command word resolves to nothing nameable.
fn protected_operand_ask(head: &str, rest: &[&str], operands_are_files: bool) -> Option<Decision> {
    for op in positional_operands(rest, &[]) {
        if !has_glob_meta(op) {
            if exclude::touches_protected(op) {
                return Some(Decision::ask(format!(
                    "`{head}` has no irreversible operation in it, but `{op}` is a protected \
gate/config path — changing it changes which hooks and gates run, and blastguard will not do that \
implicitly"
                )));
            }
            continue;
        }
        if let Some(prefix) = glob_literal_prefix(op) {
            if exclude::touches_protected(&prefix) {
                return Some(Decision::ask(format!(
                    "`{head}` has no irreversible operation in it, but `{op}` expands under \
`{prefix}`, a protected gate/config path — changing it changes which hooks and gates run, and \
blastguard will not do that implicitly"
                )));
            }
            continue;
        }
        // A pattern with no literal prefix (`?githooks/pre-commit`, `*`) was
        // never TESTED against the protected set — the test was not applicable.
        // v0.2.57 wrote `unwrap_or(false)` here and so reported untested as
        // unprotected, which is how `perl -i -pe s/.*// ?githooks/pre-commit`
        // got through. §3: an absent answer resolves to the restrictive side,
        // and it still does — but only for a caller whose operands really are
        // file operands.
        //
        // TWO CORRECTIONS, both measured 2026-09-09 over 2045 real Bash
        // commands, and both about this branch specifically.
        //
        // 1. THE REASON WAS FALSE. This branch means "blastguard could not test
        //    this token"; it was reporting "`{op}` IS a protected gate/config
        //    path". Those are different statements and the second was never
        //    observed — a reason asserting an unmade finding is the §4 failure
        //    aimed squarely at the human reading the block. The text below now
        //    says which of the two actually happened.
        //
        // 2. IT WAS NOT ALWAYS LOOKING AT OPERANDS. Tokens arrive here already
        //    whitespace-split with quoting stripped, so an inline-eval payload
        //    turns into a fistful of code words — and `[` is a glob
        //    metacharacter. `len(d['task']))` denoises to `len(d[task]))`,
        //    which has glob meta and no literal DIRECTORY prefix, so it was
        //    reported as an untested protected path. Eleven asks in the sample
        //    were exactly this, each naming a Python fragment as a gate file.
        //    `operands_are_files` is how a caller states that its trailing
        //    tokens are really file operands: an in-place editor
        //    (`perl -i -pe … ?githooks/pre-commit`) rewrites what it is handed
        //    and passes `true`; a plain `python3 -c '…'` cannot tell an operand
        //    from a word of its own program and passes `false`.
        //
        // The residual that leaves is narrow and named: `python3 -c '…'
        // ?githooks/x` reaches Allow unless the program's own string literals
        // name the path, which `interpreter_code_verdict` checks separately.
        if operands_are_files {
            return Some(Decision::ask(format!(
                "`{head}` rewrites its file operands in place, and `{op}` is a glob with no \
literal prefix — blastguard could not test whether it expands onto a protected gate/config path, \
and an untested answer is not a clean one"
            )));
        }
    }
    None
}

/// True when the interpreter was told to REWRITE the files it is handed rather
/// than merely read them: `perl -i`, `perl -pi -e`, `ruby -i.bak`, `sed -i`,
/// and the `--in-place` long spelling.
///
/// Only these commands make the trailing tokens file operands with certainty,
/// which is what [`protected_operand_ask`] needs before it can fail closed on a
/// glob it could not test. `python3 -i` is deliberately NOT here: python's `-i`
/// means "drop to an interactive prompt afterwards" and says nothing about
/// operands, so folding it in would re-open the false-reason asks this
/// distinction exists to close.
fn interpreter_edits_in_place(cmd: &str, rest: &[&str]) -> bool {
    let base = cmd.rsplit('/').next().unwrap_or(cmd);
    let stem = base.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if !matches!(stem, "perl" | "ruby" | "sed" | "gsed") {
        return false;
    }
    rest.iter().any(|t| {
        if let Some(long) = t.strip_prefix("--") {
            return long == "in-place" || long.starts_with("in-place=");
        }
        match t.strip_prefix('-') {
            // A bundled short-flag run. `-i`, `-pi`, `-i.bak` all edit in place.
            Some(letters) => {
                !letters.is_empty() && letters.starts_with(|c: char| c != '-') && {
                    let head: String = letters
                        .chars()
                        .take_while(|c| c.is_ascii_alphabetic())
                        .collect();
                    head.contains('i')
                }
            }
            None => false,
        }
    })
}

/// Closing delimiter for a Perl/Ruby generic-quote opener.
///
/// The bracket pairs close with their mirror; every other punctuation character
/// closes with itself (`q!…!`, `q/…/`, `%w|…|`).
fn generic_quote_close(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '{' => Some('}'),
        '[' => Some(']'),
        '<' => Some('>'),
        c if c.is_ascii_punctuation() && !c.is_alphanumeric() && c != '_' => Some(c),
        _ => None,
    }
}

/// True when the character before a generic-quote operator lets it BE an
/// operator rather than the tail of an identifier. `q(` inside `unique_q(x)` is
/// part of a name; `system q(…)` is a quote.
fn generic_quote_can_start(prev: Option<char>) -> bool {
    !matches!(prev, Some(c) if c.is_alphanumeric() || c == '_' || c == '$' || c == '@' || c == '%')
}

/// Bodies of Perl/Ruby **generic quotes** — `q(…)`, `qq{…}`, `qw[…]`, `qx/…/`,
/// `%q(…)`, `%Q{…}`, `%w[…]`, `%x(…)`.
///
/// THIS IS THE HOLE THAT SHIPPED IN v0.2.57 AND WAS REVERTED IN v0.2.58.
/// [`quoted_string_literals`] knew exactly three delimiters — `"`, `'`, `` ` ``
/// — so `perl -e "system 'mkfs /dev/sda'"` was denied while
/// `perl -e "system q(mkfs /dev/sda)"` was not. The SAME hazard, and the
/// spelling of the quote decided the verdict. The descent simply could not
/// reach the body, and "could not reach" was resolving to the permissive side.
///
/// Adding a second denylist of effect tokens (what v0.2.57 did) does not fix
/// this and cannot: the defect is a blind spot in the DESCENT, so the repair
/// belongs in the descent. With the body extracted, the shell analyser that
/// already judges `bash -c` payloads reaches it unchanged — no new judgment,
/// and no new list to keep complete.
///
/// Nesting is tracked for the four bracket pairs, so `qw(a (b) c)` yields the
/// whole body rather than stopping at the inner `)`. Every returned slice is a
/// strict substring of `payload`, which is what keeps the caller's descent
/// terminating.
fn generic_quote_literals(payload: &str) -> Vec<String> {
    // Longest first: `qq` and `%q` must win over `q` at the same position.
    const OPS: &[&str] = &[
        "%q", "%Q", "%w", "%W", "%i", "%I", "%x", "%r", "%s", "qq", "qw", "qr", "qx", "q", "%",
    ];
    let bytes = payload.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    'scan: while i < payload.len() {
        if !payload.is_char_boundary(i) {
            i += 1;
            continue;
        }
        for op in OPS {
            if !payload[i..].starts_with(op) {
                continue;
            }
            let prev = payload[..i].chars().next_back();
            if !generic_quote_can_start(prev) {
                continue;
            }
            // A bare `%` is the modulo/format operator far more often than it
            // is a quote, so it only counts when a BRACKET follows it —
            // `x % (y)` is arithmetic, `%(…)` is not written that way in
            // practice, so require a bracket AND a non-space before nothing.
            let after = i + op.len();
            let Some(open) = payload[after..].chars().next() else {
                continue;
            };
            if *op == "%" && !matches!(open, '(' | '{' | '[') {
                continue;
            }
            let Some(close) = generic_quote_close(open) else {
                continue;
            };
            let body_start = after + open.len_utf8();
            let nests = matches!(open, '(' | '{' | '[' | '<');
            let mut depth = 1usize;
            let mut j = body_start;
            while j < payload.len() {
                let Some(c) = payload[j..].chars().next() else {
                    break;
                };
                if c == '\\' {
                    j += c.len_utf8();
                    if let Some(esc) = payload[j..].chars().next() {
                        j += esc.len_utf8();
                    }
                    continue;
                }
                if nests && c == open {
                    depth += 1;
                } else if c == close {
                    depth -= 1;
                    if depth == 0 {
                        let inner = &payload[body_start..j];
                        if !inner.trim().is_empty() && !out.iter().any(|p| p == inner) {
                            out.push(inner.to_string());
                        }
                        i = j + c.len_utf8();
                        continue 'scan;
                    }
                }
                j += c.len_utf8();
            }
            // Unterminated: nothing reliable to extract, and skipping the
            // operator is what lets the outer scan keep making progress.
            break;
        }
        i += payload[i..].chars().next().map_or(1, char::len_utf8);
        let _ = bytes;
    }
    out
}

/// The quoted string literals inside an interpreter program. A destructive
/// SHELL command reaches the system through one of these
/// (`os.system("mkfs /dev/sda")`, `` `rm -rf /` ``), so each literal is handed
/// to the shell analyser that already judges `bash -c` payloads.
///
/// Covers the three ASCII quotes plus every Perl/Ruby generic quote (see
/// [`generic_quote_literals`] for why the latter is not optional).
pub(super) fn quoted_string_literals(payload: &str) -> Vec<String> {
    let mut out = generic_quote_literals(payload);
    let mut chars = payload.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if !matches!(c, '"' | '\'' | '`') {
            continue;
        }
        let mut end = None;
        for (i, d) in chars.by_ref() {
            if d == c {
                end = Some(i);
                break;
            }
        }
        if let Some(end) = end {
            let inner = &payload[start + c.len_utf8()..end];
            if !inner.trim().is_empty() && !out.iter().any(|p| p == inner) {
                out.push(inner.to_string());
            }
        }
    }
    out
}

/// What [`Ctx::program_text_redirects`] points at while
/// [`interpreter_code_verdict`] runs its shell pass over program text.
pub(super) struct ProgramTextSink {
    /// The R2 rows: truncating redirects whose target cannot be determined,
    /// recorded instead of denied (see [`Ctx::program_text_redirects`]).
    pub(super) rows: std::cell::RefCell<Vec<String>>,
    /// True ONLY for an inline PYTHON program the Python tokenizer showed never
    /// hands a string to a shell or a process (#1626, user ruling 2026-11-07;
    /// see "PYTHON STRINGS ARE DATA" on [`interpreter_code_verdict`]). Then a
    /// truncating redirect with a DETERMINABLE target, which the redirect arm
    /// would otherwise deny on its recoverability, is not denied: it is a `>`
    /// inside a Python string, or Python's comparison operator. Undeterminable
    /// targets still become R2 rows (an Ask), and every earlier refusal of the
    /// redirect arm (protected path, system directory, unplaceable target)
    /// still applies.
    pub(super) redirects_are_data: bool,
}

/// Judge interpreter code that this command will execute.
///
/// The arm this replaces returned a flat `Deny` on the SHAPE of the invocation,
/// without ever reading the program — `python3 -c "print(1)"` was denied with a
/// reason ("can run an arbitrary destructive command") that was a statement
/// about the shape, not a finding about the command. A gate's job is to audit
/// the action: if the program demonstrably destroys something, that is a
/// `Deny`; if it cannot be shown to, the honest verdict is `Ask` — "NOT a
/// verdict about the command, it is a refusal to guess" (`model.rs`). With no
/// human present `Decision::hardened` collapses the Ask to Deny anyway, so the
/// restrictive resolution still holds; what changes is that a real finding is
/// no longer indistinguishable from a shrug.
///
/// UNDETERMINABLE REDIRECTS IN PROGRAM TEXT (user ruling R2, 2026-10-14). The
/// shell pass over the program text reads every `>` as a redirect, so
/// `print(1 > 0)` or `s = 'x > \`y\`'` used to be DENIED as a truncating
/// redirect onto a file it could not name. When that is the ONLY Deny the
/// program text produces, the verdict is an Ask carrying [`RECOMMEND_NO`] —
/// not Allow: the `>` cannot be shown not to be a redirect. A Deny found
/// anywhere else (a destructive call, a shell command inside a literal such as
/// `os.system("echo hi > /etc/hosts")`, an existing unrecoverable target)
/// still wins. When another Ask is the result, the redirect finding and the
/// marker are appended to it, so the downgrade never disappears from the
/// reason.
///
/// PYTHON STRINGS ARE DATA (#1626, user ruling 2026-11-07). `python` is true
/// only for an inline-eval PYTHON program (`python3 -c '…'`). When, in
/// addition, every payload passes
/// [`python_file::python_strings_never_reach_a_shell`] (read with the Python
/// tokenizer) and no payload carries a [`SHELL_STRING_EXEC_MARKERS`] or
/// dynamic-execution marker anywhere, the shell pass runs with
/// [`ProgramTextSink::redirects_are_data`]: a `>` whose target IS determinable is
/// not denied, because in that program it is a string's contents or Python's
/// comparison operator — `python3 -c 's="> F"; print(s)'` is Allow. Any other
/// program (one that does not tokenise, imports a module outside the
/// allowlist, or references a runner) is judged exactly as before; an
/// undeterminable target still resolves to the R2 Ask; every other check is
/// unchanged. `strings_are_data` (only from the `python3 - <<'EOF'` audit,
/// [`python_file::python_stdin_body_reading`]) stops the descent at layer 0:
/// the parser showed no string of that program reaches a shell or a process.
pub(super) fn interpreter_code_verdict(
    shape: &str,
    payloads: &[String],
    depth: usize,
    ctx: &Ctx<'_>,
    expansion_possible: bool,
    python: bool,
    strings_are_data: bool,
) -> Decision {
    let redirects = ProgramTextSink {
        rows: std::cell::RefCell::new(Vec::new()),
        redirects_are_data: python
            && !payloads.is_empty()
            && payloads.iter().all(|p| {
                !runs_shell_strings(p)
                    && !has_dynamic_code_execution(p)
                    && python_file::python_strings_never_reach_a_shell(p)
            }),
    };
    let program_ctx = Ctx {
        program_text_redirects: Some(&redirects),
        ..ctx.clone()
    };
    let mentions = Mentions::default();
    let verdict = interpreter_program_verdict(
        shape,
        payloads,
        depth,
        ctx,
        &program_ctx,
        (expansion_possible, strings_are_data),
        &mentions,
    );
    // A destructive spelling that `inert_literal_bodies` showed sits only inside
    // plain, non-interpolating quoted literals of the layer-0 program, with no
    // dynamic-execution marker anywhere in it (backlog 27795e6e). Recorded
    // during the scan rather than returned from it, so a Deny found anywhere
    // else — an unrecoverable write, a shell command, a deeper layer — still
    // outranks it (model.rs). Otherwise it is an Ask, never Allow: the marker
    // list is a heuristic, so blastguard cannot show the string is never run.
    let verdict = match (mentions.string.get(), verdict) {
        (_, Decision::Deny(reason)) => Decision::Deny(reason),
        (None, v) => v,
        (Some(call), v) => {
            let finding = format!(
                "{shape}, and its program spells `{call}` only inside plain quoted string \
literals (no interpolation, no dynamic-execution marker seen); blastguard does not parse the \
program's language, so it cannot show that string is never run as code."
            );
            match v {
                Decision::Ask(reason) => {
                    recommend_against(Decision::ask(format!("{reason}; {finding}")))
                }
                _ => recommend_against(Decision::ask(finding)),
            }
        }
    };
    // A destructive spelling seen ONLY inside what reads as a `#` comment
    // (backlog 6798f47d). Not a Deny — the reason would claim a call nobody
    // observed — and not silence either: `strip_hash_comments` is not a parser
    // for the program's language, so a misread comment cannot be shown to be
    // inert. A Deny found elsewhere still wins.
    let verdict = match (mentions.comment.get(), verdict) {
        (_, Decision::Deny(reason)) => Decision::Deny(reason),
        (None, v) => v,
        (Some(call), v) => {
            let finding = format!(
                "{shape}, and its program spells `{call}` only in what reads as a `#` comment; \
blastguard does not parse the program's language, so it cannot show that `{call}` (which deletes \
files or directories outright) is not called"
            );
            match v {
                Decision::Ask(reason) => {
                    recommend_against(Decision::ask(format!("{reason}; {finding}")))
                }
                _ => recommend_against(Decision::ask(finding)),
            }
        }
    };
    let redirects = redirects.rows.into_inner();
    let Some(first) = redirects.first() else {
        return verdict;
    };
    let finding = format!(
        "{shape}, and the program text contains a `>` that a shell would read as a redirect \
whose target cannot be determined ({first}); in program text it may be an operator or part of a \
string, but blastguard cannot show that it is not a redirect."
    );
    match verdict {
        Decision::Deny(reason) => Decision::Deny(reason),
        Decision::Ask(reason) => recommend_against(Decision::ask(format!("{reason}; {finding}"))),
        Decision::Allow => recommend_against(Decision::ask(finding)),
    }
}

/// APIs (matched case-insensitively as substrings) that hand a STRING to a
/// shell: Python `os.system`/`os.popen`/`subprocess` (`shell=True` or not — an
/// argv list is re-joined separately), Perl/Ruby/PHP `system`/backticks/`qx`/
/// `%x`/`shell_exec`/`passthru`/`proc_open`/`popen`, Node `child_process`/
/// `execSync`, AppleScript `do shell script`. A false hit only lets a
/// shell-pass Ask on a literal stand (backlog 0fefcd3b); it never Denies.
const SHELL_STRING_EXEC_MARKERS: &[&str] = &[
    "os.system",
    "os.popen",
    "subprocess",
    "popen",
    "commands.getoutput",
    "commands.getstatusoutput",
    "pty.spawn",
    "system(",
    "system ",
    "child_process",
    "execsync",
    "shell_exec",
    "passthru",
    "proc_open",
    "qx",
    "%x",
    "`",
    "do shell script",
];

/// Is `d` the plain "command word is an expansion" Ask of `detect_bash`?
fn is_expansion_head_ask(d: &Decision) -> bool {
    matches!(d, Decision::Ask(r) if r.contains(super::EXPANSION_HEAD_TAIL))
}

/// Does `program` contain a [`SHELL_STRING_EXEC_MARKERS`] entry?
fn runs_shell_strings(program: &str) -> bool {
    let lowered = program.to_ascii_lowercase();
    SHELL_STRING_EXEC_MARKERS
        .iter()
        .any(|m| lowered.contains(m))
}

/// Destructive spellings [`interpreter_program_verdict`] saw but did not deny
/// on the spot. Each becomes an Ask in [`interpreter_code_verdict`] unless the
/// scan returns a Deny, which outranks it.
#[derive(Default)]
struct Mentions {
    /// Spelled only in what reads as a `#` comment (backlog 6798f47d).
    comment: std::cell::Cell<Option<&'static str>>,
    /// Spelled only inside literals [`inert_literal_bodies`] proved inert, in a
    /// layer-0 program with no dynamic-execution marker (backlog 27795e6e).
    string: std::cell::Cell<Option<&'static str>>,
}

/// The body of [`interpreter_code_verdict`]. `program_ctx` is `ctx` with
/// [`Ctx::program_text_redirects`] set; it is used ONLY for the shell pass
/// over each program-text layer, so every other check judges exactly as before
/// the R2 ruling.
fn interpreter_program_verdict(
    shape: &str,
    payloads: &[String],
    depth: usize,
    ctx: &Ctx<'_>,
    program_ctx: &Ctx<'_>,
    (expansion_possible, strings_are_data): (bool, bool),
    mentions: &Mentions,
) -> Decision {
    // Descend through nested quoting rather than stopping at the first layer.
    // A destructive command can sit several quote layers down —
    // `python3 <(echo "import os; os.system('mkfs /dev/sda')")` buries it three
    // deep — and a one-layer scan reported that shape as unreadable when it was
    // plainly readable. Each layer is a strict substring of the one above, so
    // the descent terminates; the cap only bounds pathological nesting.
    const MAX_QUOTE_LAYERS: usize = 4;
    // Each recovery probe spawns `git status` on one path. This runs inside a
    // PreToolUse hook, so the number of them a single command line can provoke
    // is bounded rather than left to the length of the program.
    const MAX_RECOVERY_PROBES: usize = 6;
    // Backlog 27795e6e: the inert-literal downgrade is only considered when no
    // layer-0 payload carries a dynamic-execution marker — a program that can
    // run a string is not one whose strings are shown to be data.
    let dynamic_execution = payloads.iter().any(|p| has_dynamic_code_execution(p));
    // Backlog 0fefcd3b: whether a string of this program may be handed to a
    // shell, which makes a shell-pass Ask on a literal count (see below).
    let shell_strings = payloads.iter().any(|p| runs_shell_strings(p));
    // Bodies of layer-0 literals `inert_literal_bodies` proved inert. A deeper
    // layer is exempt from the destructive-spelling Deny ONLY when it is
    // verbatim one of these (it is the same string contents one layer down);
    // every other deeper-layer payload keeps the Deny.
    let mut inert_bodies: Vec<String> = Vec::new();
    // Backlog 7483e85b: an Ask earned inside the descent (a protected-path
    // literal, a write confined to a safe root) is PENDING, never an early
    // return — every literal of every layer is still scanned, so a Deny earned
    // anywhere else outranks it (Deny over Ask over Allow, model.rs). The
    // first pending Ask is kept so its reason is the one reported.
    let mut pending_ask: Option<Decision> = None;
    // Backlog 3c750df2: the program's OS-facing calls, read from its code (see
    // `shell_calls::program_call_verdict`). A Deny returns; an Ask is pending.
    for payload in payloads {
        match shell_calls::program_call_verdict(payload, depth, ctx) {
            d @ Decision::Deny(_) => return d,
            a @ Decision::Ask(_) => {
                pending_ask.get_or_insert(a);
            }
            Decision::Allow => {}
        }
    }
    let mut layer: Vec<String> = payloads.to_vec();
    for depth_of_layer in 0..MAX_QUOTE_LAYERS {
        // Deeper layers are string contents: not code when `strings_are_data`.
        if layer.is_empty() || (strings_are_data && depth_of_layer > 0) {
            break;
        }
        let mut next = Vec::new();
        for payload in &layer {
            // Backlog 6798f47d: judged on the text with `#` comments removed,
            // so a mention in a comment is not reported as a call. A spelling
            // that exists ONLY in a comment is recorded, not dropped.
            let calls = interpreter_destructive_calls(&strip_hash_comments(payload, false));
            if let Some(&call) = calls.first() {
                // Backlog 27795e6e: a spelling that sits only inside literals
                // `inert_literal_bodies` positively proved inert (layer 0, every
                // destructive spelling in the payload, no dynamic-execution
                // marker in the program) is recorded as a PENDING Ask and the
                // scan continues, so a Deny earned anywhere else still outranks
                // it. It is never an early return.
                let proven_inert: Option<Vec<String>> = if dynamic_execution {
                    None
                } else if depth_of_layer == 0 {
                    calls
                        .iter()
                        .map(|c| inert_literal_bodies(payload, c))
                        .collect::<Option<Vec<Vec<&str>>>>()
                        .map(|v| v.into_iter().flatten().map(str::to_string).collect())
                } else if inert_bodies.iter().any(|b| b == payload) {
                    Some(Vec::new())
                } else {
                    None
                };
                if let Some(bodies) = proven_inert {
                    if mentions.string.get().is_none() {
                        mentions.string.set(Some(call));
                    }
                    for body in bodies {
                        if !inert_bodies.contains(&body) {
                            inert_bodies.push(body);
                        }
                    }
                } else {
                    // Layer 0 is the program text itself; deeper layers, and
                    // any spelling found only inside a quoted string of layer
                    // 0, are string contents that may or may not be run as
                    // code — the reason says so rather than claiming a call.
                    // Same Deny either way: only the wording depends on
                    // `strip_hash_comments(payload, true)`.
                    let outside_strings = depth_of_layer == 0
                        && interpreter_destructive_call(&strip_hash_comments(payload, true))
                            .is_some();
                    if outside_strings {
                        return Decision::deny(format!(
                            "{shape}, and it runs `{call}`, which deletes files or directories \
outright"
                        ));
                    }
                    return Decision::deny(format!(
                        "{shape}, and a quoted string in its program spells `{call}`, which deletes \
files or directories outright if that string is run as code; blastguard cannot show it is not"
                    ));
                }
            } else if mentions.comment.get().is_none() {
                if let Some(call) = interpreter_destructive_call(payload) {
                    mentions.comment.set(Some(call));
                }
            }
            // `program_ctx`: an undeterminable redirect target is recorded for
            // `interpreter_code_verdict` instead of denied (ruling R2).
            //
            // Backlog 0fefcd3b: an ASK from this shell pass used to be dropped,
            // so `os.system('echo x > .git/hooks/pre-commit')` was Allow while
            // the same redirect at top level is the protected-path Ask (Deny
            // wherever no human can answer). A string literal (layer > 0) of a
            // program that hands strings to a shell ([`runs_shell_strings`])
            // is that shell command line, so its Ask is kept as PENDING —
            // outranked by any Deny the rest of the scan earns. `detect_bash`
            // directly, not `analyze_shell_payload`: the latter's extra
            // "command word is an expansion" Ask would fire on every literal
            // such as `"$HOME/x"`, which is data far more often than a command.
            // Its Deny is the same (`analyze_shell_payload` denies only through
            // `detect_bash`).
            //
            // The R2 side channel (`program_text_redirects`) follows the same
            // rule as that Ask: a string literal of a program with NO
            // shell-string marker is never a shell line, so an undeterminable
            // `>` it holds (`print('>')`, `"x->y"`) is data, not a redirect
            // (tests/heredoc_body_gt_fp_20261107.rs). Its rows are dropped;
            // the program text itself (layer 0) keeps them, and a Deny from the
            // literal still returns below. A program that hands strings to a
            // shell keeps every row.
            let sink_rows = program_ctx
                .program_text_redirects
                .map(|s| s.rows.borrow().len());
            let verdict = super::detect_bash(payload, depth + 1, program_ctx);
            if depth_of_layer > 0 && !shell_strings {
                if let (Some(sink), Some(rows)) = (program_ctx.program_text_redirects, sink_rows) {
                    sink.rows.borrow_mut().truncate(rows);
                }
            }
            match verdict {
                Decision::Deny(reason) => return Decision::Deny(reason),
                // The "command word is an expansion" Ask (detect.rs
                // `EXPANSION_HEAD_TAIL`) is NOT kept: a literal like `"$HOME"`
                // or an argv element `'$HOME'` is data far more often than a
                // command. Any other Ask (protected path, ...) is kept.
                ask @ Decision::Ask(_)
                    if depth_of_layer > 0 && shell_strings && !is_expansion_head_ask(&ask) =>
                {
                    if pending_ask.is_none() {
                        pending_ask = Some(ask);
                    }
                }
                Decision::Ask(_) | Decision::Allow => {}
            }
            let literals = quoted_string_literals(payload);
            // GATE-DISABLING axis, checked on the OBJECTS rather than the verbs.
            //
            // `perl -e "unlink q{.githooks/pre-commit}"` reached Allow while
            // this was verb-only: `INTERPRETER_DESTRUCTIVE_CALLS` lists
            // `unlink(`, `unlink "`, `unlink '`, `unlink glob`, … and a
            // generic-quote spelling is simply not among them. Extending that
            // list is the move that failed in v0.2.57 — a verb can always be
            // spelled another way, or reached without being spelled at all.
            //
            // A path cannot. Whatever the program means to do to
            // `.githooks/pre-commit`, naming it is how it gets there, so the
            // decidable question is whether a protected path appears in the
            // program's own string literals. That is checked here, once, for
            // every quoting form the descent can reach.
            // ARGV-LIST SUBPROCESS CALLS. `subprocess.run(['git','stash',
            // 'clear'])` never forms a shell line, so `analyze_shell_payload`
            // above sees only the Python around it and the per-literal scans
            // below see three harmless words. `subprocess.run(['mkfs.ext4', …])`
            // denied only because its first element is a recognisable command
            // ALONE; the moment the hazard needs two words to be visible, it
            // vanished. Measured 2026-09-09: this was the single case where the
            // candidate lost coverage 0.2.58 had.
            //
            // Re-joining the list restores the line the interpreter is about to
            // exec. Gated on the payload actually containing a subprocess
            // spelling so the join is never invented out of unrelated literals
            // in an ordinary program.
            let lowered = payload.to_ascii_lowercase();
            if [
                "subprocess.",
                "child_process",
                "spawnsync(",
                "execfile(",
                // Deno / Bun argv-list spawns (daf7611b).
                "deno.command(",
                "deno.run(",
                "bun.spawn(",
            ]
            .iter()
            .any(|m| lowered.contains(m))
            {
                if let Decision::Deny(reason) =
                    analyze_shell_payload(&literals.join(" "), depth, ctx)
                {
                    return Decision::Deny(reason);
                }
            }
            let write_call = interpreter_write_call(payload);
            let mut probed = 0usize;
            for literal in &literals {
                let candidate = literal.trim();
                if candidate.is_empty() || candidate.contains(char::is_whitespace) {
                    continue;
                }
                if exclude::touches_protected(candidate) && pending_ask.is_none() {
                    pending_ask = Some(Decision::ask(format!(
                        "{shape}, and that program names `{candidate}`, a protected gate/config \
path — changing it changes which hooks and gates run, and blastguard will not do that implicitly"
                    )));
                }
                // RECOVERABILITY of what the program OVERWRITES.
                //
                // The verdict below allows a program in which no irreversible
                // operation was found, and that was leaking a whole class:
                // `os.truncate('<an Obsidian note>', 0)`,
                // `open('<the same note>','w')`,
                // `shutil.copyfile('/dev/null', '<the same note>')` and
                // `open('/dev/sda','wb').write(…)` were all measured reaching
                // Allow on 2026-09-09, every one of them naming its target in
                // plain text the descent had already read. 0.2.58 blocked all
                // four — by blocking every interpreter program, benign ones
                // included, which is the friction the ruling removed.
                //
                // The axis that separates them is the same one
                // `reversible.rs` was written for, applied one level in: a
                // write to a tracked, clean file costs a `git restore` and is
                // none of the gate's business; a write to a note that exists
                // nowhere else is final. `/dev/sda` lands here too and needs no
                // special case — it exists, it is in no work tree, so nothing
                // holds a second copy of it.
                //
                // Gated on `write_call` so a READ of an unrecoverable path
                // stays silent: `open('/etc/hosts').read()` names an
                // unrecoverable file and destroys nothing, and asking about it
                // is exactly the waste the ruling names. Gated on the literal
                // looking like a path, and capped, because each probe spawns
                // git and this runs inside a PreToolUse hook.
                //
                // `write_call` says the program writes SOMEWHERE, not which
                // literal it writes. A literal whose every occurrence is a
                // read-only `open(...)` consumed by a read is not the target
                // (backlog 658b7839); anything not positively attributed that
                // way is still probed.
                let write_target_candidate = write_call.is_some()
                    && candidate.contains('/')
                    && !literal_is_read_only_open(candidate, payload)
                    && !names_unwritable_directory(
                        literal,
                        candidate,
                        payload,
                        ctx.raw_base.as_deref(),
                    );
                if write_target_candidate && probed < MAX_RECOVERY_PROBES {
                    probed += 1;
                    let recovery = crate::reversible::probe(candidate, ctx.raw_base.as_deref());
                    if let crate::reversible::Recovery::Unrecoverable(why) = &recovery {
                        let call = write_call.unwrap_or("a write");
                        if let Some(root) = ctx.confined_root("interpreter write", &[candidate]) {
                            if pending_ask.is_none() {
                                pending_ask = Some(confined_ask(
                                    &format!("`{call}` onto `{candidate}`"),
                                    &root,
                                    &[candidate],
                                ));
                            }
                            continue;
                        }
                        return Decision::deny(format!(
                            "{shape}, and it writes `{candidate}` (`{call}`) — {why}"
                        ));
                    }
                    // An unanswered probe is not "recoverable" (CLAUDE.md §3):
                    // it is a pending Ask, outranked by any Deny the rest of
                    // the program earns, and never left to reach Allow.
                    if let crate::reversible::Recovery::Undetermined(why) = &recovery {
                        if pending_ask.is_none() {
                            let call = write_call.unwrap_or("a write");
                            pending_ask = Some(Decision::ask(format!(
                                "{shape}, and it writes `{candidate}` (`{call}`) — {why}"
                            )));
                        }
                    }
                }
                // SYSTEM-DIRECTORY axis (backlog 0fbfe00d / e34eb0be). The
                // probe above answers `NothingToDestroy` for a path that does
                // not exist yet, which is the right answer about bytes and the
                // wrong question for `/etc/sudoers.d/evil` or a new
                // LaunchAgents plist: the file's EXISTENCE is the change. The
                // direct forms (`>`, `tee`, `cp`) ask `system_path_block` for
                // exactly this reason, and an interpreter carrying the same
                // write must not answer less restrictively (user ruling
                // 2026-11-07: system harm is Deny through any wrapper). Checked
                // for every write-target literal, including those past the
                // probe cap — it spawns nothing. A Deny returns at once; an Ask
                // (the LaunchAgents ruling, or a landing whose symlinks cannot
                // be resolved) is pending and outranked by any Deny later.
                if write_target_candidate {
                    let block = match super::system_path_block("interpreter write", candidate, ctx)
                    {
                        Some(d) => Some(d),
                        None => {
                            match super::real_system_path_block("interpreter write", candidate, ctx)
                            {
                                Ok(d) => d,
                                Err(why) => Some(Decision::ask(format!(
                                "{shape}, and it writes `{candidate}`, which could not be resolved \
through its symlinks ({why}) — blastguard cannot tell whether it lands in a system directory, so \
it refuses to guess"
                            ))),
                            }
                        }
                    };
                    match block {
                        Some(d) if d.is_deny() => return d,
                        Some(ask) if pending_ask.is_none() => pending_ask = Some(ask),
                        _ => {}
                    }
                }
            }
            next.extend(literals);
        }
        layer = next;
    }
    // The whole descent ran without a Deny; an Ask it recorded now stands.
    if let Some(ask) = pending_ask {
        return ask;
    }
    // THE EMPTY SET IS NOT A CLEAN SCAN (CLAUDE.md §3). Everything below turns
    // on the program text having been READ; if there was no text, nothing was
    // examined and the Allow would be asserting a scan that never happened.
    //
    // The two shapes are not alike and must not share a verdict:
    //   `python3 - <<'PY' … PY`     — the body is in `payloads`; it was read.
    //   `cat evil.py | python3 -`   — the program is in a file. `payloads` is
    //                                 empty and the command line says nothing
    //                                 whatever about what will run.
    // v0.2.57 allowed both and re-opened a fail-open that
    // `tests/inline_eval_mirror_closure.rs` had already closed once. The
    // measured friction is entirely the first shape (51 here-documents across
    // 25 transcripts), so withholding the second costs nothing real.
    if payloads.iter().all(|p| p.trim().is_empty()) {
        return Decision::ask(format!(
            "{shape}, and that program is not on the command line — blastguard has no text to \
read, so it cannot tell whether anything here is irreversible"
        ));
    }

    // TEXT THAT WILL BE SUBSTITUTED IS NOT THE PROGRAM EITHER. A payload
    // carrying `$…`, `$(…)` or a backtick is expanded by the shell before the
    // interpreter ever sees it, so what was scanned above is a template and
    // what runs is something else. `printf '%s' "$(cat evil.py)" | node` has a
    // perfectly non-empty payload and says exactly as much about the program as
    // `cat evil.py | node` does, which is nothing.
    //
    // This is the guard the empty-set check alone does not give: without it,
    // the arbitrary-program hole simply moves from "no text" to "text that is a
    // placeholder", and the mirror closure in
    // `tests/inline_eval_mirror_closure.rs` would be re-opened through a
    // spelling it does not enumerate.
    // NOR IS TEXT THAT BUILDS ITS OWN CODE: see [`CODE_BUILDING`].
    for payload in payloads {
        if let Some(marker) = code_building_marker(payload) {
            return Decision::ask(format!(
                "{shape}, and that program builds or decodes the code it runs (`{marker}`) — the \
text on the command line is not the program that will execute, so blastguard cannot tell whether \
anything here is irreversible"
            ));
        }
    }

    // A SHELL ESCAPE WHOSE COMMAND IS COMPUTED. AppleScript's
    // `do shell script "…"` and JXA's `doShellScript("…")` hand a string to
    // `sh -c`; a literal argument was read by the descent above (and a
    // destructive one denied there), but `do shell script cmd` runs whatever
    // the variable holds, which is not on the command line (daf7611b).
    for payload in payloads {
        let lowered = payload.to_ascii_lowercase();
        for marker in ["do shell script", "doshellscript("] {
            let mut from = 0;
            while let Some(at) = lowered[from..].find(marker) {
                let after = lowered[from + at + marker.len()..].trim_start();
                if !after.starts_with(['"', '\'', '`']) {
                    return Decision::ask(format!(
                        "{shape}, and that program runs a shell command it computes \
(`{marker}` with a non-literal argument) — blastguard cannot see what would actually execute"
                    ));
                }
                from += at + marker.len();
            }
        }
    }

    if let Some(expanded) = payloads
        .iter()
        .filter(|_| expansion_possible)
        .find(|p| p.contains('$') || p.contains('`'))
    {
        let sample: String = expanded.trim().chars().take(60).collect();
        return Decision::ask(format!(
            "{shape}, and that program contains a shell expansion (`{sample}`) — the text on the \
command line is a template, not the program that will run, so blastguard cannot tell whether \
anything here is irreversible"
        ));
    }

    // No irreversible operation was found in the program text that WAS read.
    //
    // Operator ruling, 2026-09-09, verbatim: 「健全とは戻せない変更でもない限り
    // 自律的に実行できること。破壊的な変更を暗黙に実施しないこと。よみかきに
    // 一々許可をもとめるのは健全でもなんでもない。無駄」 — soundness is that
    // anything short of an unrecoverable change runs autonomously; asking about
    // reads and writes is waste.
    //
    // BE EXACT ABOUT WHAT THIS ALLOW CLAIMS, because the reason it replaces was
    // not exact. That reason said blastguard "cannot read that program", and
    // measured over 25 transcripts it fired 71 times — but it was false on its
    // face: the here-document body and the `-c` payload are both pushed into
    // `payloads` and both scanned above. The program WAS read. What the old
    // wording did was dress a policy default up as an inability, which is the
    // §4 failure aimed at whoever reads it.
    //
    // So, precisely: the text was read, it was descended through four layers of
    // quoting, and no irreversible operation was found in it. That is NOT proof
    // the program has no effects — a program can reach an effect without
    // spelling a recognisable name, and no command-line scan can settle that
    // (which is exactly why the v0.2.57 effect-token allowlist was unsound and
    // was reverted). It IS the operator's ruling that absence of evidence of
    // irreversibility resolves to Allow rather than to friction.
    //
    // THE RESIDUAL, STATED CORRECTLY. An earlier draft of this comment said the
    // leak was limited to effects reached "without naming them in text this
    // descent can see". That was false, and an adversarial reviewer measured it
    // false on 2026-09-09: `os.truncate('<an Obsidian note>', 0)` names its
    // effect in plain text, is read by the descent, and was allowed. The
    // residual was never "unnamed effects"; it was "named effects that are not
    // on a list" — a much larger set, and one that grows with every interpreter
    // API nobody enumerated. Writing the smaller, safer-sounding version of that
    // is the §4 failure aimed at the next reviewer.
    //
    // Fourteen of twenty-six mirror-shaped irreversible probes reached Allow
    // when that comment was written. What closed them was NOT a longer verb
    // list; it was moving the question onto the OBJECT — is the thing this
    // program writes recoverable? — which is the same substitution
    // `reversible.rs` exists for, and is decidable where "does this program
    // have effects" is not.
    //
    // What is left, stated so nobody has to rediscover it:
    //
    //   * A program that destroys a path it does not NAME — one it computes,
    //     reads from a file, or receives in `argv`. `CODE_BUILDING` catches the
    //     common constructs and the empty-set and expansion guards catch the
    //     unreadable shapes, but a program that assembles a pathname from
    //     fragments is not described by its own source and will be allowed.
    //   * A destructive verb whose spelling is on neither list AND whose object
    //     is recoverable-looking. The two lists are used ONLY to escalate, so a
    //     miss leaves the payload exactly where it was rather than allowing it —
    //     but a miss is still a miss.
    //   * Reading a protected gate file is surfaced (the object check above
    //     cannot tell a read from a write). That is friction the operator's
    //     ruling would call waste; it is kept because the gate-disabling axis is
    //     the one axis the ruling did not touch.
    //
    // With no human present `Decision::hardened` does not help here — it
    // collapses Ask to Deny and leaves Allow alone. Four things bound the
    // residual: `analyze_shell_payload` and `interpreter_destructive_call` on
    // every layer, the argv-list re-join, and the recoverability probe over the
    // literals of any payload that writes. Do not remove one without taking the
    // ruling again.
    //
    // One narrowing of the first of those was ruled (user ruling R2,
    // 2026-10-14): `analyze_shell_payload` runs here with
    // `Ctx::program_text_redirects` set on EVERY quote layer (a string literal
    // such as `'x > \`y\`'` is program text too, and is where the observed
    // false positives sat), so a truncating redirect whose TARGET cannot be
    // determined (shell expansion, or relative with no known base) is not a
    // Deny from that pass. It is not dropped either: the wrapper
    // `interpreter_code_verdict` turns it into an Ask carrying `RECOMMEND_NO`
    // (never Allow) whenever nothing else denies — so a literal that really is
    // a shell command (`os.system("echo hi > $X")`) is still put to a human,
    // and with no human present `Decision::hardened` makes it a Deny again.
    // Every other Deny of the shell pass, including a redirect onto an
    // existing unrecoverable file (`os.system("echo hi > /etc/hosts")`), is
    // unchanged. Nothing beyond that downgrade was ruled; the rest still needs
    // the ruling again.
    Decision::Allow
}

fn is_python_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether the `compile(` occurrence at byte offset `at` of `payload` is a
/// regex-module method call, `re.compile(` or `regex.compile(`, and so not the
/// builtin `compile` that turns a string into code (backlog 80b18990).
///
/// `at` is an offset into the ASCII-lowercased payload; lowercasing preserves
/// byte offsets, and every check here is made case-sensitively on the original
/// text (Python is case-sensitive: `RE.compile(` or `re.COMPILE(` is not
/// exempt).
///
/// Accepted only when ALL of these hold:
/// - the text at `at` is exactly `compile(`, immediately preceded by `re.` or
///   `regex.`, and the character before the module name is neither an
///   identifier character nor `.` (so `myre.compile(` and `x.re.compile(` are
///   not exempt);
/// - `module_bound_only_by_import` establishes that the name is bound by a
///   plain module-level `import` and is used nowhere else except as
///   `name.attr` reads.
///
/// What this does NOT prove: that the program cannot reach code execution by
/// some other route (e.g. `c = builtins.compile; c(src, 'f', 'exec')`, which
/// contains no `compile(` at all, or replacing `re.compile` through a module
/// handle obtained without writing the name). Such programs already evade the
/// `CODE_BUILDING` scan with or without this helper; this helper only removes
/// the misattribution of the regex call and never exempts an occurrence it
/// cannot positively identify — anything it does not recognise returns
/// `false`, so the marker still fires.
fn compile_call_is_regex_method(payload: &str, at: usize) -> bool {
    let Some(tail) = payload.get(at..) else {
        return false;
    };
    if !tail.starts_with("compile(") {
        return false;
    }
    let Some(head) = payload.get(..at) else {
        return false;
    };
    for module in ["re", "regex"] {
        let Some(before_module) = head.strip_suffix('.').and_then(|h| h.strip_suffix(module))
        else {
            continue;
        };
        if before_module
            .chars()
            .next_back()
            .is_some_and(|c| is_python_ident_char(c) || c == '.')
        {
            continue;
        }
        return module_bound_only_by_import(payload, module);
    }
    false
}

/// NOR IS TEXT THAT BUILDS ITS OWN CODE ([`interpreter_program_verdict`] and
/// [`python_file_body_verdict`] Ask on these). Same principle as the shell
/// expansion check, one level in: a program that decodes, concatenates or
/// looks up the name of what it is about to call is not described by its own
/// source. `getattr(__import__('os'), 'sys' + 'tem')('wipefs -a /dev/sda')`
/// contains no `os.system` for any scan to find, and
/// `exec(bytes.fromhex('…'))` contains no operation at all until it runs.
/// All four spellings were measured reaching Allow on 2026-09-09.
///
/// Be clear about what this is and is not. It IS a list, and a list can be
/// short. It is not the v0.2.57 mistake, because it does not enumerate
/// effects (open-ended, and a miss there meant "allow"); it enumerates the
/// constructs that make the text unfaithful to the program, and a miss means
/// the text is taken at face value — the same standing this arm already
/// gives text with no marker in it at all. Adding a spelling here is a
/// strict improvement; the set never has to be complete to be sound in the
/// direction it moves.
pub(super) const CODE_BUILDING: &[&str] = &[
    "exec(",
    "eval(",
    "compile(",
    "getattr(",
    "__import__",
    "__builtins__",
    "__getattribute__",
    "globals()[",
    "vars(",
    "sys.modules[",
    "b64decode",
    "fromhex(",
    "decode('hex'",
    "instance_eval",
    "class_eval",
    "kernel.send",
    ".send \"",
    ".send '",
    "new function(",
    "settimeout(\"",
    "settimeout('",
    // Execution reached through a PATH or a SERIALISED OBJECT rather than
    // through source text. Each of these runs code the command line does
    // not contain, so the text that was read describes the loader and not
    // the program. All measured reaching Allow on 2026-09-09.
    "runpy.",
    "run_path(",
    "import_module(",
    "pickle.load",
    "marshal.load",
    "functiontype(",
    "yaml.unsafe_load",
    "yaml.load(",
    // `os.system(open(…).read())` — a shell command assembled at run time.
    // The literal-argument spellings never reach here: the descent hands
    // them to `analyze_shell_payload`, which denies `os.system('mkfs …')`
    // several lines above.
    "system(open(",
    "system(f\"",
    "system(f'",
    "popen(open(",
];

/// The first [`CODE_BUILDING`] marker in `payload` (case-folded), if any.
/// `compile(` is the one marker with a common unrelated spelling:
/// `re.compile(` builds a regex, not code (backlog 80b18990). It is exempted
/// only occurrence by occurrence, and only when [`compile_call_is_regex_method`]
/// positively establishes the regex module; any other occurrence still fires.
pub(super) fn code_building_marker(payload: &str) -> Option<&'static str> {
    let lowered = payload.to_ascii_lowercase();
    CODE_BUILDING.iter().copied().find(|m| {
        if *m == "compile(" {
            lowered
                .match_indices("compile(")
                .any(|(at, _)| !compile_call_is_regex_method(payload, at))
        } else {
            lowered.contains(m)
        }
    })
}

/// Whether `name` is bound in `payload` only by a plain module-level import
/// and otherwise used only to read attributes of it.
///
/// Accepted only when ALL of these hold (checked case-sensitively):
/// - there is an UNINDENTED line that is `import <items>` where one
///   comma-separated item, trimmed, is exactly `name` (`import re`,
///   `import os, re`), and no triple quote (`'''` / `"""`) appears anywhere
///   before that line (so the line is not inside a multi-line string);
/// - every other whole-word occurrence of `name` comes after that line, is not
///   preceded by `.`, and is followed by `.` and an attribute name that does
///   not start with `_`, after which the next non-blank character is none of
///   `.`, `[`, `:`, or an assignment `=` (a lone `=`, or `=` following an
///   operator character other than in `==`);
/// - the payload contains no star import anywhere (`payload_has_star_import`):
///   `from m import *` can rebind `name` without spelling it.
///
/// This rejects the rebinding spellings that write the name
/// (`import builtins as re`, `from x import y as re`, `re = …`,
/// `from … import re`, `global re`, `for re in`, `def re(`, `class re`,
/// `lambda re`, `del re`, `re` passed as an argument, `'re'` in a string,
/// `re.compile = …`, `re.__dict__`) because none of them is an attribute read,
/// and the star import, which rebinds without writing the name. Rejection is
/// the conservative direction.
///
/// What this does NOT prove:
/// - that the module object has not been altered through a handle that never
///   spells `name` as a word (for example via `sys.modules.get('r' + 'e')`);
/// - that an imported module (e.g. a local `.py` file next to the program)
///   does not monkeypatch `re` / `re.compile` when it is imported. That code is
///   not in the program text. The gap predates this exemption: importing such
///   a module already runs arbitrary code that no scan of this text sees.
///
/// See `compile_call_is_regex_method`.
fn module_bound_only_by_import(payload: &str, name: &str) -> bool {
    if payload_has_star_import(payload) {
        return false;
    }
    // Locate the binding import line: [start, end) byte range.
    let mut binding: Option<(usize, usize)> = None;
    let mut offset = 0;
    for raw in payload.split_inclusive('\n') {
        let start = offset;
        offset += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']).trim_end();
        let Some(items) = line.strip_prefix("import ") else {
            continue;
        };
        if !items.split(',').any(|item| item.trim() == name) {
            continue;
        }
        let Some(before) = payload.get(..start) else {
            return false;
        };
        if before.contains("'''") || before.contains("\"\"\"") {
            return false;
        }
        binding = Some((start, offset));
        break;
    }
    let Some((bind_start, bind_end)) = binding else {
        return false;
    };

    for (at, _) in payload.match_indices(name) {
        let (Some(head), Some(tail)) = (payload.get(..at), payload.get(at + name.len()..)) else {
            return false;
        };
        let prev = head.chars().next_back();
        if prev.is_some_and(is_python_ident_char) {
            continue; // part of a longer identifier (`myre`)
        }
        if tail.chars().next().is_some_and(is_python_ident_char) {
            continue; // part of a longer identifier (`regex`, `rest`)
        }
        if at >= bind_start && at < bind_end {
            continue; // the binding import itself
        }
        if at < bind_start || prev == Some('.') {
            return false;
        }
        let Some(attr) = tail.strip_prefix('.') else {
            return false;
        };
        if !attr.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            return false;
        }
        let after_attr = attr.trim_start_matches(is_python_ident_char);
        let after_attr = after_attr.trim_start_matches([' ', '\t']);
        let mut chars = after_attr.chars();
        let next = chars.next();
        let second = chars.next();
        match next {
            Some('.' | '[' | ':') => return false,
            Some('=') if second != Some('=') => return false,
            Some('+' | '-' | '*' | '/' | '%' | '&' | '|' | '^' | '<' | '>' | '@' | '!')
                if second == Some('=') || matches!(second, Some('<' | '>' | '*' | '/')) =>
            {
                // Augmented assignment (`+=`, `<<=`, `**=`, `//=`, …) or a
                // comparison; either way not a plain attribute read we vouch for.
                return false;
            }
            _ => {}
        }
    }
    true
}

/// Whether `payload` may contain a star import (`from m import *`).
///
/// Deliberately over-approximate (fail-closed): it returns true when ANY
/// occurrence of the text `import`, anywhere in the payload (indented or not,
/// after `;`, inside a string or comment), is followed — after any run of
/// whitespace, newlines, `\` line continuations and `(` — by `*`. That covers
/// `import *`, `import*`, `import (*`, `import(*` and
/// `import \` + newline + `*`. A false positive only means the `re.compile(`
/// exemption is withheld and the marker asks.
fn payload_has_star_import(payload: &str) -> bool {
    payload.match_indices("import").any(|(at, kw)| {
        payload
            .get(at + kw.len()..)
            .unwrap_or("*")
            .trim_start_matches(|c: char| c.is_whitespace() || c == '\\' || c == '(')
            .starts_with('*')
    })
}
