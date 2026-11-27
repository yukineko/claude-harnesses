//! HTTP-client / GitHub-API / "arguments are data" judgments split out of
//! `detect.rs` (file-growth cap). Same three-valued contract: a master-only
//! effect is `found`, "cannot tell" is `undet`, anything else is silent.

use super::{joined, Arg, Op, Walker};

const WEB_DATA_LONG: &[&str] = &[
    "data",
    "data-raw",
    "data-binary",
    "data-ascii",
    "data-urlencode",
    "form",
    "form-string",
    "upload-file",
    "json",
    "post-data",
    "post-file",
    "body-data",
    "body-file",
];

/// Long options that take a value (the data options above do too).
const WEB_VALUE_LONG: &[&str] = &[
    "header",
    "output",
    "output-document",
    "user",
    "user-agent",
    "referer",
    "cookie",
    "cookie-jar",
    "max-time",
    "connect-timeout",
    "write-out",
    "proxy",
    "retry",
    "cacert",
    "cert",
    "config",
    "directory-prefix",
];

fn upper(s: &str) -> String {
    s.trim_matches(['"', '\'']).to_ascii_uppercase()
}

/// A GitHub REST path (`repos/<o>/<r>/...`, absolute URL or not) that merges
/// or pushes: `.../merge`, `.../merges`, `.../merge-upstream`, anything under
/// `git/` (refs, commits, trees) or `contents/`. Judged on the raw text, so
/// `repos/o/r/pulls/$N/merge` still matches.
fn github_write_target(raw: &str) -> Option<Op> {
    let path = raw.split(['?', '#']).next().unwrap_or(raw);
    let segs: Vec<&str> = path
        .trim_matches(['"', '\''])
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let repos = segs.iter().position(|s| *s == "repos")?;
    let tail = &segs[repos + 1..];
    if tail.iter().any(|s| s.starts_with("merge")) {
        return Some(Op::Merge);
    }
    if tail.iter().skip(2).any(|s| *s == "git" || *s == "contents") {
        return Some(Op::Push);
    }
    None
}

/// Programs known not to run their arguments as commands on this host, so an
/// argument that merely *names* git/ssh (`man git`, `brew install gh`,
/// `docker exec c git status`, `npm test -- --grep ssh`) is data. Anything
/// else unknown keeps the wrapper rule. Build tools (`npm test`, `make`) run
/// scripts whose content is not read: the same known limit as before.
pub(super) fn args_are_data(base: &str, args: &[Arg]) -> bool {
    let first_pos = args.iter().find_map(|a| match a.lit.as_deref() {
        Some(s) if s.starts_with('-') => None,
        Some(s) => Some(Some(s)),
        None => Some(None),
    });
    let sub_in = |subs: &[&str]| matches!(first_pos, Some(Some(s)) if subs.contains(&s));
    match base {
        "man" | "info" | "tldr" | "whatis" | "apropos" => true,
        "brew" => sub_in(&[
            "install",
            "uninstall",
            "reinstall",
            "upgrade",
            "update",
            "list",
            "info",
            "search",
            "outdated",
            "cleanup",
            "doctor",
            "fetch",
            "deps",
            "uses",
            "leaves",
            "remove",
            "pin",
            "unpin",
            "desc",
            "home",
        ]),
        "fd" | "fdfind" => !args.iter().any(|a| match a.lit.as_deref() {
            None => true,
            Some(s) => {
                s.starts_with("--exec")
                    || (s.starts_with('-') && !s.starts_with("--") && s[1..].contains(['x', 'X']))
            }
        }),
        // `exec` runs inside the container, `logs`/`ps`/... run nothing.
        "docker" | "podman" => sub_in(&[
            "exec", "logs", "ps", "images", "inspect", "pull", "stop", "start", "restart", "rm",
            "top", "stats", "version", "info", "cp",
        ]),
        "npm" | "pnpm" | "yarn" | "bun" => sub_in(&[
            "test",
            "t",
            "run",
            "run-script",
            "install",
            "i",
            "ci",
            "ls",
            "list",
            "outdated",
            "audit",
            "view",
            "info",
            "start",
            "build",
            "lint",
            "pack",
        ]),
        _ => false,
    }
}

