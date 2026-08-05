//! Static recognition of Bash commands that cannot write anything.
//!
//! # Why this exists (backlog a4b59893)
//!
//! `gate`'s own message says "**write-class** tools are downgraded", but its
//! PreToolUse matcher is `Bash|Write|Edit|MultiEdit|NotebookEdit` — the whole
//! `Bash` tool, not the write-class subset of it. Measured consequence: after a
//! single external `Read`, `git status`, `git log` and `git worktree list` were
//! denied too, so the turn could not even be *diagnosed*, and a non-interactive
//! worker (condukt/flow) had no route back except a human re-invocation. The
//! prose and the behaviour disagreed, and the prose was the honest one.
//!
//! This module restores the stated contract by classifying a command as
//! read-only or not. It is **not** a relaxation of the taint invariant: a
//! command that cannot write is not a write-class tool, so allowing it never
//! lets tainted provenance reach a mutation. What it does not do — and must not
//! be mistaken for — is stop a tainted turn from *reading* more; taint is
//! consumed by `mark`'s PostToolUse matcher, which never watched `Bash` in the
//! first place, so this changes nothing about what the turn can read.
//!
//! # Fail-closed by construction (CLAUDE.md §3)
//!
//! Every step answers "is this *positively* known to be read-only?", and every
//! unrecognised shape — an unknown program, an unknown `git` subcommand, a
//! quote this tokenizer would have to guess about, any shell metacharacter that
//! could redirect, substitute, or chain — answers `false`, i.e. "gate it".
//! `false` is the safe direction here: it means the command is treated exactly
//! as 0.1.10 treated every command.
//!
//! There is deliberately no "looks harmless" arm and no regex over the whole
//! string. The unit of decision is a token, and a token is either in a table or
//! it is not.
//!
//! # Why the arguments are allowlisted too, and not denylisted
//!
//! The first implementation of this module allowlisted *programs* and
//! denylisted *flags* (a `WRITE_CAPABLE_FLAG_PREFIXES` table of `--output`,
//! `-o`, `--exec`, `--config`). That hybrid leaked, repeatedly and measurably:
//! **thirteen** reachable write/exec paths were found in it — seven during the
//! author's own review, six more by an independent verifier afterwards, each
//! round finding holes the previous round had declared complete. Measured
//! 2026-08-05; the verifier observed every one of its six against the real
//! `is_readonly_bash`, and demonstrated one destructively (`uniq in.txt
//! victim.txt` replaced `victim.txt`'s contents — `uniq`'s second *positional*
//! operand is its output file, so no flag table could ever have caught it).
//!
//! The others were `rg --pre <cmd>`, `rg --hostname-bin=<cmd>`,
//! `sort --compress-program=PROG`, `git grep -O<pager>` /
//! `--open-files-in-pager=<pager>` (the `-o` prefix match was case-sensitive,
//! so `-O` walked past it), and `git ls-remote <url>` (an outbound channel out
//! of a tainted turn, plus `--upload-pack=<exec>`).
//!
//! A denylist cannot state the property this module needs. "None of these
//! flags is present" is not "this writes nothing" — it is "none of the writers
//! I happened to think of is present", and there is no way to observe that the
//! enumeration finished. That is precisely the shape CLAUDE.md §3 rejects, and
//! §6's warning that the implementer drifts toward the permissive side is what
//! the 13:0 score records. So the argument side is inverted to match the
//! program side:
//!
//! * a `--long` flag must appear by name in *that program's* table (its
//!   `--long=value` form is checked by name too, so `--pre`, `--upload-pack`
//!   and `--compress-program` are refused for being absent, not for being
//!   recognised as dangerous);
//! * every letter of a `-abc` bundle must appear in *that program's* short
//!   table (digits are allowed anywhere, since a digit is a count, never a
//!   verb) — which is why `-O` is now refused for `git` while `-o` stays
//!   available to `grep` (only-matching) and stays refused for `sort` (output
//!   file). The old uniform prefix rule could not express that distinction and
//!   paid for it in both directions;
//! * operands are *counted*, and a program whose Nth positional is an output
//!   file simply caps at N-1 (`uniq` caps at 1).
//!
//! Every table below is therefore a list of things positively known to read,
//! and the answer for anything not listed is `false`. Adding an entry is a
//! deliberate act with a stated reason; forgetting one costs a gated command,
//! not a silent write.
//!
//! # Known residuals (stated, not fixed, in 0.2.0)
//!
//! * `git show`/`log`/`diff`/`blame` can run a repository-configured
//!   `textconv` or `diff.external` program. The repository is the project the
//!   turn is already working in, so this is not a channel the *tainted content*
//!   opens, but it is a way a command in these tables executes something.
//! * `git status` updates `.git/index`'s stat cache and takes `index.lock`, so
//!   it does write — internal bookkeeping, not user data, but the admission
//!   rule below says "only inspect" and this is a carve-out from it.

