//! The ssh whitelist (canon v1 §3.2 W1–W11, v1.1 §2 W0/W12–W25/WN1–WN5;
//! CONTRACT §4.4, §12.3) as a CLOSED enum.
//!
//! There is no API here that takes a free command string. Static commands
//! are `&'static str` literals; the four dynamic ones (W12b pid, W13 / W22
//! executable path, W18 source-file name) carry validated newtypes
//! ([`Pid`], [`SafePath`], [`SourceName`]) whose only constructors reject
//! anything outside a strict character allowlist — so data read from a
//! server can never become shell syntax on that server.

use crate::config::Ctx;
use crate::inventory::Host;
use crate::products;
use harness_core::verdict::Required;
use std::time::{Duration, Instant};

/// A process id from W12: digits only (canon v1.1 §2 W12b).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pid(u32);

impl Pid {
    /// `None` unless `s` is a non-empty run of ASCII digits that fits u32.
    pub fn parse(s: &str) -> Option<Pid> {
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        s.parse::<u32>().ok().map(Pid)
    }
}

/// An absolute executable path that is safe to place, unquoted, into a remote
/// command (canon v1.1 §2 W13: starts with `/`, no whitespace / quotes /
/// `;|&$` / backtick). Stricter than the canon on purpose: only
/// `[A-Za-z0-9/._+@:,=-]` is accepted.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SafePath(String);

