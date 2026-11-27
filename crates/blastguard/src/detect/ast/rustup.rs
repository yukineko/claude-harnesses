//! When the r21 exemption (`. "$HOME/.cargo/env"` is rustup's PATH setup and
//! runs no text of the line, [`super::is_rustup_env_source`]) no longer
//! holds. Backlog 92f4a28d: the exemption assumed `~/.cargo/env` is the file
//! rustup wrote, but an earlier command of the SAME line can replace it
//! (`ln -sf /dev/stdin ~/.cargo/env`, `cp evil.sh ~/.cargo/env`) or feed the
//! sourced stdin (`exec 0<<< '…'`), so the `.` would run line text.
//!
//! User ruling (92f4a28d): an earlier write whose target cannot be determined
//! drops the exemption (an Ask is the accepted result); any earlier `exec`
//! with a redirection drops it. A bare `. "$HOME/.cargo/env"` stays exempt.
//!
//! [`tainted_by`] is one command's part, read in order by the evaluator;
//! [`line_tainted`] is the whole line's, read by the relink layer through
//! [`super::targets`].

use super::{is_rustup_env_source, program_source, RedirOp, RedirTarget, SimpleCommand, Value};

/// Commands that write, link, move or truncate the files named in their
/// operands.
const WRITERS: &[&str] = &[
    "cp", "gcp", "mv", "gmv", "ln", "gln", "link", "install", "ginstall", "rsync", "tee", "dd",
    "touch", "truncate", "sed", "gsed", "rm", "unlink",
];

/// A literal that may name `~/.cargo/env`, the directory it lives in, or the
/// file by its last component alone (`cd ~/.cargo; cp x env`).
fn may_name_cargo_env(p: &str) -> bool {
    p.contains(".cargo") || p.trim_end_matches('/').rsplit('/').next() == Some("env")
}

fn word_taints(v: &Value) -> bool {
    match v {
        Value::Undetermined(_) => true,
        Value::Literal(p) => may_name_cargo_env(p),
    }
}

/// `sc`, run before an `. "$HOME/.cargo/env"`, may have changed what that
/// `.` sources or the stdin it reads.
pub(crate) fn tainted_by(sc: &SimpleCommand) -> bool {
    if is_rustup_env_source(sc) {
        return false;
    }
    if let Value::Literal(c) = &sc.cwd {
        if may_name_cargo_env(c) && !sc.argv.is_empty() {
            return true;
        }
    }
    let head = match sc.argv.first().map(|w| &w.value) {
        Some(Value::Literal(h)) => Some(h.rsplit('/').next().unwrap_or(h)),
        _ => None,
    };
    // Any earlier `exec` with a redirection (ruling c): it can rebind stdin.
    if head == Some("exec") && !sc.redirects.is_empty() {
        return true;
    }
    for r in &sc.redirects {
        if matches!(
            r.op,
            RedirOp::Out
                | RedirOp::Append
                | RedirOp::Clobber
                | RedirOp::InOut
                | RedirOp::OutErr
                | RedirOp::AppendOutErr
        ) {
            if let RedirTarget::Path(v) = &r.target {
                let written = r.written.as_ref().unwrap_or(v);
                if word_taints(v) || word_taints(written) {
                    return true;
                }
            }
        }
    }
    if head.is_some_and(|h| WRITERS.contains(&h)) && sc.argv.iter().any(|w| word_taints(&w.value)) {
        return true;
    }
    // Code this line runs that is not text here (`bash x.sh`, `eval`, a
    // sourced file, an undetermined wrapper) may do any of the above.
    program_source(sc).is_some()
}

/// Some command before the line's last `. "$HOME/.cargo/env"` taints it.
pub(crate) fn line_tainted(cmds: &[SimpleCommand]) -> bool {
    let Some(last) = cmds
        .iter()
        .filter(|c| is_rustup_env_source(c))
        .map(|c| c.span.start)
        .max()
    else {
        return false;
    };
    cmds.iter().any(|c| c.span.start < last && tainted_by(c))
}
