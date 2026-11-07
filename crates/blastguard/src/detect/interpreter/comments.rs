//! `#` comment stripping for the interpreter program audit
//! ([`super::interpreter_program_verdict`]). Moved out of `interpreter.rs`
//! unchanged, only for the file-size cap; it is private to that module.

/// `payload` with every `#` line comment removed, quote-aware.
///
/// Backlog 6798f47d: `# deliberately does not call shutil.rmtree` was denied
/// as "it runs `rmtree`". A `#` opens a comment here only at the start of a
/// line or after whitespace, and only outside `'`/`"`/`` ` `` quotes
/// (backslash escapes honoured), so `"#{x}"`, `$#a`, `${#v}` and `this.#p`
/// are kept. `//` is NOT stripped: in Python it is floor division, and code
/// after it would vanish.
///
/// This is not a parser for any one language, and it can be wrong — a JS
/// private field `{ #p; … }` reads as a comment. That is why its only use is
/// to tell "a destructive spelling appears outside comments" (Deny, as before)
/// from "it appears ONLY inside what reads as a comment" (Ask, see
/// [`super::interpreter_code_verdict`]). A misreading moves a Deny to an Ask,
/// never to an Allow.
///
/// With `blank_strings`, the contents of quoted strings are dropped too (the
/// quote characters stay). That text is used ONLY to choose the wording of a
/// Deny — "it runs `x`" versus "a quoted string spells `x`" — never the verdict.
/// In particular it is NOT the precondition of the string-only downgrade
/// (backlog 27795e6e): blanking every quoted body also blanks code inside
/// interpolation (`f"{os.unlink(p)}"`, `"#{File.delete(p)}"`), so "the spelling
/// disappears" proves nothing. That downgrade is decided by
/// [`super::inert_literal_bodies`] instead.
pub(super) fn strip_hash_comments(payload: &str, blank_strings: bool) -> String {
    let mut out = String::with_capacity(payload.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut in_comment = false;
    let mut prev: Option<char> = None;
    for c in payload.chars() {
        if in_comment {
            if c == '\n' {
                in_comment = false;
                out.push(c);
            }
            prev = Some(c);
            continue;
        }
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
                if !blank_strings || quote.is_none() {
                    out.push(c);
                }
            }
            None => {
                if c == '\\' {
                    escaped = !escaped;
                    out.push(c);
                } else if escaped {
                    escaped = false;
                    out.push(c);
                } else if c == '\'' || c == '"' || c == '`' {
                    quote = Some(c);
                    out.push(c);
                } else if c == '#' && prev.is_none_or(char::is_whitespace) {
                    in_comment = true;
                } else {
                    out.push(c);
                }
            }
        }
        prev = Some(c);
    }
    out
}
