//! Audit targets (canon §3.1, CONTRACT §4.1–4.2): `~/.ssh/config` (Include
//! followed), `--from-cmd`, EC2 (`--aws`), and the user's `selection.toml`.
//!
//! An inventory that could not be read is `Err` — never an empty host list
//! (an empty list would read downstream as "nothing to audit").

use crate::config::Ctx;
use harness_core::verdict::Required;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    SshConfig,
    FromCmd,
    Ec2,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Host {
    pub alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_jump: Option<String>,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Host {
    pub fn simple(alias: &str, source: Source) -> Host {
        Host {
            alias: alias.to_string(),
            hostname: None,
            user: None,
            port: None,
            proxy_jump: None,
            source,
            instance_id: None,
            region: None,
            public_ip: None,
            private_ip: None,
            name: None,
        }
    }

    /// The ssh destination (CONTRACT §4.3): the alias for ssh_config hosts (so
    /// ssh resolves the rest from the user's config), the hostname otherwise.
    pub fn destination(&self) -> &str {
        match self.source {
            Source::SshConfig => &self.alias,
            Source::FromCmd | Source::Ec2 => self.hostname.as_deref().unwrap_or(&self.alias),
        }
    }

    /// The name the B9 TLS probe connects to.
    pub fn tls_name(&self) -> &str {
        self.hostname.as_deref().unwrap_or(&self.alias)
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "alias": self.alias,
            "hostname": self.hostname,
            "user": self.user,
            "port": self.port,
            "proxy_jump": self.proxy_jump,
            "source": self.source,
        });
        if self.source == Source::Ec2 {
            if let Some(o) = v.as_object_mut() {
                o.insert("instance_id".into(), serde_json::json!(self.instance_id));
                o.insert("region".into(), serde_json::json!(self.region));
                o.insert("public_ip".into(), serde_json::json!(self.public_ip));
                o.insert("private_ip".into(), serde_json::json!(self.private_ip));
                o.insert("name".into(), serde_json::json!(self.name));
            }
        }
        v
    }
}

pub struct Inventory {
    pub ssh_config_present: bool,
    pub hosts: Vec<Host>,
}

// ───────────────────────────── ssh config ──────────────────────────────────

const MAX_INCLUDE_DEPTH: usize = 16;

struct Parser {
    home: PathBuf,
    ssh_dir: PathBuf,
    order: Vec<String>,
    hosts: BTreeMap<String, Host>,
    current: Vec<String>,
}

