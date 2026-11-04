use super::{has_unresolvable_expansion, normalized_command, Decision};

/// Classify a `gh` invocation (user rulings 2026-10-29). Never Deny.
///
/// * Read-only commands are Allow: `<group> list|view|status|diff|checks|watch|
///   download|...` for the known groups, `gh search`, `gh status`, `gh auth
///   status`, `gh help`, `gh --version`, `gh config get|list`, `gh extension
///   list`, `gh browse`, `gh repo clone`, and `gh api` that is a GET (no method
///   and no field flag, or an explicit `GET`).
/// * Remote-changing commands are Ask, and the reason names the command (`gh pr
///   merge`, `gh repo delete`, ...): every other subcommand of a known group,
///   `gh api` with POST/PUT/PATCH/DELETE or with a field flag and no explicit GET.
/// * GitHub Actions: observation (`gh run list|view|watch|download`, `gh
///   workflow list|view`, `gh cache list`, `gh api` GET on `/actions/`) is
///   Allow like any read. Executing or configuring it (`gh workflow
///   run|enable|disable`, `gh run rerun|cancel|delete`, `gh cache delete`, `gh
///   secret|variable set|delete`, `gh api` mutating `/actions/`) is Ask with a
///   reason saying GitHub Actions needs an explicit user request. Any
///   non-read subcommand of the `run`/`workflow`/`cache`/`secret`/`variable`
///   groups, including an unknown one, gets that same reason.
/// * Anything blastguard cannot read is Ask (fail-closed): unknown group or
///   subcommand, missing subcommand, an unresolved `$`/backtick token where the
///   group, subcommand, or `gh api` method is, an unknown `gh api` flag, an
///   unknown HTTP method, a bare `gh`. The `-R/--repo <owner/repo>` global flag
///   is skipped.
///
/// Writes to `.github/workflows/` are a file-write concern, handled by
/// [`protected_path_block`], not here.
pub(super) fn analyze_gh(rest: &[&str]) -> Decision {
    fn unreadable(tok: &str) -> bool {
        has_unresolvable_expansion(tok) || tok.contains(['(', ')', '*', '?', '[', '{'])
    }
    // Leading global flags: `-R/--repo <v>` and `--repo=<v>` are skipped;
    // `--version` / `--help` are read-only; anything else is unreadable.
    let mut i = 0;
    while i < rest.len() {
        let t = rest[i];
        if t == "-R" || t == "--repo" {
            if i + 1 >= rest.len() || unreadable(rest[i + 1]) {
                return Decision::ask("gh: -R/--repo value is missing or unreadable");
            }
            i += 2;
        } else if t.starts_with("--repo=") {
            i += 1;
        } else if t == "--version" || t == "--help" || t == "-h" {
            return Decision::Allow;
        } else if t.starts_with('-') {
            return Decision::ask(format!(
                "gh {t}: unrecognised global flag — blastguard cannot tell what this does"
            ));
        } else {
            break;
        }
    }
    let Some(group_tok) = rest.get(i) else {
        return Decision::ask("gh: no subcommand given — blastguard cannot tell what this does");
    };
    if unreadable(group_tok) {
        return Decision::ask(format!(
            "gh {group_tok}: the subcommand is not literal text — blastguard cannot tell what \
this would do"
        ));
    }
    let group = normalized_command(group_tok);
    let tail = &rest[i + 1..];
    if group == "api" {
        return analyze_gh_api(tail);
    }
    // Second word (the subcommand), skipping `-R/--repo <v>` and other flags.
    let mut sub_tok: Option<&str> = None;
    let mut j = 0;
    while j < tail.len() {
        let t = tail[j];
        if t == "-R" || t == "--repo" {
            j += 2;
        } else if t.starts_with('-') {
            j += 1;
        } else {
            sub_tok = Some(t);
            break;
        }
    }
    if let Some(t) = sub_tok {
        if unreadable(t) {
            return Decision::ask(format!(
                "gh {group} {t}: the subcommand is not literal text — blastguard cannot tell \
what this would do"
            ));
        }
    }
    let sub = sub_tok.map(normalized_command);
    let sub = sub.as_deref();
    let actions_ask = |what: &str| {
        Decision::ask(format!(
            "gh {what} executes or configures GitHub Actions — GitHub Actions execution and \
configuration need an explicit user request"
        ))
    };
    let changes = |what: &str| {
        Decision::ask(format!(
            "gh {what} changes remote GitHub state (or a state blastguard cannot read) — \
confirm before running"
        ))
    };
    const READ: &[&str] = &[
        "list",
        "view",
        "status",
        "diff",
        "checks",
        "watch",
        "download",
        "item-list",
        "field-list",
    ];
    match group.as_str() {
        "search" | "status" | "browse" | "help" => Decision::Allow,
        "auth" => match sub {
            Some("status") => Decision::Allow,
            Some(s) => changes(&format!("auth {s}")),
            None => changes("auth"),
        },
        "config" => match sub {
            Some("get" | "list") => Decision::Allow,
            Some(s) => changes(&format!("config {s}")),
            None => changes("config"),
        },
        "extension" => match sub {
            Some("list") => Decision::Allow,
            Some(s) => changes(&format!("extension {s}")),
            None => changes("extension"),
        },
        "pr" | "issue" | "run" | "workflow" | "release" | "repo" | "label" | "gist" | "cache"
        | "secret" | "variable" | "ruleset" | "project" | "codespace" | "org" => {
            let Some(s) = sub else {
                return changes(&group);
            };
            if READ.contains(&s) || (matches!(group.as_str(), "repo" | "gist") && s == "clone") {
                return Decision::Allow;
            }
            let what = format!("{group} {s}");
            match group.as_str() {
                "workflow" | "run" | "cache" | "secret" | "variable" => actions_ask(&what),
                _ => changes(&what),
            }
        }
        _ => changes(&group),
    }
}

