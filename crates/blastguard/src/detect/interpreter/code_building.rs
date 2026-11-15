//! The "program builds or decodes the code it runs" scan of
//! [`super::interpreter_program_verdict`] and
//! [`super::python_file_body_verdict`]: the [`CODE_BUILDING`] markers and the
//! two occurrence-by-occurrence exemptions that are read from the Python
//! TOKENS of the program, not from its text (`re.compile(…)`, backlogs
//! 80b18990 / 2e5e080c, and a `globals()` read of a constant metadata key,
//! backlog 2e5e080c). Kept apart from `interpreter.rs` for the file-size cap.
//!
//! Both exemptions are positive identifications: each is granted only when
//! the program tokenises as Python ([`super::python_file::python_program_tokenises`])
//! and every rule below holds. A program that does not tokenise, or any
//! occurrence not positively identified, keeps the marker firing (an Ask, a
//! Deny inside a subagent). Nothing here can turn a marker on; it can only
//! withhold one that the substring scan raised.

use super::python_file::python_program_tokenises;
use super::python_semantics::{lex, Tok};

fn is_python_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// NOR IS TEXT THAT BUILDS ITS OWN CODE ([`super::interpreter_program_verdict`] and
/// [`super::python_file_body_verdict`] Ask on these). Same principle as the shell
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
///
/// The markers are SUBSTRINGS of the case-folded text, so a spelling with
/// whitespace before its `(` (`exec (…)`, `globals ()[…]`) contains none of
/// them; that gap is the list's, not the exemptions'.
pub(in crate::detect) const CODE_BUILDING: &[&str] = &[
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
///
/// Two markers have common unrelated spellings, and each is exempted only
/// occurrence by occurrence, from the Python tokens of the program:
/// - `compile(`: `re.compile(` builds a regex, not code (backlog 80b18990).
///   An occurrence is exempt only when [`compile_call_is_regex_method`]
///   positively establishes the regex module.
/// - `globals()[`: `globals()["__name__"]` reads a constant piece of module
///   metadata and calls nothing (backlog 2e5e080c). An occurrence is exempt
///   only when [`globals_subscript_is_metadata_read`] positively establishes
///   that reading.
///
/// Any other occurrence still fires, and so does every occurrence in a
/// program that does not tokenise as Python.
pub(in crate::detect) fn code_building_marker(payload: &str) -> Option<&'static str> {
    let lowered = payload.to_ascii_lowercase();
    CODE_BUILDING.iter().copied().find(|m| match *m {
        "compile(" => lowered
            .match_indices("compile(")
            .any(|(at, _)| !compile_call_is_regex_method(payload, at)),
        "globals()[" => lowered
            .match_indices("globals()[")
            .any(|(at, _)| !globals_subscript_is_metadata_read(payload, at)),
        _ => lowered.contains(m),
    })
}

/// The index of the `Name` token of `toks` that spans exactly `start..end`.
fn name_token_at(toks: &[Tok], start: Option<usize>, end: usize) -> Option<usize> {
    toks.iter().position(|t| match t {
        Tok::Name {
            start: s, end: e, ..
        } => *e == end && start.is_none_or(|want| *s == want),
        _ => false,
    })
}

fn name_text(t: Option<&Tok>) -> Option<&str> {
    match t {
        Some(Tok::Name { text, .. }) => Some(text),
        _ => None,
    }
}

/// Whether `word` occurs in `text` as a whole identifier (not as part of a
/// longer one).
fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(is_python_ident_char) && !after.is_some_and(is_python_ident_char)
    })
}

/// Whether the token at `k` is followed by an assignment to it: a lone `=`,
/// or an operator character followed by `=` or by a doubled operator
/// (augmented assignment `+=`, `<<=`, `**=`, `//=`, …; comparisons such as
/// `<=` and `!=` are rejected too — not a plain read this module vouches for).
fn followed_by_assignment(toks: &[Tok], k: usize) -> bool {
    let next = toks.get(k + 1);
    let second = toks.get(k + 2);
    match next {
        Some(Tok::Punct('=')) => second != Some(&Tok::Punct('=')),
        Some(Tok::Punct('+' | '-' | '*' | '/' | '%' | '&' | '|' | '^' | '<' | '>' | '@' | '!')) => {
            matches!(second, Some(Tok::Punct('=' | '<' | '>' | '*' | '/')))
        }
        _ => false,
    }
}