/// Shell metacharacters that can redirect output, substitute a command, chain a
/// second command, or background one. Any of these anywhere in the command
/// makes it unclassifiable by this module — `>` and `>>` write files, `` ` ``
/// and `$(` run arbitrary programs, `;`/`&&`/`&` append arbitrary programs, and
/// `<` can feed a here-doc whose body this tokenizer does not model.
///
/// `|` is NOT in this list: a pipeline is handled below by requiring EVERY
/// segment to be read-only. `||` survives the split as an empty segment, which
/// no table matches, so it is rejected there rather than here.
/// The glob characters `*` `?` `[` `]` are here for a reason that is not about
/// redirection, and it is the sharpest illustration of this module's thesis.
///
/// A glob is ONE token to this classifier and N arguments to the program, and
/// the expansion happens AFTER classification. So `uniq *` presents a single
/// operand, passes `uniq`'s cap of one, and then bash hands `uniq` however many
/// files matched — the second of which it overwrites. Measured 2026-08-05 by
/// the independent verifier, destructively: a file containing `PRECIOUS DATA
/// THAT MUST SURVIVE` was replaced by the deduplicated contents of another.
/// That is hole 8 reopened by a different spelling, one round after the operand
/// counter was introduced to close it.
///
/// The lesson is the same one the flag denylist taught: an operand *count*
/// cannot state "this writes nothing" when the operand *list* is decided after
/// the count is checked. A glob makes the count undeterminable — and also makes
/// it undeterminable whether an expansion begins with `-`, i.e. whether it
/// arrives as a flag. Cannot-determine resolves to the restricted side
/// (CLAUDE.md §3), so the whole command is gated. The cost is that `grep foo
/// *.rs` is now gated; the alternative is a demonstrated file destruction.
const FORBIDDEN_CHARS: &[char] = &[
    ';', '&', '>', '<', '`', '\n', '\r', '(', ')', '{', '}', '$', '*', '?', '[', ']',
];

/// Quote characters. A quoted command is not *unsafe*, it is *unparsed*: this
/// tokenizer splits on whitespace, so `git log --format='%h %s'` would split
/// mid-argument and be reasoned about wrongly. Refusing to classify is the
/// honest answer (CLAUDE.md §3), not a claim that the command is dangerous.
const QUOTE_CHARS: &[char] = &['\'', '"'];

/// One admitted program, with the complete set of argument shapes it is
/// admitted in.
///
/// `short`/`long` are exhaustive: a flag absent from them refuses the command.
/// They are deliberately smaller than each tool's real surface — a missing
/// read-only flag costs a gated command, which is the price this module is
/// willing to pay, while a missing *write* flag is the failure it exists to
/// prevent.
struct Program {
    /// Bare program name, matched exactly.
    name: &'static str,
    /// Letters admissible inside a `-abc` bundle. ASCII digits are admitted
    /// for every program without being listed (`head -5`, `sort -k2`): a digit
    /// is an argument to the preceding letter, never a verb of its own.
    short: &'static str,
    /// Long flags admissible by name, with or without an `=value` suffix.
    long: &'static [&'static str],
    /// How many non-flag operands the program may take before it starts
    /// treating one as an output file. `usize::MAX` where every positional is
    /// an input.
    max_operands: usize,
}

/// No positional of this program is ever an output.
const ANY_OPERANDS: usize = usize::MAX;

