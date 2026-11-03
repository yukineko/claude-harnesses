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
    if is_read_only_command(cmd)
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