/// Whether the `compile(` occurrence at byte offset `at` of `payload` is a
/// regex-module method call, `re.compile(` or `regex.compile(`, and so not the
/// builtin `compile` that turns a string into code (backlog 80b18990).
///
/// `at` is an offset into the ASCII-lowercased payload; lowercasing preserves
/// byte offsets, and every check here is made case-sensitively on the Python
/// tokens of the original text (Python is case-sensitive: `RE.compile(` or
/// `re.COMPILE(` is not exempt).
///
/// Accepted only when ALL of these hold:
/// - `payload` tokenises as Python;
/// - the text at `at` is exactly `compile(` and its `compile` ends a `Name`
///   token that is exactly `re.compile` or `regex.compile`, followed by the
///   token `(` and not preceded by the token `.` (so `myre.compile(`,
///   `x.re.compile(` and `(x).re.compile(` are not exempt, and a `compile(`
///   inside a string literal is not a token at all);
/// - [`module_bound_only_by_import`] establishes that the module name is bound
///   by a plain module-level `import` and is used nowhere else except as
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
pub(super) fn compile_call_is_regex_method(payload: &str, at: usize) -> bool {
    if !payload
        .get(at..)
        .is_some_and(|tail| tail.starts_with("compile("))
    {
        return false;
    }
    if !python_program_tokenises(payload) {
        return false;
    }
    let toks = lex(payload);
    let Some(k) = name_token_at(&toks, None, at + "compile".len()) else {
        return false;
    };
    if toks.get(k + 1) != Some(&Tok::Punct('('))
        || k.checked_sub(1).and_then(|p| toks.get(p)) == Some(&Tok::Punct('.'))
    {
        return false;
    }
    let module = match name_text(toks.get(k)) {
        Some("re.compile") => "re",
        Some("regex.compile") => "regex",
        _ => return false,
    };
    module_bound_only_by_import(payload, &toks, module)
}

/// Whether the token at `k` is the first token of a logical statement that
/// begins at column 0 of its line (an unindented statement: module level).
fn statement_at_module_level(payload: &str, toks: &[Tok], k: usize) -> bool {
    let first_of_statement = k == 0 || toks.get(k - 1) == Some(&Tok::Nl);
    let Some(Tok::Name { start, .. }) = toks.get(k) else {
        return false;
    };
    first_of_statement
        && payload
            .get(..*start)
            .is_some_and(|head| head.is_empty() || head.ends_with('\n'))
}