/// Programs whose every invocation reads and prints. Each entry is a program
/// that has no write-capable flag at all — which is why `find` (`-delete`,
/// `-exec`), `sed` (`-i`), `awk` (`> file` inside a program), `xargs`, and every
/// interpreter (`sh`, `bash`, `python`, `node`, `perl`) are absent and must stay
/// absent: they are general-purpose executors wearing a read-only-looking name.
///
/// FOUR entries were removed from the first draft of this table during review,
/// and the reason each looked safe is the reason to distrust the eyeball test
/// for the next addition. Every one of them is a program whose NAME reads as a
/// pure query:
///
/// * **`env`** prints the environment when called bare, but `env FOO=bar <cmd>`
///   EXECS `<cmd>`. It is an interpreter with a read-only-looking name — the
///   exact class the paragraph above says must stay out — and it would have
///   made `env rm -rf x` read-only.
/// * **`date`** prints the time, but `date -s <string>` / `--set` writes the
///   system clock.
/// * **`hostname`** prints the host name, but `hostname <name>` SETS it. Like
///   `env`, the write is reachable through a bare argument, not a flag.
/// * **`file`** identifies a file's type, but `file -C -m <magfile>` COMPILES a
///   magic file and writes `<magfile>.mgc` next to it.
///
/// `date`/`hostname`/`file` need root or an unusual invocation to do damage,
/// which is exactly why they survived the first pass; "unlikely" is not the
/// admission rule. The rule is "no write-capable form at all", and each of the
/// four fails it.
///
/// `sort` and `uniq` DO stay, despite `sort -o FILE` and `uniq IN OUT` writing
/// `FILE`/`OUT`, because neither shape is expressible under the tables below:
/// `o` is absent from `sort`'s `short`, `--output`/`--compress-program` are
/// absent from its `long`, and `uniq`'s `max_operands` is 1. That is the only
/// admissible difference — a write that cannot be *spelled* is unreachable,
/// while `env`'s and `hostname`'s are reachable through a bare argument that no
/// flag table constrains and `file`'s through an ordinary flag.
///
/// `uniq`'s second operand is the case that proves the point: it was admitted
/// under the old flag denylist and stayed reachable there no matter what the
/// denylist contained, because it is not a flag at all.
const READONLY_PROGRAMS: &[Program] = &[
    Program {
        name: "pwd",
        short: "LP",
        long: &["logical", "physical"],
        max_operands: 0,
    },
    Program {
        name: "whoami",
        short: "",
        long: &[],
        max_operands: 0,
    },
    Program {
        name: "uname",
        short: "amnprsvio",
        long: &[
            "all",
            "machine",
            "nodename",
            "processor",
            "hardware-platform",
            "operating-system",
            "kernel-name",
            "kernel-release",
            "kernel-version",
        ],
        max_operands: 0,
    },
    Program {
        name: "ls",
        short: "aAbBcCdDfFgGhHiIklLmnNopqQrRsStuUvxXZ1",
        long: &[
            "all",
            "almost-all",
            "long",
            "human-readable",
            "reverse",
            "recursive",
            "sort",
            "time",
            "directory",
            "classify",
            "color",
            "inode",
            "size",
            "time-style",
            "group-directories-first",
            "numeric-uid-gid",
            "full-time",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "wc",
        short: "clmwL",
        long: &["lines", "words", "bytes", "chars", "max-line-length"],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "echo",
        short: "neE",
        long: &[],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "head",
        short: "cnqvz",
        long: &[
            "lines",
            "bytes",
            "quiet",
            "silent",
            "verbose",
            "zero-terminated",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        // `-f`/`-F` (follow) are absent, but NOT because "a gate that hangs is
        // a gate that gets removed" — the earlier draft said that, and it was
        // false by this table's own contents: bare `cat`, `sort`, `grep PAT`,
        // `wc`, `cut`, `tr`, `nl`, `column` and `uniq` all read stdin and block
        // identically, and every one of them is admitted (a unit test even
        // asserts `sort` is). Non-termination is not what this module decides.
        // They are absent for the ordinary reason: nobody needed them, and an
        // unlisted flag costs a gated command.
        name: "tail",
        short: "cnqvz",
        long: &[
            "lines",
            "bytes",
            "quiet",
            "silent",
            "verbose",
            "zero-terminated",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "cat",
        short: "AbeEnstTuv",
        long: &[
            "number",
            "number-nonblank",
            "show-ends",
            "show-tabs",
            "show-all",
            "squeeze-blank",
            "show-nonprinting",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        // `-o` is only-matching here and is admitted; the same letter stays out
        // of `sort`'s table, which a uniform prefix rule could not express.
        name: "grep",
        short: "EFGPHILRUZabcdefhiklmnoqrsvwxyz",
        long: &[
            "extended-regexp",
            "fixed-strings",
            "basic-regexp",
            "perl-regexp",
            "regexp",
            "file",
            "ignore-case",
            "word-regexp",
            "line-regexp",
            "count",
            "only-matching",
            "quiet",
            "silent",
            "no-messages",
            "invert-match",
            "line-number",
            "with-filename",
            "no-filename",
            "byte-offset",
            "recursive",
            "dereference-recursive",
            "include",
            "exclude",
            "exclude-dir",
            "files-with-matches",
            "files-without-match",
            "max-count",
            "after-context",
            "before-context",
            "context",
            "color",
            "colour",
            "binary-files",
            "text",
            "null",
            "null-data",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        // `--pre` and `--hostname-bin` hand rg an arbitrary program to run.
        // They are refused by being absent, which is the whole point of the
        // inversion: nobody had to think of them.
        name: "rg",
        // `z` is NOT `--null-data` here. In ripgrep it is `--search-zip`, which
        // spawns a PATH-resolved decompression binary (`gzip`, `xz`, …). The
        // verifier proved the exec differentially: `rg -z` succeeded with a real
        // gzip on PATH and failed with an unrunnable shim ahead of it. The
        // letter was carried over from `grep`, where it IS `--null-data` and
        // harmless — which is exactly why each table must be derived from its
        // own tool's `--help` and not from the letter's meaning elsewhere.
        short: "eFgiIlLmnNopqrstuvwxAaBcCS",
        long: &[
            "regexp",
            "fixed-strings",
            "glob",
            "iglob",
            "ignore-case",
            "smart-case",
            "case-sensitive",
            "word-regexp",
            "line-regexp",
            "files",
            "files-with-matches",
            "files-without-match",
            "count",
            "count-matches",
            "only-matching",
            "invert-match",
            "line-number",
            "no-line-number",
            "with-filename",
            "no-filename",
            "heading",
            "no-heading",
            "column",
            "byte-offset",
            "context",
            "after-context",
            "before-context",
            "max-count",
            "max-depth",
            "type",
            "type-not",
            "hidden",
            "no-ignore",
            "follow",
            "multiline",
            "multiline-dotall",
            "null",
            "null-data",
            "quiet",
            "stats",
            "trim",
            "vimgrep",
            "json",
            "color",
            "colors",
            "sort",
            "sortr",
            "text",
            "binary",
            "replace",
            "path-separator",
            "crlf",
            "engine",
            "pcre2",
            "no-messages",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "basename",
        short: "asz",
        long: &["suffix", "multiple", "zero"],
        max_operands: 2,
    },
    Program {
        name: "dirname",
        short: "z",
        long: &["zero"],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "stat",
        short: "cfLt",
        long: &[
            "format",
            "printf",
            "dereference",
            "file-system",
            "terse",
            "cached",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "du",
        short: "abcdhHklLmPsSxX",
        long: &[
            "all",
            "bytes",
            "total",
            "human-readable",
            "max-depth",
            "summarize",
            "apparent-size",
            "block-size",
            "si",
            "separate-dirs",
            "one-file-system",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        // `--output=LIST` here is a field list, not a file, but the name reads
        // as a writer and the tables cost nothing to keep narrow.
        name: "df",
        short: "ahHiklmPtTvx",
        long: &[
            "all",
            "human-readable",
            "si",
            "inodes",
            "local",
            "portability",
            "total",
            "print-type",
            "block-size",
            "type",
            "exclude-type",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "which",
        short: "as",
        long: &["all"],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "printenv",
        short: "0",
        long: &["null"],
        max_operands: ANY_OPERANDS,
    },
    Program {
        // `o`/`--output` write a file; `--compress-program` and
        // `--temporary-directory`/`T` hand it a program and a write target.
        // All four are refused by absence.
        name: "sort",
        short: "bcdfghiklmMnrRsStuVz",
        long: &[
            "ignore-leading-blanks",
            "dictionary-order",
            "ignore-case",
            "general-numeric-sort",
            "human-numeric-sort",
            "month-sort",
            "numeric-sort",
            "version-sort",
            "random-sort",
            "reverse",
            "sort",
            "key",
            "field-separator",
            "stable",
            "unique",
            "check",
            "zero-terminated",
            "debug",
            "merge",
            "buffer-size",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        // `uniq [INPUT [OUTPUT]]` — the second operand is written. Capping at
        // one operand is the only defence, and no flag table has one.
        name: "uniq",
        short: "cdDfisuwz",
        long: &[
            "count",
            "repeated",
            "all-repeated",
            "skip-fields",
            "skip-chars",
            "ignore-case",
            "unique",
            "check-chars",
            "zero-terminated",
            "group",
        ],
        max_operands: 1,
    },
    Program {
        name: "cut",
        short: "bcdfns",
        long: &[
            "bytes",
            "characters",
            "fields",
            "delimiter",
            "complement",
            "only-delimited",
            "zero-terminated",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "tr",
        short: "cCdst",
        long: &["delete", "squeeze-repeats", "complement", "truncate-set1"],
        max_operands: 2,
    },
    Program {
        name: "diff",
        short: "abBcdEHiInNpqrstTuwWyZ",
        long: &[
            "brief",
            "recursive",
            "unified",
            "context",
            "ignore-all-space",
            "ignore-space-change",
            "ignore-blank-lines",
            "ignore-case",
            "side-by-side",
            "new-file",
            "text",
            "color",
            "minimal",
            "report-identical-files",
            "exclude",
            "exclude-from",
            "label",
            "suppress-common-lines",
            "expand-tabs",
            "strip-trailing-cr",
        ],
        max_operands: 2,
    },
    Program {
        name: "cmp",
        short: "bilns",
        long: &[
            "print-bytes",
            "ignore-initial",
            "verbose",
            "bytes",
            "quiet",
            "silent",
        ],
        max_operands: 2,
    },
    Program {
        name: "column",
        short: "tsxn",
        long: &["table", "separator", "fillrows", "columns"],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "nl",
        short: "bdfhilnpstvw",
        long: &[
            "body-numbering",
            "header-numbering",
            "footer-numbering",
            "number-format",
            "number-width",
            "number-separator",
            "line-increment",
            "starting-line-number",
            "join-blank-lines",
            "no-renumber",
            "section-delimiter",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "realpath",
        short: "eLmPqsz",
        long: &[
            "canonicalize-existing",
            "canonicalize-missing",
            "logical",
            "physical",
            "quiet",
            "strip",
            "no-symlinks",
            "zero",
            "relative-to",
            "relative-base",
        ],
        max_operands: ANY_OPERANDS,
    },
    Program {
        name: "readlink",
        short: "efmnqsvz",
        long: &[
            "canonicalize",
            "canonicalize-existing",
            "canonicalize-missing",
            "no-newline",
            "quiet",
            "silent",
            "verbose",
            "zero",
        ],
        max_operands: ANY_OPERANDS,
    },
];

/// `git` subcommands that only inspect, IN EVERY SHAPE. A subcommand that reads
/// in one shape and writes in another does not belong here; it belongs in
/// [`GIT_SUBCOMMANDS_WITH_ONE_READONLY_SHAPE`].
///
/// Absent on purpose, and each for a reason that is not obvious: `config` (a
/// bare `git config k v` WRITES), `stash` (`stash list` reads but `stash` alone
/// mutates), `tag` (creates), `branch` (`-d`/`-D`/`-m` mutate), `checkout`,
/// `restore`, `clean`, `gc`, `fetch`, `pull`, `push`.
///
/// `ls-remote` was here and has been REMOVED. It writes nothing locally, which
/// is why it passed the admission rule as written — but it contacts a remote of
/// the caller's choosing, which makes it an outbound channel out of a turn that
/// is tainted precisely because it consumed untrusted content. Reading is not
/// the only thing a tainted turn must not do with attacker-influenced data;
/// sending it somewhere is worse. (`--upload-pack=<exec>` also ran a program,
/// but the channel alone is disqualifying.)
const READONLY_GIT_SUBCOMMANDS: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "rev-parse",
    "ls-files",
    "ls-tree",
    "blame",
    "shortlog",
    "describe",
    "cat-file",
    "name-rev",
    "whatchanged",
    "grep",
];

/// `git` subcommands that are read-only in exactly one shape, paired with the
/// single second token that makes them so. Anything else after the subcommand —
/// including nothing at all, unless the pair's second element is `None` — is
/// refused.
///
/// * `worktree list` reads; `add`, `remove`, `prune`, `repair`, `move` mutate.
/// * `reflog` with no verb is `reflog show` and reads; `reflog expire` and
///   `reflog delete` DESTROY reflog entries. This one was in the always-safe
///   table above until review caught it, which is why the two tables now exist:
///   "the bare form reads" is not the same property as "every form reads", and
///   collapsing them is how `expire` got a pass.
const GIT_SUBCOMMANDS_WITH_ONE_READONLY_SHAPE: &[(&str, Option<&str>)] = &[
    ("worktree", Some("list")),
    ("reflog", None),
    ("reflog", Some("show")),
];

/// Short flags admitted after a `git` subcommand, pooled across subcommands.
///
/// `O` is absent, and that absence is the fix for a measured hole: `git grep
/// -O<pager>` and `--open-files-in-pager=<pager>` launch a program, and the old
/// denylist compared against `-o` with a case-sensitive prefix match, so `-O`
/// walked straight past it. Under an allowlist the letter simply is not there.
///
/// `o` and `c` are absent too — `--output` writes and `-c <k>=<v>` sets config
/// for the invocation (`git -c core.pager=<cmd>` before the subcommand is
/// already refused by [`is_readonly_git`], but after it there is no reason to
/// admit the letter).
const GIT_SHORT: &str = "abEFhilnpqrstuvwz";

/// Long flags admitted after a `git` subcommand, pooled across subcommands.
///
/// Pooling is safe in the direction that matters: admitting `--porcelain` for
/// `git log`, where it means nothing, costs an error message from git. What it
/// must never do is admit a flag that writes or execs for ANY of the
/// subcommands in the two tables above, which is why `--output`,
/// `--open-files-in-pager`, `--upload-pack`, `--exec-path`, `--textconv` and
/// `--ext-diff` are all absent.
const GIT_LONG: &[&str] = &[
    "oneline",
    "stat",
    "numstat",
    "shortstat",
    "name-only",
    "name-status",
    "graph",
    "all",
    "short",
    "branch",
    "porcelain",
    "cached",
    "staged",
    "no-color",
    "color",
    "decorate",
    "abbrev-commit",
    "abbrev",
    "reverse",
    "patch",
    "no-patch",
    "summary",
    "list",
    "show-current",
    "verify",
    "quiet",
    "count",
    "parents",
    "children",
    "format",
    "pretty",
    "date",
    "author",
    "committer",
    "since",
    "until",
    "grep",
    "word-diff",
    "ignore-all-space",
    "ignore-space-change",
    "unified",
    "follow",
    "full-history",
    "first-parent",
    "merges",
    "no-merges",
    "others",
    "ignored",
    "exclude-standard",
    "modified",
    "deleted",
    "stage",
    "long",
    "null",
    "line-number",
    "files-with-matches",
    "count-matches",
    "untracked",
    "no-renames",
    "find-renames",
    "diff-filter",
    "max-count",
    "skip",
    "boundary",
    "line-porcelain",
    // `--show-signature` was here and is REMOVED: it invokes `gpg.program` on a
    // signed commit. Reasoned, not observed (this machine has neither a signed
    // commit nor gpg), and removing an unobservable exec path is the cheap
    // direction.
    "objects",
    "batch-check",
    "abbrev-ref",
    "symbolic",
    "symbolic-full-name",
    "git-dir",
    "show-toplevel",
    "is-inside-work-tree",
    "absolute-git-dir",
    "relative",
    "no-textconv",
    "text",
];

/// Is `token` an admissible argument for a program with these tables?
///
/// The three arms are the whole inversion: a `--long` must be named, every
/// letter of a `-abc` bundle must be named, and an operand only counts against
/// a budget. Nothing is admitted for looking harmless.
fn argument_is_admitted(
    token: &str,
    short: &str,
    long: &[&str],
    operands: &mut usize,
    max_operands: usize,
) -> bool {
    if let Some(body) = token.strip_prefix("--") {
        // A bare `--` (end of options) would make every following token an
        // operand under a different tokenizer's rules than this one models.
        if body.is_empty() {
            return false;
        }
        let name = match body.split_once('=') {
            Some((name, _)) => name,
            None => body,
        };
        return long.contains(&name);
    }
    if let Some(body) = token.strip_prefix('-') {
        // A lone `-` means stdin to some programs and nothing to others.
        if body.is_empty() {
            return false;
        }
        // A digit is an argument to the preceding letter (`head -n5`,
        // `sort -k2`), never a verb, so it is admitted for every program
        // without appearing in any table.
        return body
            .chars()
            .all(|c| c.is_ascii_digit() || short.contains(c));
    }
    *operands += 1;
    *operands <= max_operands
}

/// Is `command` — a `Bash` tool's `command` string — statically known to write
/// nothing?
///
/// `false` means "not recognised as read-only", which is the answer for every
/// unknown shape. It never means "this command is dangerous".
pub fn is_readonly_bash(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() {
        return false;
    }
    if command.contains(FORBIDDEN_CHARS) || command.contains(QUOTE_CHARS) {
        return false;
    }
    // A pipeline is read-only exactly when every stage is. `||` leaves an empty
    // segment, which fails `is_readonly_segment`.
    command.split('|').all(is_readonly_segment)
}

/// One pipeline stage: a program name followed by arguments, already known to
/// be free of quotes and metacharacters.
fn is_readonly_segment(segment: &str) -> bool {
    let mut tokens = segment.split_whitespace();
    let Some(program) = tokens.next() else {
        return false;
    };
    // A path-qualified program (`/usr/bin/git`, `./script`) is not classified:
    // the table is keyed by bare names, and matching only the basename would
    // let `./ls` — any local executable named after a table entry — through.
    if program.contains('/') || program.contains('\\') {
        return false;
    }
    let rest: Vec<&str> = tokens.collect();
    if program == "git" {
        return is_readonly_git(&rest);
    }
    let Some(entry) = READONLY_PROGRAMS.iter().find(|p| p.name == program) else {
        return false;
    };
    arguments_are_admitted(&rest, entry.short, entry.long, entry.max_operands)
}

/// Every argument admitted, and the operand budget not exceeded.
fn arguments_are_admitted(args: &[&str], short: &str, long: &[&str], max_operands: usize) -> bool {
    let mut operands = 0usize;
    args.iter()
        .all(|t| argument_is_admitted(t, short, long, &mut operands, max_operands))
}

/// `git` arguments, with the program name already stripped.
///
/// Any option BEFORE the subcommand is refused outright rather than inspected:
/// `git -c <k>=<v>`, `--exec-path`, `-C <dir>` and friends change what runs and
/// where, and enumerating the safe ones is the kind of table that is wrong the
/// day git grows another. The subcommand must be the first token.
fn is_readonly_git(args: &[&str]) -> bool {
    let Some(subcommand) = args.first() else {
        // Bare `git` prints usage, but a table of one special case for a
        // no-op is not worth the arm; not recognised.
        return false;
    };
    if subcommand.starts_with('-') {
        return false;
    }
    // Checked BEFORE the always-safe table: a subcommand that appears here is
    // one whose bare name must NOT be trusted on its own.
    let rest: &[&str] = if GIT_SUBCOMMANDS_WITH_ONE_READONLY_SHAPE
        .iter()
        .any(|(name, _)| name == subcommand)
    {
        let verb = args.get(1).copied();
        if !GIT_SUBCOMMANDS_WITH_ONE_READONLY_SHAPE.contains(&(subcommand, verb)) {
            return false;
        }
        // `verb.is_some()` means the pair consumed args[1]; the `None` pair
        // (bare `git reflog`) consumed nothing.
        if verb.is_some() {
            &args[2..]
        } else {
            &args[1..]
        }
    } else {
        if !READONLY_GIT_SUBCOMMANDS.contains(subcommand) {
            return false;
        }
        &args[1..]
    };
    // Every git subcommand admitted above takes paths/revisions as operands,
    // none of which is an output, so the budget is unbounded. The flags are
    // where the writers live, and those are named.
    arguments_are_admitted(rest, GIT_SHORT, GIT_LONG, ANY_OPERANDS)
}

#[cfg(test)]
mod tests {
    use super::is_readonly_bash;

    /// Assert against the CONTRACT this module's docs state, not against
    /// whatever the tables happen to contain: "positively known to write
    /// nothing" is `true`, and every other shape — unknown program, unknown
    /// `git` subcommand, quoting this tokenizer would have to guess at, any
    /// metacharacter that can redirect/substitute/chain — is `false`.
    fn assert_readonly(command: &str) {
        assert!(
            is_readonly_bash(command),
            "expected {command:?} to be recognised as read-only"
        );
    }

    fn assert_gated(command: &str) {
        assert!(
            !is_readonly_bash(command),
            "expected {command:?} NOT to be recognised as read-only"
        );
    }

    /// The commands this module exists for (backlog a4b59893): a tainted turn
    /// must be able to diagnose itself.
    #[test]
    fn the_diagnostic_commands_are_read_only() {
        for command in [
            "git status",
            "git log --oneline",
            "git diff",
            "git worktree list",
            "ls -la",
            "pwd",
        ] {
            assert_readonly(command);
        }
    }

    /// A pipeline is read-only exactly when EVERY stage is.
    #[test]
    fn a_pipeline_of_read_only_stages_is_read_only() {
        assert_readonly("git status | wc -l");
        assert_readonly("git log --oneline | head -20 | sort");
    }

    /// A pipeline with ONE non-read-only stage is not read-only — the
    /// anti-vacuity control for the test above, which would still pass if
    /// `is_readonly_bash` looked only at the first stage.
    #[test]
    fn a_pipeline_with_one_gated_stage_is_gated() {
        assert_gated("git status | xargs shred");
        assert_gated("cat f | python3");
    }

    /// Mutating `git` subcommands.
    #[test]
    fn mutating_git_subcommands_are_gated() {
        for command in [
            "git config user.name foo",
            "git checkout main",
            "git commit -m msg",
            "git push",
            "git",
        ] {
            assert_gated(command);
        }
    }

    /// `GIT_SUBCOMMANDS_WITH_ONE_READONLY_SHAPE`: a subcommand whose BARE name
    /// must not be trusted, paired with the single shape that reads.
    ///
    /// This is also the OBSERVATION that settles a type question the
    /// implementer flagged as an untested inference: `contains(&(subcommand,
    /// verb))` compares a `&&str` against a table of `&str` and relies on the
    /// tuple literal's expected type driving a deref coercion. It compiles — but
    /// "it compiles" would also be true of a comparison that never matched, in
    /// which case `git reflog` would be gated (fail-closed, and invisible) while
    /// `git reflog expire` would be gated too, and no test would tell the two
    /// apart. Asserting BOTH directions is what makes the pair matching real:
    /// the `true` rows can only pass if the tuple comparison actually matches,
    /// and the `false` rows can only pass if it actually discriminates.
    #[test]
    fn git_subcommands_read_only_in_exactly_one_shape() {
        assert_readonly("git reflog");
        assert_readonly("git reflog show");
        assert_readonly("git reflog show --all");
        assert_readonly("git worktree list");
        assert_readonly("git worktree list --porcelain");

        assert_gated("git reflog expire --all");
        assert_gated("git reflog expire");
        assert_gated("git reflog delete");
        // Gated by the metacharacter rule (`{`/`}`), NOT by the pair table —
        // stated so a reader does not credit this row to the table above.
        assert_gated("git reflog delete HEAD@{0}");
        assert_gated("git worktree");
        assert_gated("git worktree add /tmp/x");
        assert_gated("git worktree remove x");
        assert_gated("git worktree prune");
        // An option in the verb position is not the one read-only shape either.
        assert_gated("git reflog --all");
        assert_gated("git worktree --porcelain");
    }

    /// Programs that are not in the table at all — including every general
    /// purpose executor, which the module docs say must stay absent.
    #[test]
    fn unknown_programs_and_executors_are_gated() {
        for command in [
            "shred -u /etc/passwd",
            "sed -i s/a/b/ f",
            "find . -delete",
            "python3 -m pip",
            "bash script.sh",
            "xargs shred",
            "touch out.txt",
            "cargo test",
        ] {
            assert_gated(command);
        }
    }

    /// A path-qualified program is never classified, so a local executable
    /// named after a table entry cannot borrow that entry's verdict.
    #[test]
    fn path_qualified_programs_are_gated() {
        assert_gated("./ls");
        assert_gated("/usr/bin/ls");
        assert_gated("bin/git status");
        assert_gated(".\\ls");
    }

    /// Redirection, command substitution, chaining and backgrounding are
    /// refused wholesale — note each of these has `git status`, a read-only
    /// command, as its FIRST token, so a tokenizer that looked only at the
    /// program name would pass every one of them.
    #[test]
    fn metacharacters_are_gated() {
        for command in [
            "git status > out.txt",
            "git status >> out.txt",
            "git status; shred x",
            "git status && shred x",
            "git status || shred x",
            "git status & shred x",
            "echo $(shred x)",
            "echo `shred x`",
            "cat < f",
            "ls ${HOME}",
        ] {
            assert_gated(command);
        }
    }

    /// Quoting is refused because this tokenizer splits on whitespace and would
    /// otherwise reason about a quoted argument's interior as if it were
    /// separate tokens. Refusing to classify is not a claim of danger.
    #[test]
    fn quoted_commands_are_gated() {
        assert_gated("git log --format='%h %s'");
        assert_gated("python3 -c \"import os\"");
    }

    /// The empty and whitespace-only command: nothing was classified, so the
    /// answer is `false`, never a vacuous `true` over an empty token stream.
    #[test]
    fn the_empty_command_is_gated() {
        assert_gated("");
        assert_gated("   ");
        assert_gated("\t");
    }

    /// Write-capable flags, refused uniformly across programs.
    #[test]
    fn write_capable_flags_are_gated() {
        assert_gated("git diff --output=f");
        assert_gated("git diff --output f");
        assert_gated("sort -o out.txt");
        assert_gated("sort --output=out.txt");
        assert_gated("git --exec-path=/tmp status");
    }

    /// `-c` is NOT a write-capable flag prefix (it rejected only read-only,
    /// everyday commands). These pin the read-only side of that decision across
    /// several programs, so a re-added `-c` cannot pass by fixing one of them.
    ///
    /// `uniq -c` is deliberately NOT among them even though it is the same
    /// shape: this verifier has filed `uniq` as a suspected bare-argument writer
    /// (`uniq INPUT OUTPUT` truncates `OUTPUT`, the `env`/`hostname` pattern),
    /// and pinning any `uniq` verdict here would turn a test into an obstacle to
    /// that fix. The `-c` decision is fully covered without it.
    #[test]
    fn dash_c_no_longer_gates_read_only_commands() {
        assert_readonly("wc -c");
        assert_readonly("grep -c foo");
        assert_readonly("sort -c");
        assert_readonly("ls -c");
        assert_readonly("head -c 10");
        assert_readonly("tail -c 10");
        assert_readonly("cut -c 1-3");
        assert_readonly("git status | wc -c");
    }

    /// ANTI-VACUITY for the test above: dropping `-c` from the flag denylist
    /// must not have loosened anything. `git -c core.pager=<cmd> status` is
    /// refused by [`super::is_readonly_git`]'s "no option in the subcommand
    /// position" rule — asserted through several shapes so a future edit to
    /// that rule cannot silently reopen the hole while the flag table stays
    /// unchanged. `sh -c` is unreachable for a second, independent reason:
    /// `sh` is not in [`super::READONLY_PROGRAMS`].
    #[test]
    fn dash_c_removal_did_not_loosen_anything() {
        assert_gated("git -c core.pager=shred status");
        assert_gated("git -c core.pager=shred log");
        assert_gated("git --no-pager status");
        assert_gated("git -C /tmp status");
        assert_gated("sh -c shred");
        assert_gated("sort -o out.txt");
        assert_gated("git diff --output=f");
    }

    /// The FOUR programs removed from the table during review, each of which
    /// has a name that reads as a pure query and a write reachable anyway:
    /// `env FOO=bar <cmd>` EXECS `<cmd>`; `date -s`/`--set` writes the system
    /// clock; `hostname <name>` SETS the host name from a bare argument;
    /// `file -C -m <magfile>` compiles and writes `<magfile>.mgc`.
    ///
    /// The BARE form of each is pinned alongside the dangerous one on purpose.
    /// Bare `env`/`date`/`hostname`/`file x.txt` really are read-only, so the
    /// temptation to re-add "just the harmless shape" is exactly the regression
    /// this test has to catch — the table's unit of decision is the program, not
    /// the invocation.
    #[test]
    fn programs_with_a_reachable_write_are_not_read_only() {
        for command in [
            "env FOO=bar shred x",
            "env",
            "env shred",
            "date -s 2020-01-01",
            "date",
            "hostname newname",
            "hostname",
            "file x.txt",
            "file -C -m magic",
        ] {
            assert_gated(command);
        }
    }

    /// The control for the test above: `sort` STAYS in the table even though
    /// `sort -o FILE` writes, because `-o`/`--output` are refused for every
    /// program. Without this pair, "remove anything that can ever write" and
    /// "remove what writes through a bare argument" are indistinguishable.
    #[test]
    fn sort_stays_read_only_except_through_the_refused_output_flag() {
        assert_readonly("sort");
        assert_readonly("sort -r");
        assert_gated("sort -o out.txt");
    }
}