impl Walker {
    /// `gh api ...` with a write method (or `-f/-F` fields, which make it a
    /// POST) to a GitHub endpoint that merges or pushes is master-only; a
    /// write to an endpoint that cannot be read statically is undetermined.
    pub(super) fn gh_api(&mut self, args: &[Arg], rest: &[Arg]) {
        let mut method: Option<Option<String>> = None;
        let mut fields = false;
        let mut field_texts: Vec<&Arg> = Vec::new();
        let mut endpoint: Option<&Arg> = None;
        let mut seen_api = false;
        let mut j = 0usize;
        while let Some(a) = args.get(j) {
            let Some(s) = a.lit.as_deref() else {
                if seen_api && endpoint.is_none() {
                    endpoint = Some(a);
                }
                j += 1;
                continue;
            };
            if !seen_api {
                if s == "api" {
                    seen_api = true;
                    j += 1;
                } else if matches!(s, "-R" | "--repo") {
                    j += 2;
                } else {
                    j += 1;
                }
                continue;
            }
            match s {
                "-X" | "--method" => {
                    method = Some(args.get(j + 1).and_then(|v| v.lit.as_deref()).map(upper));
                    j += 2;
                }
                "-f" | "-F" | "--field" | "--raw-field" | "--input" => {
                    fields = true;
                    field_texts.extend(args.get(j + 1));
                    j += 2;
                }
                "-H" | "--header" | "-q" | "--jq" | "-t" | "--template" | "--hostname"
                | "--cache" | "-p" | "--preview" => j += 2,
                _ if s.starts_with("--method=") => {
                    method = Some(Some(upper(&s["--method=".len()..])));
                    j += 1;
                }
                _ if s.starts_with("--field=")
                    || s.starts_with("--raw-field=")
                    || s.starts_with("--input=") =>
                {
                    fields = true;
                    field_texts.push(a);
                    j += 1;
                }
                _ if s.starts_with("-X") && s.len() > 2 => {
                    method = Some(Some(upper(&s[2..])));
                    j += 1;
                }
                _ if (s.starts_with("-f") || s.starts_with("-F")) && s.len() > 2 => {
                    fields = true;
                    field_texts.push(a);
                    j += 1;
                }
                _ if s.starts_with('-') => j += 1,
                _ => {
                    if endpoint.is_none() {
                        endpoint = Some(a);
                    }
                    j += 1;
                }
            }
        }
        let write = match &method {
            Some(None) => true,
            Some(Some(m)) => !matches!(m.as_str(), "GET" | "HEAD"),
            None => fields,
        };
        if !write {
            return;
        }
        let Some(ep) = endpoint else {
            self.undet(&joined(rest), "gh api の write の endpoint が読めない");
            return;
        };
        if ep.raw.trim_matches(['"', '\'']).trim_start_matches('/') == "graphql" {
            let risky = field_texts
                .iter()
                .any(|f| f.lit.is_none() || f.raw.to_ascii_lowercase().contains("mutation"))
                || method.as_ref().is_some_and(|m| m.is_none());
            if risky {
                self.undet(
                    &joined(rest),
                    "gh api graphql の mutation（または読めない query）",
                );
            }
            return;
        }
        match github_write_target(&ep.raw) {
            Some(op) => self.found(op),
            None if ep.lit.is_none() => self.undet(
                &joined(rest),
                "gh api の write の endpoint が literal でない（merge/push の可能性）",
            ),
            None => {}
        }
    }

    /// `curl`/`wget`/`http`/`xh` that writes (explicit method other than
    /// GET/HEAD, or data/form/upload flags, or httpie data items) to a GitHub
    /// merge/push endpoint is master-only; a write whose URL cannot be read
    /// statically is undetermined.
    pub(super) fn web_client(&mut self, base: &str, args: &[Arg], rest: &[Arg]) {
        let httpie = matches!(base, "http" | "https" | "xh" | "xhs");
        let value_chars: &str = if base == "wget" {
            "XdFTHouAebcmwxKPOt"
        } else {
            "XdFTHouAebcmwxKPt"
        };
        let mut explicit_write = false;
        let mut explicit_read = false;
        let mut method_unknown = false;
        let mut implicit = false;
        let mut pos: Vec<&Arg> = Vec::new();
        let mut set_method = |v: &str| {
            let m = upper(v);
            if matches!(m.as_str(), "GET" | "HEAD" | "OPTIONS") {
                explicit_read = true;
            } else {
                explicit_write = true;
            }
        };
        let mut j = 0usize;
        while let Some(a) = args.get(j) {
            let Some(s) = a.lit.as_deref() else {
                pos.push(a);
                j += 1;
                continue;
            };
            if s == "--" {
                j += 1;
            } else if let Some(v) = s
                .strip_prefix("--request=")
                .or_else(|| s.strip_prefix("--method="))
            {
                set_method(v);
                j += 1;
            } else if matches!(s, "-X" | "--request" | "--method") {
                match args.get(j + 1).and_then(|v| v.lit.as_deref()) {
                    Some(v) => set_method(v),
                    None => method_unknown = true,
                }
                j += 2;
            } else if let Some(long) = s.strip_prefix("--") {
                let name = long.split('=').next().unwrap_or(long);
                let is_data = WEB_DATA_LONG.contains(&name);
                if is_data {
                    implicit = true;
                }
                if !long.contains('=') && (is_data || WEB_VALUE_LONG.contains(&name)) {
                    j += 2;
                } else {
                    j += 1;
                }
            } else if s.starts_with('-') && s.len() > 1 {
                // A short cluster: the first value-taking letter takes the
                // rest of the token, or the next word, as its value.
                let body = &s[1..];
                match body.char_indices().find(|(_, c)| value_chars.contains(*c)) {
                    Some((ci, c)) => {
                        let inline = &body[ci + c.len_utf8()..];
                        if matches!(c, 'd' | 'F' | 'T') {
                            implicit = true;
                        }
                        if c == 'X' {
                            if inline.is_empty() {
                                match args.get(j + 1).and_then(|v| v.lit.as_deref()) {
                                    Some(v) => set_method(v),
                                    None => method_unknown = true,
                                }
                            } else {
                                set_method(inline);
                            }
                        }
                        j += if inline.is_empty() { 2 } else { 1 };
                    }
                    None => j += 1,
                }
            } else {
                if httpie {
                    if pos.is_empty()
                        && matches!(
                            upper(s).as_str(),
                            "GET" | "HEAD" | "OPTIONS" | "PUT" | "POST" | "PATCH" | "DELETE"
                        )
                    {
                        set_method(s);
                        j += 1;
                        continue;
                    }
                    if (s.contains('=') && !s.contains("==")) && !s.contains("://") {
                        implicit = true;
                    }
                }
                pos.push(a);
                j += 1;
            }
        }
        let write = method_unknown || explicit_write || (implicit && !explicit_read);
        if !write {
            return;
        }
        let mut unresolved = pos.is_empty();
        for a in &pos {
            if let Some(op) = github_write_target(&a.raw) {
                self.found(op);
                return;
            }
            if a.lit.is_none() {
                unresolved = true;
            }
        }
        if unresolved {
            self.undet(
                &joined(rest),
                "HTTP write の URL が読めない（GitHub の merge/push endpoint の可能性）",
            );
        }
    }
}