/// Whether `name` is bound in `payload` (lexed as `toks`) only by a plain
/// module-level import and otherwise used only to read attributes of it.
///
/// Read from the Python tokens, so `import re; re.compile("x")` (a `;` ends
/// the statement) binds `re` just as two lines do, and an `import re` inside
/// a string literal or a comment binds nothing. Accepted only when ALL of
/// these hold (checked case-sensitively):
/// - some statement is `import <items>`, begins at column 0 of its line
///   (`def f():\n    import re` and `x = 1; import re` are not accepted), and
///   one comma-separated item is the single name token `name` (`import re`,
///   `import os, re`; not `import re as x`);
/// - every other `Name` token that has `name` as a dotted component comes
///   after that item, is not preceded by the token `.`, is exactly
///   `name.attr` with `attr` starting with an ASCII letter (not `_`), and is
///   followed by none of `.`, `[`, `:` or an assignment (a lone `=`, or an
///   operator token followed by `=` or a doubled operator — except `==`);
/// - no string literal (f-strings included) contains `name` as a whole word;
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
/// See [`compile_call_is_regex_method`].
fn module_bound_only_by_import(payload: &str, toks: &[Tok], name: &str) -> bool {
    if payload_has_star_import(payload) {
        return false;
    }
    let mut binding: Option<usize> = None;
    'statements: for (k, t) in toks.iter().enumerate() {
        if name_text(Some(t)) != Some("import") || !statement_at_module_level(payload, toks, k) {
            continue;
        }
        let mut item_start = k + 1;
        let mut j = k + 1;
        loop {
            let end = matches!(toks.get(j), None | Some(Tok::Nl));
            if end || toks.get(j) == Some(&Tok::Punct(',')) {
                if j == item_start + 1 && name_text(toks.get(item_start)) == Some(name) {
                    binding = Some(item_start);
                    break 'statements;
                }
                if end {
                    break;
                }
                item_start = j + 1;
            }
            j += 1;
        }
    }
    let Some(bound) = binding else {
        return false;
    };
    for (k, t) in toks.iter().enumerate() {
        match t {
            Tok::Str { text, .. } => {
                if contains_word(text, name) {
                    return false;
                }
            }
            Tok::Name { text, .. } => {
                let parts: Vec<&str> = text.split('.').collect();
                if !parts.contains(&name) || k == bound {
                    continue;
                }
                if k < bound
                    || parts.len() != 2
                    || parts[0] != name
                    || k.checked_sub(1).and_then(|p| toks.get(p)) == Some(&Tok::Punct('.'))
                    || !parts[1].starts_with(|c: char| c.is_ascii_alphabetic())
                    || matches!(toks.get(k + 1), Some(Tok::Punct('.' | '[' | ':')))
                    || followed_by_assignment(toks, k)
                {
                    return false;
                }
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

/// Module-metadata names whose `globals()` entry is plain data (a `str` or
/// `None`) set by the interpreter. Deliberately short: `__builtins__`,
/// `__loader__` and `__spec__` are NOT here, because each is (or leads to) an
/// object whose methods load or run code (`__loader__.load_module('os')`).
const METADATA_KEYS: &[&str] = &[
    "__name__",
    "__doc__",
    "__package__",
    "__file__",
    "__cached__",
];

/// Whether the `globals()[` occurrence at byte offset `at` of `payload` (an
/// offset into the ASCII-lowercased payload, which preserves byte offsets) is
/// a read of one constant module-metadata entry, `globals()["__name__"]`,
/// that calls nothing and binds nothing (backlog 2e5e080c).
///
/// Such a read is the same as writing the name itself (`__name__`); it does
/// not build or look up code. What the marker is there for is a subscript
/// that IS a dynamic lookup or call — `globals()[name](…)`,
/// `globals()["ex" + "ec"]`, `globals()["f"](…)`, `globals()["x"] = …` —
/// and every one of those still fires.
///
/// Accepted only when ALL of these hold (on the Python tokens, so
/// case-sensitively):
/// - `payload` tokenises as Python;
/// - the occurrence is the token sequence `globals` `(` `)` `[` STR `]`,
///   where `globals` is a bare name token starting at `at`, STR is ONE
///   non-f-string literal whose value is in [`METADATA_KEYS`] (so no implicit
///   concatenation, no expression, no variable key), and the token after `]`
///   is neither `(` (a call on the result) nor an assignment;
/// - every other token that names `globals` (a `Name` with `globals` as any
///   dotted component, or a string literal containing the word) is itself
///   such an accepted read — so `globals` was not rebound (`globals = …`,
///   `def globals`, `import x as globals`, `setattr(b, "globals", …)`) and
///   the namespace is not written through it (`globals().update(…)`);
/// - the key is not otherwise named anywhere in the program: no `Name`
///   token has it as a dotted component (`__name__ = …`, `m.__name__ = …`)
///   and no other string literal contains it as a whole word
///   (`setattr(m, "__name__", …)`). So the entry still holds the
///   interpreter's own value when it is read.
///
/// What this does NOT prove: that the entry was not rewritten through a
/// route that never spells the key or `globals` (`vars()`, `sys.modules[…]`,
/// `exec`, `__builtins__` — each of those is a [`CODE_BUILDING`] marker of
/// its own and fires regardless), or by code imported from outside the text.
fn globals_subscript_is_metadata_read(payload: &str, at: usize) -> bool {
    if !payload
        .get(at..)
        .is_some_and(|tail| tail.starts_with("globals()["))
    {
        return false;
    }
    if !python_program_tokenises(payload) {
        return false;
    }
    let toks = lex(payload);
    let read_at = |k: usize| -> Option<&str> {
        if name_text(toks.get(k)) != Some("globals")
            || k.checked_sub(1).and_then(|p| toks.get(p)) == Some(&Tok::Punct('.'))
            || toks.get(k + 1) != Some(&Tok::Punct('('))
            || toks.get(k + 2) != Some(&Tok::Punct(')'))
            || toks.get(k + 3) != Some(&Tok::Punct('['))
            || toks.get(k + 5) != Some(&Tok::Punct(']'))
            || toks.get(k + 6) == Some(&Tok::Punct('('))
            || followed_by_assignment(&toks, k + 5)
        {
            return None;
        }
        match toks.get(k + 4) {
            Some(Tok::Str {
                text,
                fstring: false,
            }) if METADATA_KEYS.contains(&text.as_str()) => Some(text.as_str()),
            _ => None,
        }
    };
    let Some(k) = name_token_at(&toks, Some(at), at + "globals".len()) else {
        return false;
    };
    let Some(key) = read_at(k) else {
        return false;
    };
    let accepted: Vec<usize> = (0..toks.len()).filter(|&j| read_at(j).is_some()).collect();
    for (j, t) in toks.iter().enumerate() {
        let in_accepted_read = accepted.iter().any(|&r| j == r || j == r + 4);
        match t {
            Tok::Name { text, .. } => {
                let parts: Vec<&str> = text.split('.').collect();
                if (parts.contains(&"globals") && !in_accepted_read) || parts.contains(&key) {
                    return false;
                }
            }
            Tok::Str { text, .. } => {
                if in_accepted_read {
                    continue;
                }
                if contains_word(text, "globals") || contains_word(text, key) {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}
