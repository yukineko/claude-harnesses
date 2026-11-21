use super::{
    exclude, first_shell_word, glob_literal_prefix, has_glob_meta, has_unresolvable_expansion,
    is_code_interpreter, is_exec_wrapper, is_shell, positional_operands, quote_aware_words,
    takes_shell_command_flag, Decision,
};

/// Commands that only READ their operands. Reading a gate's config
/// (`cat .claude/settings.json`, `grep hooks .githooks/pre-commit`) is routine
/// work and must stay Allow, so this set is what makes the unrecognised-verb
/// rule in [`unknown_verb_protected_ask`] affordable at all.
///
/// The list runs in the CLOSED direction, which is the whole reason it is
/// spelled this way round. A list of DESTRUCTIVE verbs rots open: every verb
/// nobody thought of (`unlink`, `ditto`, `ex`, `awk -i inplace`) is a free
/// bypass the day it is invented, which is exactly how finding 3 was produced.
/// A list of READ-ONLY verbs rots closed: a verb nobody thought of resolves to
/// Ask when it touches a protected path, which is what CLAUDE.md §3 asks of
/// "cannot determine". The residual cost is a false Ask on some unlisted
/// read-only tool aimed at a gate file — recoverable by a human, or by adding
/// the name here.
///
/// Shells, code interpreters and the shell-evaluation words (`eval`, `exec`,
/// `source`, `.`) are NOT listed here: they are exempted separately in
/// [`unknown_verb_protected_ask`] because they have dedicated analysis above
/// (payload re-analysis, the inline-eval-flag deny). RUNNING a hook script or
/// sourcing an rc file (`. ~/.bashrc`, `source .venv/bin/activate`) is not
/// modifying it, and what the payload then does is judged by the recursive
/// analysis, not by this rule.
const READ_ONLY_COMMANDS: &[&str] = &[
    // viewers / pagers
    "cat",
    "bat",
    "head",
    "tail",
    "less",
    "more",
    "nl",
    "od",
    "xxd",
    "hexdump",
    "strings",
    // search
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ack", // listing / metadata
    "ls",
    "stat",
    "file",
    "du",
    "df",
    "wc",
    "realpath",
    "readlink",
    "dirname",
    "basename",
    "pwd",
    // comparison
    "diff",
    "cmp",
    "comm", // digests
    "md5",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "sha512sum",
    "shasum",
    "cksum",
    // text transforms that write only to stdout.
    //
    // Round 4 (adversarial verifier): `sort` and `uniq` were HERE and are the
    // reason this list is not a set of "commands that only READ their operands"
    // — they both have OUTPUT-FILE forms (`sort -o FILE`, `uniq [IN [OUT]]`)
    // that TRUNCATE the named file (proven: each zeroed a real file). A command
    // that can write must not be silently exempted by a read-only rule, so they
    // are removed from this list and given dedicated arms (`analyze_sort`,
    // `analyze_uniq`) that Deny the output-onto-a-protected-path forms while
    // leaving pure reads Allow. The remaining entries write ONLY to stdout.
    "cut",
    "tr",
    "fold",
    "column",
    "paste",
    "join",
    "rev",
    // structured-data readers
    "jq",
    "yq", // shell builtins / navigation that create nothing destructive
    "echo",
    "printf",
    "true",
    "false",
    "test",
    // `[` is `test` under another name, and `[[` is bash's conditional
    // expression: both only evaluate a condition over their operands (a
    // command substitution inside one is judged on its own). Before the
    // compound-syntax fix (1875f626) they were reached as the "unknown verb"
    // of `if [ -f .githooks/pre-commit ]; then …`.
    "[",
    "[[",
    "which",
    "type",
    "whereis",
    "cd",
    "pushd",
    "popd",
    "mkdir",
    "export",
    "set",
    "unset",
];

fn is_read_only_command(cmd: &str) -> bool {
    READ_ONLY_COMMANDS.contains(&cmd)
}