/// `gh api`: GET (no method and no field flag, or an explicit `GET`) is Allow;
/// POST/PUT/PATCH/DELETE, or a field flag (`-f/-F/--field/--raw-field/--input`)
/// with no explicit GET (gh then defaults to POST), is Ask; a mutation of an
/// `/actions/` endpoint carries the GitHub-Actions reason. A missing endpoint,
/// an unreadable method, an unknown HTTP method or an unknown flag is Ask.
fn analyze_gh_api(args: &[&str]) -> Decision {
    let undeterminable = |why: &str| {
        Decision::ask(format!(
            "gh api: {why} — blastguard cannot tell whether this reads or changes GitHub"
        ))
    };
    let mut method: Option<String> = None;
    let mut fields = false;
    let mut endpoint: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        let t = args[i];
        if has_unresolvable_expansion(t) && (t.starts_with('-') || endpoint.is_some()) {
            return undeterminable("an argument is not literal text");
        }
        // (long, short, effect): effect 0 = method, 1 = field, 2 = ignored value.
        const VALUE_FLAGS: &[(&str, &str, u8)] = &[
            ("--method", "-X", 0),
            ("--field", "-F", 1),
            ("--raw-field", "-f", 1),
            ("--input", "", 1),
            ("--header", "-H", 2),
            ("--jq", "-q", 2),
            ("--template", "-t", 2),
            ("--hostname", "", 2),
            ("--cache", "", 2),
            ("--preview", "-p", 2),
        ];
        let mut matched = false;
        for &(long, short, effect) in VALUE_FLAGS {
            let (value, used): (Option<String>, usize) =
                if t == long || (!short.is_empty() && t == short) {
                    (args.get(i + 1).map(|v| (*v).to_string()), 2)
                } else if let Some(v) = t.strip_prefix(&format!("{long}=")) {
                    (Some(v.to_string()), 1)
                } else if !short.is_empty() && t.starts_with(short) && t.len() > short.len() {
                    (Some(t[short.len()..].to_string()), 1)
                } else {
                    continue;
                };
            let Some(value) = value else {
                return undeterminable(&format!("{t} has no value"));
            };
            match effect {
                0 => {
                    if has_unresolvable_expansion(&value) {
                        return undeterminable("the method is not literal text");
                    }
                    method = Some(value.trim_matches(['"', '\'']).to_uppercase());
                }
                1 => fields = true,
                _ => {}
            }
            i += used;
            matched = true;
            break;
        }
        if matched {
            continue;
        }
        if matches!(
            t,
            "--paginate" | "--slurp" | "-i" | "--include" | "--silent" | "--verbose"
        ) {
            i += 1;
            continue;
        }
        if t.starts_with('-') {
            return undeterminable(&format!("unrecognised flag {t}"));
        }
        if endpoint.is_none() {
            endpoint = Some(t);
        }
        i += 1;
    }
    let Some(endpoint) = endpoint else {
        return undeterminable("no endpoint given");
    };
    let effective = match (&method, fields) {
        (Some(m), _) => m.as_str(),
        (None, true) => "POST",
        (None, false) => "GET",
    };
    match effective {
        "GET" => Decision::Allow,
        "POST" | "PUT" | "PATCH" | "DELETE" => {
            if endpoint.contains("/actions/") || endpoint.ends_with("/actions") {
                Decision::ask(format!(
                    "gh api {effective} {endpoint} executes or configures GitHub Actions — \
GitHub Actions execution and configuration need an explicit user request"
                ))
            } else {
                Decision::ask(format!(
                    "gh api {effective} {endpoint} changes remote GitHub state (a field flag \
without an explicit GET makes gh api POST)"
                ))
            }
        }
        other => undeterminable(&format!("unknown HTTP method {other}")),
    }
}