fn split_kv(line: &str) -> Option<(String, String)> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return None;
    }
    let idx = t
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(t.len());
    let key = t[..idx].to_ascii_lowercase();
    let mut rest = t[idx..].trim_start();
    if let Some(r) = rest.strip_prefix('=') {
        rest = r.trim_start();
    }
    Some((key, rest.trim().to_string()))
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn tokens(s: &str) -> Vec<String> {
    // Minimal quote-aware split: "a b" stays one token.
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut inq = false;
    for c in s.chars() {
        match c {
            '"' => inq = !inq,
            c if c.is_whitespace() && !inq => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn is_pattern(tok: &str) -> bool {
    tok.contains('*') || tok.contains('?') || tok.starts_with('!')
}

/// Shell-style wildcard match (`*`, `?`) of one path component.
pub fn wildcard(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Expand a path with `*` / `?` components. Unlistable directories are `Err`.
fn glob(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut cur: Vec<PathBuf> = vec![PathBuf::new()];
    for comp in path.components() {
        let s = comp.as_os_str().to_string_lossy().into_owned();
        if !(s.contains('*') || s.contains('?')) {
            for c in cur.iter_mut() {
                c.push(comp.as_os_str());
            }
            continue;
        }
        let mut next = Vec::new();
        for dir in &cur {
            let listing = match harness_core::boundary::read_dir_entries_checked(dir).require() {
                Required::Determined(Some(v)) => v,
                Required::Determined(None) => continue,
                Required::Blocked(why) => return Err(why.to_string()),
            };
            for p in listing {
                let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                    continue;
                };
                if name.starts_with('.') && !s.starts_with('.') {
                    continue;
                }
                if wildcard(&s, &name) {
                    next.push(p);
                }
            }
        }
        next.sort();
        cur = next;
    }
    Ok(cur)
}

impl Parser {
    fn resolve_include(&self, tok: &str) -> PathBuf {
        let expanded = if let Some(rest) = tok.strip_prefix("~/") {
            self.home.join(rest)
        } else {
            PathBuf::from(tok)
        };
        if expanded.is_absolute() {
            expanded
        } else {
            self.ssh_dir.join(expanded)
        }
    }

    fn set_field(&mut self, key: &str, value: &str) {
        for alias in self.current.clone() {
            let Some(h) = self.hosts.get_mut(&alias) else {
                continue;
            };
            let slot = match key {
                "hostname" => &mut h.hostname,
                "user" => &mut h.user,
                "proxyjump" => &mut h.proxy_jump,
                _ => continue,
            };
            if slot.is_none() {
                *slot = Some(value.to_string());
            }
        }
    }

    fn parse_text(&mut self, text: &str, file: &Path, depth: usize) -> Result<(), String> {
        for (lineno, line) in text.lines().enumerate() {
            let Some((key, value)) = split_kv(line) else {
                continue;
            };
            let at = || format!("{}:{}", file.display(), lineno + 1);
            match key.as_str() {
                "host" => {
                    let mut block = Vec::new();
                    for tok in tokens(&value) {
                        if is_pattern(&tok) {
                            continue;
                        }
                        if !self.hosts.contains_key(&tok) {
                            self.order.push(tok.clone());
                            self.hosts
                                .insert(tok.clone(), Host::simple(&tok, Source::SshConfig));
                        }
                        block.push(tok);
                    }
                    self.current = block;
                }
                "match" => self.current.clear(),
                "include" => {
                    if depth >= MAX_INCLUDE_DEPTH {
                        return Err(format!("{}: Include nested too deeply", at()));
                    }
                    for tok in tokens(&value) {
                        let p = self.resolve_include(&tok);
                        let files = if tok.contains('*') || tok.contains('?') {
                            glob(&p).map_err(|e| format!("{}: Include {tok}: {e}", at()))?
                        } else {
                            vec![p]
                        };
                        for f in files {
                            match harness_core::boundary::read_to_string(&f).require() {
                                Required::Determined(Some(t)) => {
                                    // An included file's Host blocks are its own;
                                    // the including block resumes afterwards.
                                    let saved = std::mem::take(&mut self.current);
                                    self.parse_text(&t, &f, depth + 1)?;
                                    self.current = saved;
                                }
                                Required::Determined(None) => {}
                                Required::Blocked(why) => {
                                    return Err(format!("{}: Include {}: {why}", at(), f.display()))
                                }
                            }
                        }
                    }
                }
                "port" => {
                    let v = unquote(&value);
                    let port = match v.parse::<u32>() {
                        Ok(p) if (1..=65535).contains(&p) => p as u16,
                        _ => {
                            return Err(format!("{}: Port {v:?} is not an integer 1..=65535", at()))
                        }
                    };
                    for alias in self.current.clone() {
                        if let Some(h) = self.hosts.get_mut(&alias) {
                            if h.port.is_none() {
                                h.port = Some(port);
                            }
                        }
                    }
                }
                "hostname" | "user" | "proxyjump" => {
                    let v = unquote(&value);
                    self.set_field(&key, &v);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// `Ok(None)` = `~/.ssh/config` absent; `Err` = unreadable or unparseable.
pub fn read_ssh_config(home: &Path) -> Result<Option<Vec<Host>>, String> {
    let ssh_dir = home.join(".ssh");
    let path = ssh_dir.join("config");
    let text = match harness_core::boundary::read_to_string(&path).require() {
        Required::Determined(None) => return Ok(None),
        Required::Determined(Some(t)) => t,
        Required::Blocked(why) => return Err(why.to_string()),
    };
    let mut p = Parser {
        home: home.to_path_buf(),
        ssh_dir,
        order: Vec::new(),
        hosts: BTreeMap::new(),
        current: Vec::new(),
    };
    p.parse_text(&text, &path, 0)?;
    let mut out = Vec::new();
    for a in &p.order {
        if let Some(h) = p.hosts.remove(a) {
            out.push(h);
        }
    }
    Ok(Some(out))
}

/// `--from-cmd`: `/bin/sh -c CMD`; only exit 0 counts.
pub fn from_cmd(cmd: &str) -> Result<Vec<Host>, String> {
    let mut c = std::process::Command::new("/bin/sh");
    c.arg("-c").arg(cmd).stdin(std::process::Stdio::null());
    let out = match harness_core::boundary::run_with_timeout(&mut c, Duration::from_secs(300))
        .require()
    {
        Required::Determined(o) => o,
        Required::Blocked(why) => return Err(why.to_string()),
    };
    let stdout = match out.stdout_on_success().require() {
        Required::Determined(s) => s,
        Required::Blocked(why) => return Err(format!("--from-cmd did not succeed: {why}")),
    };
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut h = Host::simple(l, Source::FromCmd);
            h.hostname = Some(l.to_string());
            h
        })
        .collect())
}

/// Build the merged inventory (ssh config, `--from-cmd`, `--aws`).
pub fn build(ctx: &Ctx, from_cmd_arg: Option<&str>, with_aws: bool) -> Result<Inventory, String> {
    let ssh = read_ssh_config(&ctx.home).map_err(|e| format!("~/.ssh/config: {e}"))?;
    let ssh_config_present = ssh.is_some();
    // `None` is the OBSERVED absence of ~/.ssh/config (reported as
    // "ssh_config": "absent"); an unreadable config already returned `Err`.
    let mut hosts: Vec<Host> = Vec::new();
    if let Some(h) = ssh {
        hosts.extend(h);
    }
    if let Some(c) = from_cmd_arg {
        hosts.extend(from_cmd(c)?);
    }
    if with_aws {
        hosts.extend(crate::aws::ec2_hosts(ctx)?);
    }
    let mut seen = std::collections::BTreeSet::new();
    hosts.retain(|h| seen.insert(h.alias.clone()));
    Ok(Inventory {
        ssh_config_present,
        hosts,
    })
}

// ───────────────────────────── selection ───────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Selection {
    #[serde(default)]
    pub host: Vec<Host>,
}

pub fn selection_path(ctx: &Ctx) -> PathBuf {
    ctx.state_dir().join("selection.toml")
}

/// `Ok(None)` = no selection file.
pub fn read_selection(ctx: &Ctx) -> Result<Option<Selection>, String> {
    let p = selection_path(ctx);
    match harness_core::boundary::read_to_string(&p).require() {
        Required::Determined(None) => Ok(None),
        Required::Determined(Some(t)) => toml::from_str::<Selection>(&t)
            .map(Some)
            .map_err(|e| format!("{} is not a valid selection: {e}", p.display())),
        Required::Blocked(why) => Err(why.to_string()),
    }
}

pub fn write_selection(ctx: &Ctx, sel: &Selection) -> Result<(), String> {
    let dir = ctx.state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let body = toml::to_string(sel).map_err(|e| format!("cannot serialize selection: {e}"))?;
    let p = selection_path(ctx);
    let tmp = dir.join(format!(".selection.toml.{}", std::process::id()));
    std::fs::write(&tmp, body).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("cannot replace {}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matches() {
        assert!(wildcard("*", "extra"));
        assert!(wildcard("web?", "web1"));
        assert!(!wildcard("web?", "web12"));
        assert!(wildcard("a*b*c", "aXXbYc"));
        assert!(!wildcard("a*b", "aXc"));
    }

    #[test]
    fn kv_forms() {
        assert_eq!(
            split_kv("  HostName=host.example"),
            Some(("hostname".into(), "host.example".into()))
        );
        assert_eq!(split_kv("Port 22"), Some(("port".into(), "22".into())));
        assert_eq!(split_kv("# c"), None);
    }
}
