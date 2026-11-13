//! The input fds an `exec` opens for the rest of a command line
//! (`exec 3< <(…); bash <&3`, backlog 72872200).

use super::{parse_redirect, procsub_body};

/// What an fd `exec` opens for the rest of the line refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExecFd {
    /// A process substitution (`exec 3< <(…)`): its command text.
    Procsub(String),
    /// A file, or a standard fd duplicated (`exec 3< f`, `exec 3<&0`): read as
    /// `READER < f` is.
    File,
    /// A here-document / here-string, or a duplicate of an fd above 2: text
    /// blastguard does not read as a program.
    Unreadable,
}

/// The input fds every `exec` stage among `stages` opens for the rest of the
/// line (`exec 3< <(…)`, `exec < <(…)`, `exec 4< f`), in order. Only an
/// `exec` that runs no command (every word after it a redirection) is one: an
/// `exec CMD …` replaces the shell, and its redirections are CMD's. Output
/// redirections are not recorded (nothing reads a program from them).
pub(super) fn exec_opened_fds(stages: &[String]) -> Vec<(u32, ExecFd)> {
    let mut out = Vec::new();
    for stage in stages {
        let words = super::super::quote_aware_words(stage);
        if words.first().map(String::as_str) != Some("exec") {
            continue;
        }
        let mut j = 1;
        while j < words.len() {
            let Some((fd, op, glued)) = parse_redirect(&words[j]) else {
                break;
            };
            let target = if glued.is_empty() {
                j += 1;
                words.get(j).map_or("", String::as_str)
            } else {
                glued
            };
            j += 1;
            if !op.starts_with('<') {
                continue;
            }
            let fd = fd.unwrap_or(0);
            let opened = match op {
                "<" | "<>" => match procsub_body(target) {
                    Some(body) => ExecFd::Procsub(body.to_string()),
                    None => ExecFd::File,
                },
                "<&" => match target.parse::<u32>() {
                    Ok(n) if n <= 2 => ExecFd::File,
                    _ if target == "-" => continue,
                    _ => ExecFd::Unreadable,
                },
                _ => ExecFd::Unreadable,
            };
            out.push((fd, opened));
        }
    }
    out
}
