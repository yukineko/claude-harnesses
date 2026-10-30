//! `sitecraft check pages` — open every configured page in Playwright (via
//! the bundled `assets/check-pages.mjs`, run by `$SITECRAFT_NODE` / `node`)
//! and block on console errors, page errors, same-origin asset 4xx/5xx and
//! same-origin broken links.
//!
//! Both the subprocess exit status and its stdout JSON decide the verdict;
//! neither alone can yield `Clean`. Zero pages, missing browser, missing
//! Playwright, missing node, unresolvable script, invalid/empty JSON, or a
//! non-zero exit with valid JSON → `Undetermined`.

use std::path::Path;

use crate::config::Config;
use crate::Verdict;

/// Check pages of `root` against a running server.
///
/// `base_url` is the server origin; `None` means `http://127.0.0.1:<port>`
/// from the config. Contract: config not loadable (`Absent` or
/// `Undetermined`) → `Undetermined`; otherwise [`check_pages`].
pub fn run(root: &Path, base_url: Option<&str>) -> Verdict {
    let _ = base_url;
    Verdict::undetermined(format!(
        "sitecraft check pages: not implemented yet (root {})",
        root.display()
    ))
}

/// Check `config.pages` at `base_url`.
///
/// Contract: `Clean` iff the script exited 0, its JSON parsed against the
/// schema, it checked at least one page, and reported zero findings;
/// `Violation` (naming page URL and kind) iff it ran correctly and reported
/// findings; `Undetermined` in every other case (see module docs). Empty
/// `pages` → `Undetermined` without spawning node.
pub fn check_pages(config: &Config, base_url: &str) -> Verdict {
    Verdict::undetermined(format!(
        "sitecraft check pages: not implemented yet ({} pages at {base_url})",
        config.pages.len()
    ))
}