/// `rg --pre CMD` / `rg --pre=CMD` runs `CMD FILE` for every searched file, so
/// an `rg` carrying it is NOT data-only even though plain `rg` is read-only
/// (1f037eb5). Matched on the exact flag token (`--pre-glob` only selects which
/// files get preprocessed and executes nothing). Any `--pre` token counts,
/// including one after `--`: over-asking is the restrictive side. The value of
/// `--pre-glob` is deliberately NOT skipped as a flag value below: that skip
/// applied to every head and let `gzip -- --pre-glob <protected>` through;
/// leaving it an operand only over-asks for `rg` (the restrictive side).
///
/// `ag --pager CMD` / `ack --pager=CMD` is the same hazard (e33849bd): the
/// pager is a caller-chosen program that is fed the matched lines (protected
/// file content), so `--pager sh` runs it. The option name (the part before
/// `=`) counts when it is `--pager` or an unambiguous long-option abbreviation
/// of it at least `--pag` long (getopt_long / Getopt::Long accept `--pag`,
/// `--page`). `--pager-glob`, `--pagerx`, `--nopager` and `--no-pager` are NOT
/// matched. As for `rg`, any such token counts even after `--`, and no
/// flag-value skip is added.
fn has_exec_flag(cmd: &str, rest: &[&str]) -> bool {
    let any_word = |f: &dyn Fn(&str) -> bool| {
        rest.iter().any(|t| {
            let w = first_shell_word(t).unwrap_or_else(|| (*t).to_string());
            f(&w)
        })
    };
    match cmd {
        "rg" => any_word(&|w| w == "--pre" || w.starts_with("--pre=")),
        "ag" | "ack" => any_word(&|w| {
            let name = w.split('=').next().unwrap_or(w);
            name.len() >= 5 && "--pager".starts_with(name)
        }),
        _ => false,
    }
}

/// `ACK_PAGER=CMD ack …` / `ACK_PAGER_COLOR=CMD ack …` (ba09708a): ack
/// documents both variables as "a pager program ... to which ack will send its
/// output" and `--pager` as "This can also be specified via the ACK_PAGER and
/// ACK_PAGER_COLOR environment variables" — the same caller-chosen program fed
/// the matched lines that [`has_exec_flag`] refuses for `--pager` (e33849bd).
///
/// `prefix` is the segment's words before the command word. Any of them whose
/// name part (before `=`, a trailing `+` of `NAME+=` dropped, one level of
/// quoting removed) is one of the two variables counts, WHATEVER the value:
/// `ACK_PAGER=less`, `ACK_PAGER=` and an unresolvable `ACK_PAGER="$X"` all
/// lose the exemption, as every `--pager=…` value does. A quoted prefix word
/// (`'ACK_PAGER=sh' ack`) is a command name to bash, not an assignment;
/// matching it anyway only over-asks (the restrictive side).
///
/// This function sees only the ack segment's own prefix. An EARLIER segment of
/// the same command line that sets the variable (`export ACK_PAGER=sh; ack …`,
/// `declare -x ACK_PAGER=sh; ack …`) is judged by [`line_sets_ack_pager_before`]
/// (a700901f) instead. NOT covered by either (the variable reaches ack from
/// outside the command line, or through a word neither parses as an
/// assignment): an inherited environment, `ACKRC` / `.ackrc` carrying
/// `--pager`, `eval 'export ACK_PAGER=sh'`, `read ACK_PAGER`, `printf -v
/// ACK_PAGER …`. `ag` reads no pager variable (its only `getenv` calls are
/// `TERM`, `HOME` and `XDG_CONFIG_HOME`), so only `ack` is keyed here.
fn env_selects_pager(cmd: &str, prefix: &[&str]) -> bool {
    cmd == "ack" && prefix.iter().any(|t| names_ack_pager(t, false))
}

/// True when the shell word `raw` (one level of quoting removed) is an
/// assignment `NAME=…` / `NAME+=…` to `ACK_PAGER` or `ACK_PAGER_COLOR`, or,
/// when `bare_ok`, the bare name itself (`export ACK_PAGER`). The value is
/// never inspected.
fn names_ack_pager(raw: &str, bare_ok: bool) -> bool {
    let w = first_shell_word(raw).unwrap_or_else(|| raw.to_string());
    let name = match w.split_once('=') {
        Some((name, _)) => name.strip_suffix('+').unwrap_or(name),
        None if bare_ok => w.as_str(),
        None => return false,
    };
    matches!(name, "ACK_PAGER" | "ACK_PAGER_COLOR")
}

/// True when `word` (unquoted) is a shell assignment word `NAME=…` /
/// `NAME+=…` with a valid identifier `NAME` — the words bash takes as
/// assignments at the head of a simple command.
fn is_assignment_word(raw: &str) -> bool {
    let w = first_shell_word(raw).unwrap_or_else(|| raw.to_string());
    w.split_once('=').is_some_and(|(name, _)| {
        let name = name.strip_suffix('+').unwrap_or(name);
        let mut cs = name.chars();
        cs.next()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
            && cs.all(|c| c == '_' || c.is_ascii_alphanumeric())
    })
}

