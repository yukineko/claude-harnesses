//! `sitecraft check security` — block on:
//!
//! * secrets in build output (`build_dir`, including `*.map`): fixed-prefix
//!   patterns (AWS access key, GitHub tokens, Stripe secret keys, private-key
//!   blocks with a body, Google API keys); matches are printed redacted;
//! * dangerous HTML sinks in sources (`innerHTML` assignment of a
//!   non-constant, `dangerouslySetInnerHTML`, `v-html`, `{@html}`, `eval`,
//!   `new Function`), excluding `node_modules` and `dist`;
//! * `npm audit --json` (`$SITECRAFT_NPM` / `npm`) reporting high or critical
//!   vulnerabilities, judged from the JSON shape, not the exit code.
//!
//! Undetermined: audit JSON without all six `metadata.vulnerabilities` keys,
//! an `error` object (including ENOLOCK), empty or invalid stdout, missing
//! npm, or zero files to scan.

use std::path::Path;

use crate::config::Config;
use crate::Verdict;

/// Run the security check for `root`.
///
/// Contract: config not loadable (`Absent` or `Undetermined`) →
/// `Undetermined`; otherwise [`check_security`].
pub fn run(root: &Path) -> Verdict {
    Verdict::undetermined(format!(
        "sitecraft check security: not implemented yet (root {})",
        root.display()
    ))
}

/// Run the three scans for `root` with `config` and combine them with
/// [`Verdict::worst_of`]. `Clean` only when every scan ran on a non-empty
/// target set and found nothing; violation output names file, line and fix.
pub fn check_security(config: &Config, root: &Path) -> Verdict {
    Verdict::undetermined(format!(
        "sitecraft check security: not implemented yet (root {}, build_dir {})",
        root.display(),
        config.build_dir
    ))
}