impl SafePath {
    pub fn parse(s: &str) -> Option<SafePath> {
        let ok = s.starts_with('/')
            && s.len() <= 4096
            && s.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, '/' | '.' | '_' | '+' | '@' | ':' | ',' | '=' | '-')
            });
        ok.then(|| SafePath(s.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Last path segment.
    pub fn basename(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }
    /// The usrmerge alias (CONTRACT §12.10 item 12): `/usr/bin/x` → `/bin/x`,
    /// `/usr/sbin/x` → `/sbin/x`, `/usr/lib/x` → `/lib/x`; `None` otherwise.
    pub fn usrmerge_alias(&self) -> Option<SafePath> {
        ["/usr/bin/", "/usr/sbin/", "/usr/lib/"]
            .iter()
            .find(|p| self.0.starts_with(**p) && self.0.len() > p.len())
            .map(|_| SafePath(self.0["/usr".len()..].to_string()))
    }
}

/// A file name listed in `/etc/apt/sources.list.d` that may be `cat`ed
/// (canon v1.1 §2 W18: `[A-Za-z0-9._-]+`; CONTRACT §12.3: ending in `.list`
/// or `.sources`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceName(String);

impl SourceName {
    pub fn parse(s: &str) -> Option<SourceName> {
        let ok = !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && (s.ends_with(".list") || s.ends_with(".sources"))
            && !s.starts_with('.');
        ok.then(|| SourceName(s.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The five fixed read-only PowerShell commands (canon v1.1 §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Win {
    Os,
    HotFix,
    RebootRequired,
    Firewall,
    Nla,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Remote {
    /// W1
    OsRelease,
    /// W2
    DpkgList,
    /// W3
    Listen,
    /// W4 [sudo]
    SshdConfig,
    /// W5 [sudo]
    Ufw,
    /// W6 [sudo]
    Nft,
    /// W7a
    AptStamp,
    /// W7b
    AptLists,
    /// W8
    RebootRequired,
    /// W9
    UnattendedUnit,
    /// W10
    AutoUpgrades,
    /// W11
    Date,
    /// W0 (Windows probe; only after W1 failed)
    WinVer,
    /// W12 [sudo]
    ListenProcs,
    /// W12b [sudo]
    Exe(Pid),
    /// W13
    DpkgOwner(SafePath),
    /// W14
    AptSim,
    /// W15 (Ubuntu)
    ProSecurity,
    /// W16 [sudo]
    NeedRestart,
    /// W17 (Ubuntu)
    DistroInfoUbuntu,
    /// W17 (Debian)
    DistroInfoDebian,
    /// W18 `/etc/apt/sources.list`
    SourcesList,
    /// W18 `ls /etc/apt/sources.list.d`
    SourcesDir,
    /// W18 one file of `sources.list.d`
    SourcesFile(SourceName),
    /// W19
    DpkgAudit,
    /// W20
    Holds,
    /// W21: index into [`products::W21`]
    ProductVersion(usize),
    /// W22: a non-dpkg listening executable + the W21 args of its product
    ExeVersion(SafePath, &'static str),
    /// W23 [sudo]
    DockerImages,
    /// W24
    Uname,
    /// W25 (Ubuntu)
    ProStatus,
    /// WN1–WN5
    Win(Win),
}

/// How the non-zero exits of one command are read (CONTRACT §4.4, §12.3).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    /// exit 1 + `No such file or directory` → absent.
    AbsentFile,
    /// W9: `disabled` / `masked` / `not-found` on stdout, any exit.
    Unit,
    /// stderr `command not found` (any exit) → tool missing.
    Tool,
    /// W21/W22: exit 127 / `command not found` / `No such file` → product
    /// absent, unless stderr has `error while loading shared libraries`
    /// (then undetermined);
    /// stdout and stderr are both the observation.
    Product,
    /// W13: exit 1 + `no path found` → not managed by dpkg.
    Owner,
}

impl Remote {
    /// The v1 commands W2–W11 (W1 is run first, separately, to dispatch the OS).
    pub const V1_AFTER_OS: [Remote; 11] = [
        Remote::DpkgList,
        Remote::Listen,
        Remote::SshdConfig,
        Remote::Ufw,
        Remote::Nft,
        Remote::AptStamp,
        Remote::AptLists,
        Remote::RebootRequired,
        Remote::UnattendedUnit,
        Remote::AutoUpgrades,
        Remote::Date,
    ];

    pub const WIN: [Win; 5] = [
        Win::Os,
        Win::HotFix,
        Win::RebootRequired,
        Win::Firewall,
        Win::Nla,
    ];

    /// The exact remote command string (byte-for-byte CONTRACT §4.4 / §12.3).
    pub fn command(&self) -> String {
        match self {
            Remote::OsRelease => "LC_ALL=C cat /etc/os-release".into(),
            Remote::DpkgList => {
                r"LC_ALL=C dpkg-query -W -f '${Package}\t${Version}\t${source:Package}\t${source:Version}\n'".into()
            }
            Remote::Listen => "LC_ALL=C ss -H -tln".into(),
            Remote::SshdConfig => "sudo -n LC_ALL=C sshd -T".into(),
            Remote::Ufw => "sudo -n LC_ALL=C ufw status".into(),
            Remote::Nft => "sudo -n LC_ALL=C nft list ruleset".into(),
            Remote::AptStamp => {
                "LC_ALL=C stat -c %Y /var/lib/apt/periodic/update-success-stamp".into()
            }
            Remote::AptLists => "LC_ALL=C stat -c %Y /var/lib/apt/lists".into(),
            Remote::RebootRequired => "LC_ALL=C stat -c %Y /var/run/reboot-required".into(),
            Remote::UnattendedUnit => "LC_ALL=C systemctl is-enabled unattended-upgrades".into(),
            Remote::AutoUpgrades => "LC_ALL=C cat /etc/apt/apt.conf.d/20auto-upgrades".into(),
            Remote::Date => "LC_ALL=C date +%s".into(),
            Remote::WinVer => "cmd /c ver".into(),
            Remote::ListenProcs => "sudo -n LC_ALL=C ss -H -tlnp".into(),
            Remote::Exe(p) => format!("sudo -n LC_ALL=C readlink /proc/{}/exe", p.0),
            Remote::DpkgOwner(p) => format!("LC_ALL=C dpkg -S {}", p.0),
            Remote::AptSim => "LC_ALL=C apt-get -s -o Debug::NoLocking=1 dist-upgrade".into(),
            Remote::ProSecurity => "LC_ALL=C pro security-status --format json".into(),
            Remote::NeedRestart => "sudo -n LC_ALL=C needrestart -b -r l".into(),
            Remote::DistroInfoUbuntu => "LC_ALL=C ubuntu-distro-info --supported".into(),
            Remote::DistroInfoDebian => "LC_ALL=C debian-distro-info --supported".into(),
            Remote::SourcesList => "LC_ALL=C cat /etc/apt/sources.list".into(),
            Remote::SourcesDir => "LC_ALL=C ls /etc/apt/sources.list.d".into(),
            Remote::SourcesFile(n) => format!("LC_ALL=C cat /etc/apt/sources.list.d/{}", n.0),
            Remote::DpkgAudit => "LC_ALL=C dpkg --audit".into(),
            Remote::Holds => "LC_ALL=C apt-mark showhold".into(),
            Remote::ProductVersion(i) => match products::W21.get(*i) {
                Some((_, cmd)) => format!("LC_ALL=C {cmd}"),
                // Unreachable: indices come from iterating W21 itself. A
                // command that cannot be named is a command that is not sent.
                None => String::new(),
            },
            Remote::ExeVersion(p, args) => format!("LC_ALL=C {} {args}", p.0),
            Remote::DockerImages => "sudo -n LC_ALL=C docker ps --format '{{.Image}}'".into(),
            Remote::Uname => "LC_ALL=C uname -r".into(),
            Remote::ProStatus => "LC_ALL=C pro status --format json".into(),
            Remote::Win(w) => format!(
                "powershell -NoProfile -NonInteractive -Command \"{}\"",
                match w {
                    Win::Os => "Get-CimInstance Win32_OperatingSystem | Select-Object Caption,Version,BuildNumber | ConvertTo-Json",
                    Win::HotFix => "Get-HotFix | Sort-Object InstalledOn -Descending | Select-Object -First 1 HotFixID,InstalledOn | ConvertTo-Json",
                    Win::RebootRequired => r"Test-Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired'",
                    Win::Firewall => "Get-NetFirewallProfile | Select-Object Name,Enabled | ConvertTo-Json",
                    Win::Nla => r"Get-ItemProperty 'HKLM:\System\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp' -Name UserAuthentication | ConvertTo-Json",
                }
            ),
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            Remote::OsRelease => "W1",
            Remote::DpkgList => "W2",
            Remote::Listen => "W3",
            Remote::SshdConfig => "W4",
            Remote::Ufw => "W5",
            Remote::Nft => "W6",
            Remote::AptStamp => "W7a",
            Remote::AptLists => "W7b",
            Remote::RebootRequired => "W8",
            Remote::UnattendedUnit => "W9",
            Remote::AutoUpgrades => "W10",
            Remote::Date => "W11",
            Remote::WinVer => "W0",
            Remote::ListenProcs => "W12",
            Remote::Exe(_) => "W12b",
            Remote::DpkgOwner(_) => "W13",
            Remote::AptSim => "W14",
            Remote::ProSecurity => "W15",
            Remote::NeedRestart => "W16",
            Remote::DistroInfoUbuntu | Remote::DistroInfoDebian => "W17",
            Remote::SourcesList | Remote::SourcesDir | Remote::SourcesFile(_) => "W18",
            Remote::DpkgAudit => "W19",
            Remote::Holds => "W20",
            Remote::ProductVersion(_) => "W21",
            Remote::ExeVersion(..) => "W22",
            Remote::DockerImages => "W23",
            Remote::Uname => "W24",
            Remote::ProStatus => "W25",
            Remote::Win(Win::Os) => "WN1",
            Remote::Win(Win::HotFix) => "WN2",
            Remote::Win(Win::RebootRequired) => "WN3",
            Remote::Win(Win::Firewall) => "WN4",
            Remote::Win(Win::Nla) => "WN5",
        }
    }

    fn kind(&self) -> Kind {
        match self {
            Remote::AptStamp
            | Remote::AptLists
            | Remote::RebootRequired
            | Remote::AutoUpgrades
            | Remote::SourcesList => Kind::AbsentFile,
            Remote::UnattendedUnit => Kind::Unit,
            Remote::ProSecurity
            | Remote::NeedRestart
            | Remote::DistroInfoUbuntu
            | Remote::DistroInfoDebian
            | Remote::DockerImages
            | Remote::ProStatus => Kind::Tool,
            Remote::ProductVersion(_) | Remote::ExeVersion(..) => Kind::Product,
            Remote::DpkgOwner(_) => Kind::Owner,
            _ => Kind::Plain,
        }
    }
}

/// Outcome of one whitelist invocation (CONTRACT §4.4, §12.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Obs {
    /// exit 0, stdout (W21/W22: stdout followed by stderr).
    Out(String),
    /// the file / product does not exist, or (W13) the path is not managed by dpkg.
    Absent,
    /// W9 unit state read from stdout regardless of exit code.
    Unit(String),
    /// the tool is not installed (`command not found`); carries the stderr tail.
    Missing(String),
    /// could not observe; carries why.
    Undet(String),
}

fn tail_of(stderr: &str) -> String {
    let t: String = stderr.trim().chars().take(300).collect();
    if t.is_empty() {
        "<no stderr>".to_string()
    } else {
        t
    }
}

/// Classify a finished invocation.
pub fn classify(r: &Remote, code: i32, stdout: &str, stderr: &str) -> Obs {
    if code == 255 {
        return Obs::Undet(format!(
            "{} ssh failed (exit 255): {}",
            r.id(),
            tail_of(stderr)
        ));
    }
    let kind = r.kind();
    if kind == Kind::Unit {
        let first = stdout.lines().next().map(str::trim).unwrap_or("");
        if matches!(first, "disabled" | "masked" | "not-found") {
            return Obs::Unit(first.to_string());
        }
    }
    if code == 0 {
        if kind == Kind::Product {
            return Obs::Out(format!("{stdout}\n{stderr}"));
        }
        return Obs::Out(stdout.to_string());
    }
    match kind {
        Kind::AbsentFile if code == 1 && stderr.contains("No such file or directory") => {
            return Obs::Absent
        }
        Kind::Tool if stderr.contains("command not found") => return Obs::Missing(tail_of(stderr)),
        // canon v1.1 W21 (exit 127 / `command not found` / `No such file`),
        // except a dynamic loader error (CONTRACT §12.10 item 8): that binary
        // exists but cannot run, so it is undetermined, not absent.
        Kind::Product
            if (code == 127
                || stderr.contains("command not found")
                || stderr.contains("No such file"))
                && !stderr.contains("error while loading shared libraries") =>
        {
            return Obs::Absent
        }
        Kind::Owner if code == 1 && stderr.contains("no path found") => return Obs::Absent,
        _ => {}
    }
    Obs::Undet(format!(
        "{} `{}` exited {code}: {}",
        r.id(),
        r.command(),
        tail_of(stderr)
    ))
}

fn dest_ok(d: &str) -> bool {
    !d.is_empty()
        && !d.starts_with('-')
        && !d
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\'' || c == '"')
}

/// Run one whitelist command on `host` via ssh.
///
/// The whole ssh process is bounded by `ConnectTimeout +
/// remote_command_timeout_secs` (canon v1.1 §2: ConnectTimeout bounds only
/// the connection; the command itself gets `remote_command_timeout_secs`).
/// Hitting the bound kills the process tree and yields `Undet` with
/// `時間切れ`.
pub fn run(ctx: &Ctx, host: &Host, r: &Remote) -> Obs {
    let dest = host.destination();
    if !dest_ok(dest) {
        return Obs::Undet(format!(
            "{}: destination {dest:?} is not a plain host name; refusing to pass it to ssh",
            r.id()
        ));
    }
    let cmd = r.command();
    if cmd.is_empty() {
        return Obs::Undet(format!("{}: no command defined", r.id()));
    }
    let mut c = std::process::Command::new(&ctx.ssh_bin);
    c.arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg(format!("ConnectTimeout={}", ctx.ssh_connect_timeout))
        .arg(dest)
        .arg(&cmd)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null());
    let limit = Duration::from_secs((ctx.ssh_connect_timeout + ctx.remote_command_timeout) as u64);
    let started = Instant::now();
    match harness_core::boundary::run_with_timeout(&mut c, limit).require() {
        Required::Determined(out) => {
            let code = out.code();
            let stderr = out.stderr().to_string();
            // stdout is read through `stdout_allowing(&[code])`: the exit code
            // is judged by `classify` right below, on the same value.
            match out.stdout_allowing(&[code]).require() {
                Required::Determined(stdout) => classify(r, code, &stdout, &stderr),
                Required::Blocked(why) => Obs::Undet(format!("{}: {why}", r.id())),
            }
        }
        Required::Blocked(why) => {
            if started.elapsed() >= limit {
                Obs::Undet(format!(
                    "{} 時間切れ: `{cmd}` did not finish within {}s (ConnectTimeout {}s + remote_command_timeout_secs {}s): {why}",
                    r.id(),
                    limit.as_secs(),
                    ctx.ssh_connect_timeout,
                    ctx.remote_command_timeout
                ))
            } else {
                Obs::Undet(format!("{}: {why}", r.id()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(
            classify(&Remote::AptStamp, 1, "", "stat: No such file or directory"),
            Obs::Absent
        );
        assert!(matches!(
            classify(&Remote::Date, 1, "", "No such file or directory"),
            Obs::Undet(_)
        ));
        assert_eq!(
            classify(&Remote::UnattendedUnit, 1, "masked\n", ""),
            Obs::Unit("masked".into())
        );
        assert!(matches!(
            classify(&Remote::UnattendedUnit, 255, "", ""),
            Obs::Undet(_)
        ));
        assert!(matches!(
            classify(&Remote::SshdConfig, 1, "", "sudo: a password is required"),
            Obs::Undet(_)
        ));
        assert!(matches!(
            classify(
                &Remote::NeedRestart,
                1,
                "",
                "sudo: needrestart: command not found"
            ),
            Obs::Missing(_)
        ));
        assert!(matches!(
            classify(
                &Remote::ProSecurity,
                255,
                "",
                "bash: pro: command not found"
            ),
            Obs::Undet(_)
        ));
    }

    #[test]
    fn dynamic_arguments_are_allowlisted() {
        assert!(Pid::parse("812").is_some());
        for bad in ["", "$(id)", "7;reboot", "-1", "12a"] {
            assert!(Pid::parse(bad).is_none(), "{bad}");
        }
        assert!(SafePath::parse("/opt/good/bin/node").is_some());
        for bad in [
            "relative/node",
            "/tmp/a b/node",
            "/tmp/'q'/node",
            "/tmp/a|b",
            "/tmp/a&b",
            "/tmp/a;b",
            "/tmp/$HOME",
            "/tmp/`id`",
            "/usr/bin/foo (deleted)",
        ] {
            assert!(SafePath::parse(bad).is_none(), "{bad}");
        }
        assert!(SourceName::parse("ok.list").is_some());
        assert!(SourceName::parse("x.sources").is_some());
        for bad in [
            "bad;name.list",
            "$(id).list",
            "foo.list.save",
            "README",
            ".list",
        ] {
            assert!(SourceName::parse(bad).is_none(), "{bad}");
        }
    }
}