/// True when a segment STRICTLY BEFORE `seg_idx` of `line` sets `ACK_PAGER` /
/// `ACK_PAGER_COLOR` by a SHELL ASSIGNMENT (a700901f). Only two word positions
/// count, so a word that merely contains the text (`grep ACK_PAGER= README`,
/// `printf ACK_PAGER=x`) does not:
///
/// * the segment's leading assignment words, before its command word
///   (`ACK_PAGER=sh;` updates an inherited exported variable). This also
///   counts a prefix on another command (`ACK_PAGER=sh other_cmd; ack …`),
///   which bash does NOT persist — counted anyway, on the restrictive side;
/// * the operands of a declaration builtin `export` / `declare` / `typeset` /
///   `readonly` / `local` (optionally behind `builtin` / `command`), where a
///   bare name also counts (`export ACK_PAGER`, `declare -x ACK_PAGER`), and
///   options are skipped (`export -- ACK_PAGER=sh`, `export X=1
///   ACK_PAGER=sh`). Without `-x` a declaration still updates an inherited
///   exported variable, so the option is not required.
///
/// The value is ignored, as in [`env_selects_pager`]: an undeterminable
/// `export ACK_PAGER="$X"` counts. Leading `(` / `{` and the reserved words
/// `!`, `then`, `do`, `else`, `time`, … are skipped before the command word.
/// When `seg_idx` is past the end of the line's segments the earlier text
/// cannot be located, which resolves to the restrictive `true`.
pub(super) fn line_sets_ack_pager_before(line: &str, seg_idx: usize) -> bool {
    let segs = super::split_segments(line);
    if seg_idx > segs.len() {
        return true;
    }
    segs.iter().take(seg_idx).any(|seg| {
        let seg = seg.trim_start_matches(|c: char| c == '(' || c == '{' || c.is_whitespace());
        let words = quote_aware_words(seg);
        let mut it = words.iter().map(String::as_str).peekable();
        const SKIP: &[&str] = &[
            "!", "{", "(", "if", "then", "elif", "else", "while", "until", "do", "time",
        ];
        while it.next_if(|w| SKIP.contains(w)).is_some() {}
        while let Some(w) = it.next_if(|w| is_assignment_word(w)) {
            if names_ack_pager(w, false) {
                return true;
            }
        }
        let wrapper = |w: &&str| matches!(*w, "builtin" | "command");
        while it.next_if(wrapper).is_some() {}
        let declares = it.next().is_some_and(|head| {
            let head = first_shell_word(head).unwrap_or_else(|| head.to_string());
            matches!(
                head.as_str(),
                "export" | "declare" | "typeset" | "readonly" | "local"
            )
        });
        declares && it.any(|w| !w.starts_with('-') && names_ack_pager(w, true))
    })
}

/// a700901f: the verdict for `ack` at `tokens[idx]` when an earlier segment of
/// `line` sets its pager variable ([`line_sets_ack_pager_before`]) — the
/// variable reaches ack exactly as an assignment prefix does, so it is judged
/// as one: a protected operand asks, anything else is `Allow`. `Allow` for any
/// other command word or when no earlier segment sets it.
pub(super) fn ack_exported_ask(
    tokens: &[&str],
    idx: usize,
    line: &str,
    seg_idx: usize,
) -> Decision {
    if super::normalized_command(tokens[idx]) != "ack" || !line_sets_ack_pager_before(line, seg_idx)
    {
        return Decision::Allow;
    }
    unknown_verb_protected_ask_after("ack", &["ACK_PAGER=set-earlier"], &tokens[idx + 1..])
}

