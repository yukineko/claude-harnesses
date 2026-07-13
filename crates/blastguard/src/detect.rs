//! Pure destructive-operation detection. No I/O, no globals beyond the static
//! allowlist in [`crate::exclude`] — just `(tool_name, tool_input) -> Decision`.
//!
//! The bias is deliberately asymmetric: we only Deny on *clearly* destructive,
//! hard-to-undo patterns (recursive/wildcard deletion, full-file truncation,
//! disk-level writes, working-tree discards). Anything ambiguous falls through
//! to Allow so blastguard never gets in the way of ordinary work.

use serde_json::Value;

use crate::exclude;
use crate::model::Decision;

/// Top-level entry: dispatch on the tool name.
pub fn detect(tool_name: &str, tool_input: Option<&Value>) -> Decision {
    match tool_name {
        "Bash" => {
            let cmd = tool_input
                .and_then(|v| v.get("command"))
                .and_then(|c| c.as_str());
            match cmd {
                Some(c) => detect_bash(c, 0),
                None => Decision::Allow,
            }
        }
        "Write" => detect_write(tool_input),
        // Edit / MultiEdit / NotebookEdit are partial edits, not full-file
        // destruction — always allowed.
        _ => Decision::Allow,
    }
}

// ---------------------------------------------------------------------------
// File-write handling
// ---------------------------------------------------------------------------