/// True when `cmd` is a read-only command whose words after the head are DATA
/// (a pattern, paths, text), never a command line the program runs. The
/// unknown-head tail rule (`unknown_wrapper_ask`) must not re-read them: with
/// `grep -rn truncate <glob>` the pattern word `truncate` was taken for the
/// start of a `truncate` command line and the whole command was refused.
///
/// This is an explicit ALLOWLIST, not "READ_ONLY_COMMANDS minus a denylist":
/// only heads that cannot execute any argument under any option qualify
/// (`grep`/`egrep`/`fgrep`, `cat`, `head`, `tail`, `wc`, `ls`, `stat`, `du`).
/// Every other read-only head keeps the tail rule because some option can run
/// its argument: `bat --pager <cmd>`, `less`/`more` (`+cmd`, LESSOPEN),
/// `rg --pre <prog>`, `ag`/`ack --pager`. So `bat --pager rm -rf /etc` and
/// `rg --pre rm …` still ask, and `ag`/`ack` carrying a `--pager` option (or an
/// abbreviation of it) are not read-only-exempt for a protected operand
/// ([`has_exec_flag`], e33849bd).
pub(super) fn argv_is_data_only(cmd: &str) -> bool {
    is_read_only_command(cmd)
        && matches!(
            cmd,
            "grep" | "egrep" | "fgrep" | "cat" | "head" | "tail" | "wc" | "ls" | "stat" | "du"
        )
}

/// Verdict for a command this module has NO rule for, when one of its operands
/// names (or contains) a protected gate/hook/policy path.
///
/// Round 2 (adversarial verifier). The catch-all arm of the dispatch was
/// `other => Allow` (modulo `mkfs`), so every verb without an arm reached every
/// protected path unclassified — measured ALLOW on 0.2.19: `unlink <protected>`,
/// `rmdir .githooks`, `patch -p1 <protected>`, `ed`/`ex` scripts, `awk -i
/// inplace`, `gzip`/`bzip2`/`xz` (which REPLACE their input), `rsync -a --delete
/// … .claude/hooks/`, `ditto`, `tar -x -C .claude`, `unzip -d`.
///
/// The choice made here, stated explicitly because the alternative was on the
/// table: this is a GENERAL PRINCIPLE with a read-only exemption, not a list of
/// destructive verbs. An unknown verb pointed at a protected path is the literal
/// definition of "cannot determine" — blastguard does not know whether it reads,
/// rewrites or deletes — and CLAUDE.md §3 says that resolves to the restrictive
/// side. `Ask` rather than `Deny` because the honest content of the verdict is a
/// refusal to guess, not a claim about the command (`model.rs`: "it is NOT a
/// verdict about the command, it is a refusal to guess about one"); with no
/// human present `Decision::hardened` collapses it to Deny anyway, so the
/// restrictive resolution holds either way.
///
/// A verb-list version of this rule was rejected on the evidence: the list in
/// the finding above IS the list somebody would have written in round 1, and it
/// was assembled by an adversary probing round 1's gaps, not by foresight.
///
/// OPERANDS ARE SHELL WORDS, NOT WHITESPACE FRAGMENTS (1875f626). `rest` comes
/// from the segment tokeniser, which splits on whitespace only, so a quoted
/// argument arrived here in pieces: `backlog add "harden .githooks/pre-commit"`
/// yielded the "operand" `.githooks/pre-commit"` (quote attached), and a
/// quoted multi-line `--notes "…\n.githooks/pre-commit …"` yielded the bare
/// line fragment as if it were a path argument. Both are FREE TEXT inside one
/// argument, not a path the program was handed. Whether a fragment matched was
/// an accident of glob shape: `.githooks/pre-commit"` matched (`.githooks/**`
/// takes anything below it, stray quote included) while the fragment
/// `.claude/settings.json"` of `"see .claude/settings.json"` matched nothing
/// (measured on 0.2.78: the first asked, the second was allowed). So the
/// operands are re-read here as whole shell words ([`quote_aware_words`]) with
/// one level of quoting removed ([`first_shell_word`]) before the path test.
///
/// A command word that is itself an unexpanded expansion (`$B …`) is not an
/// unknown VERB — it is an unknown PROGRAM, the refusal
/// `unresolvable-command-word` already names (`unknown_wrapper_ask`). It still
/// asks here (the program could be anything, including a writer), worded as
/// that refusal so the two classes stay separable in `rule_id`.
pub(super) fn unknown_verb_protected_ask(cmd: &str, rest: &[&str]) -> Decision {
    unknown_verb_protected_ask_after(cmd, &[], rest)
}