fn extract_path(ti: Option<&Value>) -> Option<String> {
    let v = ti?;
    for key in ["file_path", "notebook_path", "path"] {
        if let Some(p) = v.get(key).and_then(|p| p.as_str()) {
            if !p.is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

/// Write is new/overwrite both. We stay conservative and only Deny the clearly
/// destructive shapes: replacing a (non-config) file with empty content, or
/// overwriting git internals. Everything else is allowed.
fn detect_write(ti: Option<&Value>) -> Decision {
    let path = match extract_path(ti) {
        Some(p) => p,
        None => return Decision::Allow,
    };
    if exclude::is_config_file(&path) {
        return Decision::Allow;
    }
    if exclude::is_git_internal(&path) {
        return Decision::deny(format!(
            "Write would overwrite git internals ({path}) — refusing"
        ));
    }
    let content = ti
        .and_then(|v| v.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if content.trim().is_empty() {
        return Decision::deny(format!(
            "Write would replace {path} with empty content, wiping the file"
        ));
    }
    Decision::Allow
}

// ---------------------------------------------------------------------------
// Bash handling
// ---------------------------------------------------------------------------

/// How deep we recurse into shell-evaluation wrappers (`eval`, `sh -c`, …)
/// before giving up. Bounds work and guards against pathological nesting; no
/// realistic command line wraps a destructive payload this many layers deep.
const MAX_SHELL_DEPTH: usize = 8;

fn detect_bash(cmd: &str, depth: usize) -> Decision {
    // 1. Fork bomb (whitespace-insensitive signature).
    let compact: String = cmd.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains(":(){") && compact.contains(":|:") {
        return Decision::deny("fork bomb pattern detected");
    }

    // 2. Truncating `>` redirects (quote-aware, ignores >>, 2>, &>, >&). Scan
    // *every* redirect on the line, not just the first: a safe early redirect
    // (`> /dev/null`) must not blind the gate to a later truncating redirect in
    // a subsequent `;`/`&&`/`|` segment.
    for target in redirect_targets(cmd) {
        if !redirect_target_is_safe(&target) {
            return Decision::deny(format!(
                "'> {target}' truncates and overwrites an existing file"
            ));
        }
    }

    // 3. Per-command-segment analysis.
    for seg in split_segments(cmd) {
        let d = analyze_segment(&seg, depth);
        if d.is_deny() {
            return d;
        }
    }

    Decision::Allow
}

fn redirect_target_is_safe(target: &str) -> bool {
    let t = exclude::normalize(target);
    matches!(t.as_str(), "/dev/null" | "/dev/stdout" | "/dev/stderr") || exclude::is_config_file(&t)
}

/// Quote-aware split of a command line into individual simple-command segments
/// on `;`, newline, `&&`, `||`, `|`, `&`.
fn split_segments(cmd: &str) -> Vec<String> {
    let bytes = cmd.as_bytes();
    let mut segs = Vec::new();
    let mut cur = String::new();
    let (mut in_s, mut in_d) = (false, false);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '\'' && !in_d {
            in_s = !in_s;
            cur.push(c);
            i += 1;
            continue;
        }
        if c == '"' && !in_s {
            in_d = !in_d;
            cur.push(c);
            i += 1;
            continue;
        }
        if !in_s && !in_d {
            // Two-char operators.
            if (c == '&' && bytes.get(i + 1) == Some(&b'&'))
                || (c == '|' && bytes.get(i + 1) == Some(&b'|'))
            {
                segs.push(std::mem::take(&mut cur));
                i += 2;
                continue;
            }
            if c == ';' || c == '\n' || c == '|' || c == '&' {
                segs.push(std::mem::take(&mut cur));
                i += 1;
                continue;
            }
        }
        cur.push(c);
        i += 1;
    }
    segs.push(cur);
    segs
}

/// Find the first single `>` redirect outside quotes and return its target
/// token. Returns None for `>>`, `2>`, `&>`, `>&` and quoted `>`.
#[cfg(test)]
fn single_redirect_target(seg: &str) -> Option<String> {
    redirect_targets(seg).into_iter().next()
}

/// Every single `>` truncating-redirect target on the line, in order, outside
/// quotes. Skips `>>`, `2>`, `&>`, `>&`, quoted `>`, Rust arrows (`->`) and
/// angle-bracket placeholders (`<value>`). Scanning the *whole* line (rather
/// than pre-split segments) keeps the fd-dup context (`2>&1`, `>&2`) intact
/// while still catching a truncating redirect in any later segment.
fn redirect_targets(seg: &str) -> Vec<String> {
    let bytes = seg.as_bytes();
    let (mut in_s, mut in_d) = (false, false);
    let mut targets = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\'' && !in_d {
            in_s = !in_s;
            i += 1;
            continue;
        }
        if c == b'"' && !in_s {
            in_d = !in_d;
            i += 1;
            continue;
        }
        if c == b'>' && !in_s && !in_d {
            let prev = if i > 0 { bytes[i - 1] } else { 0 };
            let next = *bytes.get(i + 1).unwrap_or(&0);
            // An explicit-fd redirect whose fd is NOT stdout (`2>file`, `11>file`)
            // is allowed; fd 1 (`1>file`) truncates stdout identically to bare
            // `>file` and must be treated as a real truncating redirect. Read the
            // WHOLE digit run before `>` (not just one byte) so multi-digit fds
            // like `11>`/`21>` — which merely END in 1 — stay allowed. (`1>&2` is
            // still skipped below by the `next == b'&'` fd-dup clause.)
            let explicit_nonstdout_fd = prev.is_ascii_digit() && {
                let mut s = i - 1;
                while s > 0 && bytes[s - 1].is_ascii_digit() {
                    s -= 1;
                }
                &bytes[s..i] != b"1"
            };
            // Skip append `>>`, fd dup forms, and stderr/other-fd forms.
            if next == b'>' || prev == b'>' || prev == b'&' || explicit_nonstdout_fd || next == b'&'
            {
                i += 1;
                continue;
            }
            // Not a redirect: the second char of a Rust return-type arrow `->`.
            if prev == b'-' {
                i += 1;
                continue;
            }
            // Not a redirect: the close of an angle-bracket identifier
            // placeholder like `<value>` / `<id>` / `<RID>` — a `>` reached by
            // scanning back over identifier chars to an opening `<`.
            if is_angle_placeholder_close(bytes, i) {
                i += 1;
                continue;
            }
            // Single truncating redirect — read the target token.
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            let start = j;
            while j < bytes.len() {
                let cj = bytes[j];
                if (cj as char).is_whitespace()
                    || cj == b';'
                    || cj == b'|'
                    || cj == b'&'
                    || cj == b'>'
                {
                    break;
                }
                j += 1;
            }
            targets.push(seg[start..j].to_string());
            i = j;
            continue;
        }
        i += 1;
    }
    targets
}

/// True when the `>` at `bytes[gt]` closes an angle-bracket identifier
/// placeholder like `<value>` / `<id>` / `<RID>`: scan back over identifier
/// chars (`[A-Za-z0-9_]`, at least one) and find an opening `<`. Such prose is a
/// placeholder token, not a truncating redirect target.
fn is_angle_placeholder_close(bytes: &[u8], gt: usize) -> bool {
    let mut k = gt;
    while k > 0 && (bytes[k - 1].is_ascii_alphanumeric() || bytes[k - 1] == b'_') {
        k -= 1;
    }
    // Require ≥1 identifier char between the `<` and the `>`.
    k < gt && k > 0 && bytes[k - 1] == b'<'
}

fn basename(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

fn is_assignment(tok: &str) -> bool {
    if let Some(eq) = tok.find('=') {
        let name = &tok[..eq];
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && name
                .chars()
                .next()
                .map(|c| !c.is_ascii_digit())
                .unwrap_or(false)
    } else {
        false
    }
}

/// Index of the effective command word, skipping leading `VAR=val` assignments
/// and benign wrapper commands (sudo, env, nohup, …).
fn command_index(tokens: &[&str]) -> Option<usize> {
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if t.is_empty() || is_assignment(t) {
            i += 1;
            continue;
        }
        match basename(t) {
            "sudo" | "doas" | "nohup" | "env" | "command" | "time" | "nice" | "ionice" => {
                i += 1;
            }
            _ => return Some(i),
        }
    }
    None
}

fn is_short_flag(tok: &str) -> bool {
    tok.starts_with('-') && !tok.starts_with("--")
}

/// True if any short flag bundle in `rest` contains `ch`, or the long flag is set.
fn has_short(rest: &[&str], ch: char) -> bool {
    rest.iter().any(|t| is_short_flag(t) && t.contains(ch))
}

/// Shells that take a command line as a string argument (e.g. `sh -c "…"`).
fn is_shell(cmd: &str) -> bool {
    matches!(cmd, "sh" | "bash" | "zsh" | "ksh" | "dash")
}

/// Non-shell interpreters that run an inline program supplied as a string
/// argument (e.g. `python3 -c "…"`, `perl -e "…"`). Like a shell's `-c`, this
/// lets `find -exec <interp> -c "<payload>"` run an arbitrary destructive
/// command per match, slipping past the literal `rm` token scan.
fn is_code_interpreter(cmd: &str) -> bool {
    matches!(
        cmd,
        "python" | "python2" | "python3" | "perl" | "ruby" | "node" | "nodejs" | "php" | "lua"
    )
}

/// Inline-code eval flags for the interpreters in [`is_code_interpreter`]
/// (`python -c`, `perl -e`/`-E`, `ruby -e`, `node -e`/`--eval`/`-p`, `php -r`).
/// A script-file argument (no such flag) is deliberately NOT matched, so
/// `find -exec python3 script.py` is left alone.
fn is_inline_eval_flag(tok: &str) -> bool {
    matches!(tok, "-c" | "-e" | "-E" | "-r" | "-p" | "--eval" | "--print")
}

/// Strip one layer of matching surrounding quotes from a reconstructed
/// argument string. Tokenisation has already split on whitespace, so a quoted
/// payload like `"rm -rf /"` arrives as `"rm -rf /"` — peel the wrapper so the
/// inner command line can be re-analysed.
fn strip_wrapping_quotes(s: &str) -> &str {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' || first == b'\'') && first == last {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// For `<shell> … -c <payload>`, return the payload command line (the words
/// after the `-c`/bundled-`c` flag, with surrounding quotes peeled). Returns
/// `None` when there is no `-c` flag (e.g. `bash script.sh`, whose file we
/// cannot inspect).
fn dash_c_payload(rest: &[&str]) -> Option<String> {
    let pos = rest
        .iter()
        .position(|t| is_short_flag(t) && t.contains('c'))?;
    let payload = rest[pos + 1..].join(" ");
    let inner = strip_wrapping_quotes(&payload);
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

fn analyze_segment(seg: &str, depth: usize) -> Decision {
    let tokens: Vec<&str> = seg.split_whitespace().collect();
    let idx = match command_index(&tokens) {
        Some(i) => i,
        None => return Decision::Allow,
    };
    let cmd = basename(tokens[idx]);
    let rest = &tokens[idx + 1..];

    // A help invocation never destroys anything.
    if rest.iter().any(|t| *t == "--help" || *t == "-h") {
        return Decision::Allow;
    }

    // Shell-evaluation wrappers that would otherwise smuggle a destructive
    // command past the per-command analysis. We re-analyse the *inline*
    // command line they evaluate; an opaque file argument (e.g.
    // `source .venv/bin/activate`) re-analyses to a harmless path and stays
    // allowed, preserving the no-false-positive bias.
    if depth < MAX_SHELL_DEPTH {
        // `eval`/`exec`/`source`/`.` run their remaining words as a command.
        if matches!(cmd, "eval" | "exec" | "source" | ".") && !rest.is_empty() {
            let joined = rest.join(" ");
            let inline = strip_wrapping_quotes(&joined);
            let d = detect_bash(inline, depth + 1);
            if d.is_deny() {
                return d;
            }
        }
        // `sh -c "<payload>"` and friends evaluate the `-c` argument.
        if is_shell(cmd) {
            if let Some(payload) = dash_c_payload(rest) {
                let d = detect_bash(&payload, depth + 1);
                if d.is_deny() {
                    return d;
                }
            }
        }
    }

    match cmd {
        "rm" => analyze_rm(rest),
        "git" => analyze_git(rest),
        "find" => analyze_find(rest),
        "xargs" => analyze_xargs(rest, depth),
        "truncate" => Decision::deny("truncate can shrink a file to zero bytes"),
        "shred" => Decision::deny("shred destroys file contents irreversibly"),
        "dd" => {
            if rest.iter().any(|t| t.starts_with("of=")) {
                Decision::deny("dd with of= writes raw bytes over a device/file")
            } else {
                Decision::Allow
            }
        }
        "chmod" => {
            if has_short(rest, 'R') || rest.contains(&"--recursive") {
                Decision::deny("recursive chmod re-permissions a whole tree")
            } else {
                Decision::Allow
            }
        }
        "chown" => {
            if has_short(rest, 'R') || rest.contains(&"--recursive") {
                Decision::deny("recursive chown re-owns a whole tree")
            } else {
                Decision::Allow
            }
        }
        other => {
            if other.starts_with("mkfs") {
                Decision::deny("mkfs formats a filesystem, destroying all data")
            } else {
                Decision::Allow
            }
        }
    }
}

/// True when `tok` contains an unescaped shell glob metacharacter (`*`, `?`,
/// `[`) — i.e. it is a wildcard pattern that can expand to many paths, not a
/// single literal filename.
fn has_glob_meta(tok: &str) -> bool {
    let mut escaped = false;
    for c in tok.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '*' | '?' | '[' => return true,
            _ => {}
        }
    }
    false
}

fn analyze_rm(rest: &[&str]) -> Decision {
    let recursive = rest
        .iter()
        .any(|t| (is_short_flag(t) && (t.contains('r') || t.contains('R'))) || *t == "--recursive");
    let operands: Vec<&str> = rest
        .iter()
        .filter(|t| !t.starts_with('-'))
        .copied()
        .collect();
    let wildcard = operands.iter().any(|o| has_glob_meta(o));

    if !recursive && !wildcard {
        // Single, non-recursive rm of named files — below the destructive bar.
        return Decision::Allow;
    }

    // Destructive shape. Exempt only when every operand is a known, *literal*
    // config file. A wildcard operand (`*.toml`, `*.lock`) must never qualify:
    // it self-matches the config globs and would otherwise re-open the gate for
    // an unbounded wildcard delete.
    if !operands.is_empty()
        && operands
            .iter()
            .all(|o| !has_glob_meta(o) && exclude::is_config_file(o))
    {
        return Decision::Allow;
    }

    if recursive {
        Decision::deny("recursive rm (-r) can delete an entire directory tree")
    } else {
        Decision::deny("rm with a wildcard can delete many files at once")
    }
}

fn analyze_git(rest: &[&str]) -> Decision {
    let sub = rest
        .iter()
        .find(|t| !t.starts_with('-'))
        .map(|t| basename(t))
        .unwrap_or("");
    match sub {
        "clean" => {
            let has_f = has_short(rest, 'f') || rest.contains(&"--force");
            let has_d = has_short(rest, 'd');
            let has_x = has_short(rest, 'x');
            if has_f && (has_d || has_x) {
                Decision::deny("git clean -f with -d/-x deletes untracked files & dirs")
            } else {
                Decision::Allow
            }
        }
        "reset" => {
            if rest.contains(&"--hard") {
                Decision::deny("git reset --hard discards working-tree changes")
            } else {
                Decision::Allow
            }
        }
        "checkout" => {
            if rest.contains(&"--force") || has_short(rest, 'f') {
                return Decision::deny("git checkout --force discards working-tree changes");
            }
            if let Some(pos) = rest.iter().position(|t| *t == "--") {
                if rest[pos + 1..].iter().any(|t| *t == "." || *t == "./") {
                    return Decision::deny("git checkout -- . discards all working-tree changes");
                }
            }
            Decision::Allow
        }
        _ => Decision::Allow,
    }
}

fn analyze_find(rest: &[&str]) -> Decision {
    if rest.contains(&"-delete") {
        return Decision::deny("find -delete removes every matching file");
    }
    if let Some(pos) = rest.iter().position(|t| *t == "-exec" || *t == "-execdir") {
        // The token right after -exec is the command run for each match.
        // A shell there (`find … -exec sh -c "rm …"`) can run any destructive
        // command and slips past a literal `rm` scan.
        if let Some(c) = rest.get(pos + 1).map(|t| basename(t)) {
            if is_shell(c) {
                return Decision::deny(
                    "find -exec on a shell can run an arbitrary destructive command per match",
                );
            }
            // A non-shell interpreter with an inline-eval flag (`python3 -c`,
            // `perl -e`, `node -e`, …) is equally dangerous: the payload runs
            // per match and slips past the literal `rm` scan below.
            if is_code_interpreter(c) && rest[pos + 1..].iter().any(|t| is_inline_eval_flag(t)) {
                return Decision::deny(
                    "find -exec on a code interpreter with an inline-eval flag can run an arbitrary destructive command per match",
                );
            }
        }
        if rest.iter().any(|t| basename(t) == "rm") {
            return Decision::deny("find -exec rm removes every matching file");
        }
    }
    Decision::Allow
}

/// `xargs` runs a command assembled from its trailing args (after xargs's own
/// option flags), appending the piped items. Without this branch a destructive
/// payload (`find … | xargs rm -rf`, `xargs -I{} sh -c "rm -rf {}"`) fell through
/// to the catch-all Allow arm. Re-analyse the inner command through `detect_bash`
/// so it reuses the rm / shell-`-c` / find logic (and the recursion bound).
fn analyze_xargs(rest: &[&str], depth: usize) -> Decision {
    if depth >= MAX_SHELL_DEPTH {
        return Decision::Allow;
    }
    match xargs_command_start(rest) {
        Some(start) => {
            let inner = rest[start..].join(" ");
            if inner.trim().is_empty() {
                Decision::Allow
            } else {
                detect_bash(&inner, depth + 1)
            }
        }
        None => Decision::Allow,
    }
}

/// Index in `rest` of the command word xargs will execute, skipping xargs's own
/// option flags (and the separate-token value of a value-taking short flag).
/// `--` ends option parsing. Returns None when no command word follows (xargs
/// with only flags just echoes its input — nothing destructive to re-analyse).
fn xargs_command_start(rest: &[&str]) -> Option<usize> {
    // Value-taking xargs short flags whose value is the NEXT token when the flag
    // is given standalone (`-I {}`, `-n 1`, BSD `-J %`). Bundled forms (`-I{}`,
    // `-n1`, `-J%`) embed the value and consume only themselves. Covers both GNU
    // (I i n L P s d E a) and BSD/macOS (J R S) — the default xargs on this host —
    // because a MISSED value flag would misread its value as the command word and
    // leak a destructive payload (`xargs -J % rm -rf %`) past re-analysis.
    const VALUE_SHORT: &str = "IinLPsdEaJRS";
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t == "--" {
            return if i + 1 < rest.len() {
                Some(i + 1)
            } else {
                None
            };
        }
        if let Some(stripped) = t.strip_prefix("--") {
            // Long flag: value-taking ones consume the next token unless `=`-joined.
            let takes = matches!(
                stripped.split('=').next().unwrap_or(""),
                "max-args"
                    | "max-lines"
                    | "max-procs"
                    | "max-chars"
                    | "delimiter"
                    | "eof"
                    | "arg-file"
                    | "replace"
                    | "process-slot-var"
            );
            i += if takes && !t.contains('=') { 2 } else { 1 };
            continue;
        }
        if t.starts_with('-') && t.len() >= 2 {
            // Short flag (bundle). A standalone value-taking flag (`-I`, `-n`)
            // consumes the next token; a bundle that embeds the value (`-I{}`,
            // `-n1`) or a boolean bundle (`-0rt`) consumes only itself.
            let last = t.chars().last().unwrap();
            let standalone_value = t.len() == 2 && VALUE_SHORT.contains(last);
            i += if standalone_value { 2 } else { 1 };
            continue;
        }
        return Some(i); // first non-flag token = the command word
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash(cmd: &str) -> Decision {
        detect("Bash", Some(&json!({ "command": cmd })))
    }

    // ---- Bash: deny group ----
    #[test]
    fn denies_recursive_and_wildcard_rm() {
        assert!(bash("rm -rf dir").is_deny());
        assert!(bash("rm -fr dir").is_deny());
        assert!(bash("rm -Rf dir").is_deny());
        assert!(bash("rm -r -f dir").is_deny());
        assert!(bash("rm --recursive build").is_deny());
        assert!(bash("rm *").is_deny());
        assert!(bash("rm -f *").is_deny());
        assert!(bash("rm path/*").is_deny());
        assert!(bash("sudo rm -rf /var/data").is_deny());
    }

    #[test]
    fn denies_destructive_git() {
        assert!(bash("git clean -fdx").is_deny());
        assert!(bash("git clean -fd").is_deny());
        assert!(bash("git clean -f -d").is_deny());
        assert!(bash("git reset --hard").is_deny());
        assert!(bash("git reset --hard HEAD~3").is_deny());
        assert!(bash("git checkout -- .").is_deny());
        assert!(bash("git checkout --force").is_deny());
        assert!(bash("git checkout -f").is_deny());
    }

    #[test]
    fn denies_truncate_shred_mkfs_dd_chmod_chown_find() {
        assert!(bash("truncate -s0 x").is_deny());
        assert!(bash("truncate -s 0 file").is_deny());
        assert!(bash("shred secret").is_deny());
        assert!(bash("mkfs.ext4 /dev/sdb1").is_deny());
        assert!(bash("dd of=/dev/sda").is_deny());
        assert!(bash("dd if=/dev/zero of=/dev/sda bs=1M").is_deny());
        assert!(bash("chmod -R 777 .").is_deny());
        assert!(bash("chmod --recursive 755 src").is_deny());
        assert!(bash("chown -R root .").is_deny());
        assert!(bash("find . -delete").is_deny());
        assert!(bash("find . -name '*.log' -delete").is_deny());
        assert!(bash("find . -type f -exec rm {} ;").is_deny());
    }

    #[test]
    fn denies_truncating_redirect_and_fork_bomb() {
        assert!(bash("echo x > existing").is_deny());
        assert!(bash("cat a > b.txt").is_deny());
        assert!(bash(":(){ :|:& };:").is_deny());
    }

    #[test]
    fn denies_shell_eval_bypasses() {
        // eval / exec run their inline arguments as a command.
        assert!(bash("eval \"rm -rf /\"").is_deny());
        assert!(bash("eval 'rm -rf /'").is_deny());
        assert!(bash("exec rm -rf /").is_deny());
        // <shell> -c "<payload>" re-analyses the payload.
        assert!(bash("sh -c \"rm -rf /\"").is_deny());
        assert!(bash("bash -c 'shred secret'").is_deny());
        assert!(bash("zsh -c \"rm -rf /var\"").is_deny());
        assert!(bash("bash -lc \"rm -rf /\"").is_deny());
        // find -exec/-execdir on a shell can run any destructive command.
        assert!(bash("find . -type f -exec sh -c 'rm -rf {}' ;").is_deny());
        assert!(bash("find . -execdir bash -c \"rm -rf .\" ;").is_deny());
        // CA-blastguard-001: a NON-shell interpreter with an inline-eval flag
        // (python -c, perl -e, node -e, …) is equally destructive per match and
        // used to fall through to Allow (only sh-family shells were caught).
        assert!(
            bash("find . -type f -exec python3 -c \"import os; os.system('rm -rf /')\" ;")
                .is_deny()
        );
        assert!(bash("find . -exec perl -e 'unlink glob \"*\"' ;").is_deny());
        assert!(
            bash("find . -execdir node -e \"require('fs').rmSync('/',{recursive:true})\" ;")
                .is_deny()
        );
        // …but a plain script-file exec (no inline-eval flag) is NOT over-blocked.
        assert_eq!(bash("find . -exec python3 script.py ;"), Decision::Allow);
        // A benign wrapper in front still resolves to the eval builtin.
        assert!(bash("sudo eval \"rm -rf /\"").is_deny());
        // Nested wrapping is caught up to the recursion bound.
        assert!(bash("sh -c \"sh -c 'rm -rf /'\"").is_deny());
    }

    #[test]
    fn absolute_path_and_leading_whitespace_rm_still_denied() {
        // Regression: basename() already normalises an absolute path, and
        // split_whitespace() drops leading blanks — both stay denied.
        assert!(bash("/bin/rm -rf /data").is_deny());
        assert!(bash("  rm -rf dir").is_deny());
        assert!(bash("\trm -rf dir").is_deny());
    }

    // ---- Bash: allow group ----
    #[test]
    fn allows_benign_commands() {
        assert_eq!(bash("ls"), Decision::Allow);
        assert_eq!(bash("ls -la"), Decision::Allow);
        assert_eq!(bash("cat f"), Decision::Allow);
        assert_eq!(bash("git status"), Decision::Allow);
        assert_eq!(bash("git diff"), Decision::Allow);
        assert_eq!(bash("cargo test"), Decision::Allow);
        assert_eq!(bash("cargo build -p blastguard"), Decision::Allow);
        assert_eq!(bash("mkdir -p a/b"), Decision::Allow);
        assert_eq!(bash("rm notes.txt"), Decision::Allow);
        assert_eq!(bash("rm a.txt b.txt"), Decision::Allow);
        assert_eq!(bash("git checkout main"), Decision::Allow);
        assert_eq!(bash("git checkout -b feature"), Decision::Allow);
        assert_eq!(bash("git clean -n"), Decision::Allow);
        assert_eq!(bash("chmod 644 file"), Decision::Allow);
        assert_eq!(bash("find . -name '*.rs'"), Decision::Allow);
        assert_eq!(bash("rm --help"), Decision::Allow);
    }

    #[test]
    fn allows_benign_shell_wrappers() {
        // Quoted text that merely mentions a destructive command is not run.
        assert_eq!(bash("echo 'eval rm -rf /'"), Decision::Allow);
        // Help and benign payloads stay allowed.
        assert_eq!(bash("sh --help"), Decision::Allow);
        assert_eq!(bash("bash -c \"cargo test\""), Decision::Allow);
        assert_eq!(bash("sh -c 'ls -la'"), Decision::Allow);
        // source/. of an opaque file path cannot be inspected — allowed
        // (re-analyse-inline bias: a bare path is not a destructive command).
        assert_eq!(bash("source .venv/bin/activate"), Decision::Allow);
        assert_eq!(bash(". ~/.bashrc"), Decision::Allow);
        // A shell running a script file (no -c) — file unknowable, allowed.
        assert_eq!(bash("bash build.sh"), Decision::Allow);
        // find -exec with a non-shell, non-rm command.
        assert_eq!(
            bash("find . -name '*.rs' -exec grep todo {} ;"),
            Decision::Allow
        );
    }

    #[test]
    fn allows_rm_of_config_files_even_when_destructive() {
        // Single config file, non-recursive — allowed anyway.
        assert_eq!(bash("rm package.json"), Decision::Allow);
        // Recursive rm whose only target is a config tree.
        assert_eq!(bash("rm -rf .claude"), Decision::Allow);
        assert_eq!(bash("rm -f Cargo.lock"), Decision::Allow);
    }

    // ---- Bash: boundary cases (no false positives) ----
    #[test]
    fn append_and_fd_redirects_are_not_truncation() {
        assert_eq!(bash("echo x >> log.txt"), Decision::Allow);
        assert_eq!(bash("cargo test 2>&1"), Decision::Allow);
        assert_eq!(bash("cargo build 2> err.log"), Decision::Allow);
        assert_eq!(bash("make >&2"), Decision::Allow);
    }

    #[test]
    fn redirect_to_devnull_and_config_is_allowed() {
        assert_eq!(bash("echo hi > /dev/null"), Decision::Allow);
        assert_eq!(bash("generate > config.toml"), Decision::Allow);
    }

    #[test]
    fn quoted_destructive_text_is_not_executed() {
        // The dangerous text lives inside an echo string, not as a command.
        assert_eq!(bash("echo 'rm -rf /'"), Decision::Allow);
        assert_eq!(bash("echo \"a > b\""), Decision::Allow);
    }

    // ---- Redirect false-positives: Rust arrows & angle-bracket placeholders ----
    // Regression: descriptive prose containing a Rust return-type arrow (`->`)
    // or an angle-bracket identifier placeholder (`<value>`) must not be read as
    // a truncating `>` redirect (which would force-gate benign condukt tasks).
    #[test]
    fn benign_rust_arrow_is_not_a_redirect() {
        assert_eq!(single_redirect_target("fn foo() -> Bar"), None);
        assert_eq!(bash("implement fn parse() -> Result"), Decision::Allow);
        assert_eq!(bash("map the input -> output pipeline"), Decision::Allow);
    }

    #[test]
    fn benign_angle_bracket_placeholder_is_not_a_redirect() {
        assert_eq!(single_redirect_target("pass --flag <value>"), None);
        assert_eq!(bash("run with --flag <value>"), Decision::Allow);
        assert_eq!(bash("tdd red --task <id> --cmd foo"), Decision::Allow);
        assert_eq!(bash("emit {risk} for <RID>"), Decision::Allow);
    }

    #[test]
    fn benign_lookalike_task_prose_is_low_and_reversible() {
        // The end-to-end path the bug broke: classify() over a task's prose.
        let a = crate::classify::classify("implement fn parse() -> Result and pass --flag <value>");
        assert_eq!(a.risk, crate::classify::Risk::Low);
        assert!(a.reversible);
        assert!(!a.requires_gate());
    }

    #[test]
    fn benign_fix_keeps_real_truncating_redirect_denied() {
        // Guard the other direction: a genuine `> path` redirect stays denied.
        assert!(bash("cat x > /etc/passwd").is_deny());
        assert!(bash("echo x > existing").is_deny());
        // Arrow/placeholder skip must not swallow a real redirect on the line.
        assert!(bash("echo done -> nope; cat x > realfile").is_deny());
    }

    // ---- Regression: fail-open defects (CA-blastguard-01 / -02) ----
    #[test]
    fn later_segment_truncating_redirect_is_denied() {
        // CA-blastguard-01: a safe early redirect must not blind the gate to a
        // later truncating redirect in a subsequent segment.
        assert!(bash("cat notes.txt > important.txt").is_deny()); // control
        assert!(bash("echo hi > /dev/null; cat notes.txt > important.txt").is_deny());
    }

    #[test]
    fn wildcard_rm_is_not_config_exempt() {
        // CA-blastguard-02: a wildcard operand must not self-match a config glob
        // (`*.toml` matching the `*.toml` allow-glob) and slip past as exempt.
        assert!(bash("rm -rf *.toml").is_deny());
        assert!(bash("rm *.lock").is_deny());
        // Control: a single literal config file stays exempt.
        assert_eq!(bash("rm Cargo.toml"), Decision::Allow);
    }

    // ---- Regression: round-2026W28 CONFIRMED fail-opens (CA-blastguard-01/02/03) ----
    #[test]
    fn explicit_stdout_fd_truncating_redirect_is_denied() {
        // CA-blastguard-03: `1>file` is an explicit stdout truncating redirect,
        // semantically identical to bare `>file` (denied) — must also be denied.
        // Only stderr (`2>file`) and fd-dup (`2>&1`, `1>&2`) forms stay allowed.
        assert!(bash("echo x 1> existing").is_deny());
        assert!(bash("cat a 1>b.txt").is_deny());
        // No regression: stderr redirect and fd-dup forms stay allowed.
        assert_eq!(bash("cargo build 2> err.log"), Decision::Allow);
        assert_eq!(bash("cargo test 2>&1"), Decision::Allow);
        assert_eq!(bash("run 1>&2"), Decision::Allow);
        // Multi-digit explicit fds are NOT stdout — only the fd number `1`
        // truncates stdout. `11>`, `21>` (end in 1 but aren't fd 1) stay allowed;
        // guards against reading just the single byte before `>`.
        assert_eq!(bash("echo hi 11> file.txt"), Decision::Allow);
        assert_eq!(bash("echo hi 21> out.txt"), Decision::Allow);
        assert_eq!(bash("echo hi 10> out.txt"), Decision::Allow);
    }

    #[test]
    fn rm_question_and_bracket_globs_are_denied() {
        // CA-blastguard-02b: `?` and `[` globs expand to many files like `*` does,
        // but the main wildcard check only looked for `*` (has_glob_meta existed,
        // unused). They must be denied.
        assert!(bash("rm ?.txt").is_deny());
        assert!(bash("rm [0-9].txt").is_deny());
        assert!(bash("rm file?").is_deny());
        // Control: a literal filename (no glob metachar) stays allowed.
        assert_eq!(bash("rm notes.txt"), Decision::Allow);
    }

    #[test]
    fn xargs_destructive_payload_is_denied() {
        // CA-blastguard-01: xargs runs a command assembled from its trailing args;
        // a destructive payload was passed straight through (no xargs branch).
        assert!(bash("find . | xargs rm -rf").is_deny());
        assert!(bash("find . -type f | xargs -I{} sh -c \"rm -rf {}\"").is_deny());
        assert!(bash("ls | xargs -n1 rm -rf").is_deny());
        // BSD/macOS xargs `-J <replstr>` is value-taking too; its value token must
        // not be mistaken for the command word (would leak the payload past re-analysis).
        assert!(bash("find . | xargs -J % rm -rf %").is_deny());
        assert!(bash("find . -type f | xargs -J % sh -c \"rm -rf %\"").is_deny());
        // Controls: benign xargs payloads stay allowed (no false positive).
        assert_eq!(
            bash("find . -name '*.rs' | xargs grep todo"),
            Decision::Allow
        );
        assert_eq!(bash("ls | xargs -n1 echo"), Decision::Allow);
    }

    // ---- File operations ----
    #[test]
    fn edit_is_always_allowed() {
        assert_eq!(
            detect("Edit", Some(&json!({ "file_path": "src/main.rs" }))),
            Decision::Allow
        );
        assert_eq!(
            detect("MultiEdit", Some(&json!({ "file_path": "src/main.rs" }))),
            Decision::Allow
        );
    }

    #[test]
    fn write_empty_content_to_source_is_denied() {
        assert!(detect(
            "Write",
            Some(&json!({ "file_path": "src/main.rs", "content": "" }))
        )
        .is_deny());
        assert!(detect(
            "Write",
            Some(&json!({ "file_path": "src/main.rs", "content": "   \n" }))
        )
        .is_deny());
    }

    #[test]
    fn write_with_content_or_to_config_is_allowed() {
        assert_eq!(
            detect(
                "Write",
                Some(&json!({ "file_path": "src/main.rs", "content": "fn main() {}" }))
            ),
            Decision::Allow
        );
        // Config files are exempt even when emptied.
        assert_eq!(
            detect(
                "Write",
                Some(&json!({ "file_path": "Cargo.toml", "content": "" }))
            ),
            Decision::Allow
        );
        assert_eq!(
            detect(
                "Write",
                Some(&json!({ "file_path": ".claude/settings.json", "content": "" }))
            ),
            Decision::Allow
        );
    }

    #[test]
    fn write_to_git_internals_is_denied() {
        assert!(detect(
            "Write",
            Some(&json!({ "file_path": ".git/config", "content": "x" }))
        )
        .is_deny());
    }

    #[test]
    fn missing_or_unknown_input_is_allowed() {
        assert_eq!(detect("Bash", None), Decision::Allow);
        assert_eq!(
            detect("Read", Some(&json!({ "file_path": "x" }))),
            Decision::Allow
        );
        assert_eq!(detect("Write", Some(&json!({}))), Decision::Allow);
    }
}