/// [`unknown_verb_protected_ask`] for a command word that stands behind the
/// words `prefix` (the segment's tokens before it: leading `NAME=value`
/// assignments and any wrapper words). An `ack` whose environment selects a
/// pager through such an assignment loses the read-only exemption exactly as
/// `ack --pager` does ([`env_selects_pager`], ba09708a).
pub(super) fn unknown_verb_protected_ask_after(
    cmd: &str,
    prefix: &[&str],
    rest: &[&str],
) -> Decision {
    if (is_read_only_command(cmd) && !has_exec_flag(cmd, rest) && !env_selects_pager(cmd, prefix))
        || is_shell(cmd)
        || is_code_interpreter(cmd)
        || matches!(cmd, "eval" | "exec" | "source" | ".")
    {
        return Decision::Allow;
    }
    if cmd == "backlog" && !backlog_writes_operand_paths(rest) {
        return Decision::Allow;
    }
    let words = quote_aware_words(&rest.join(" "));
    let raw: Vec<&str> = words.iter().map(String::as_str).collect();
    for raw_op in positional_operands(&raw, &[]) {
        let op = first_shell_word(raw_op).unwrap_or_else(|| raw_op.to_string());
        let hit = if has_glob_meta(&op) {
            glob_literal_prefix(&op)
                .map(|p| exclude::touches_protected(&p))
                .unwrap_or(false)
        } else {
            exclude::touches_protected(&op)
        };
        if hit {
            if has_unresolvable_expansion(cmd) {
                return Decision::ask(format!(
                    "the command word `{cmd}` is an expansion whose value only exists at run \
time, and {op} is a protected gate/config path — blastguard cannot tell what program this runs \
or what it does to that path"
                ));
            }
            return Decision::ask(format!(
                "`{cmd}` is a command blastguard has no rule for, and {op} is a protected \
gate/config path — blastguard cannot tell whether this reads it, rewrites it or removes it, \
and refuses to guess"
            ));
        }
    }
    Decision::Allow
}

/// True when a `backlog` invocation may WRITE to a path it was handed as an
/// operand.
///
/// `backlog` is this repository's task-queue CLI (`crates/backlog/src/main.rs`,
/// clap `Command` enum). Its positional operands and flag values are task
/// TEXT and ids (`add --title … --notes …`, `done <id>`, `edit <id>`), plus a
/// `--project` path that only LOCATES the store (the write goes to the store
/// file under that project's root, never to the path given). Free text that
/// mentions `.githooks/pre-commit` is a note about the gate, not an operation
/// on it, and asking about it was pure friction (1875f626: 14 recurrences).
///
/// The one exception is `merge-driver BASE OURS THEIRS`, which writes the
/// merged result INTO `OURS` — a real file operand — so any invocation that
/// contains that subcommand word keeps the generic unknown-verb rule. Matching
/// the word ANYWHERE (not only in first position) is deliberate: it keeps the
/// exception if a global option is ever added in front of the subcommand, at
/// the cost of a rare ask for free text consisting of exactly that word.
///
/// Scope, stated so a reviewer can check it rather than trust it: this is
/// keyed on the program NAME. A different program installed as `backlog`
/// would inherit it. Every other subcommand either writes only the store,
/// writes a fixed file it does not take as an operand (`install` /
/// `uninstall` edit `~/.claude/settings.json` — not an operand, so this rule
/// never saw it before either), or is rejected by clap without side effects
/// (an unknown subcommand such as `show`). If a new subcommand that writes an
/// operand path is added to `backlog`, it must be added here.
fn backlog_writes_operand_paths(rest: &[&str]) -> bool {
    rest.iter()
        .any(|t| first_shell_word(t).as_deref() == Some("merge-driver"))
}

/// True when `cmd` is a command word this module has an actual opinion about —
/// either a rule arm in `analyze_command_at`, a re-analysis arm, or a wrapper
/// whose payload is already followed.
///
/// Kept in lockstep with `analyze_command_at`'s `match` and the arms above it;
/// `unknown_wrapper_ask_covers_every_unrecognized_head` pins that a head this
/// returns false for really does fall through to the catch-all Allow arm.
pub(super) fn is_recognized_command(cmd: &str) -> bool {
    // Rule arms in `analyze_command_at`'s match (the `mkfs` arm is a prefix).
    matches!(
        cmd,
        "rm" | "git"
            | "find"
            | "xargs"
            | "truncate"
            | "shred"
            | "dd"
            | "chmod"
            | "chown"
            | "tee"
            | "eval"
            | "exec"
            | "source"
            | "."
            | "-exec"
            | "-execdir"
            | "-ok"
            | "-okdir"
            | "curl"
            | "wget"
            | "nc"
            | "ncat"
            | "netcat"
    ) || cmd.starts_with("mkfs")
        // Re-analysis / wrapper arms.
        || is_shell(cmd)
        || is_code_interpreter(cmd)
        || is_exec_wrapper(cmd)
        || takes_shell_command_flag(cmd)
}
