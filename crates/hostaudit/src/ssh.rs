//! The ssh whitelist W1–W11 (canon §3.2, CONTRACT §4.4) as a CLOSED enum.
//!
//! There is no API here that takes a command string: the only remote commands
//! hostaudit can ever send are the twelve `&'static str` literals returned by
//! [`Remote::command`]. Adding one means adding a variant — a reviewed change.

use crate::config::Ctx;
use crate::inventory::Host;
use harness_core::verdict::Required;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
}

impl Remote {
    pub const ALL: [Remote; 12] = [
        Remote::OsRelease,
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

    /// The exact remote command string (byte-for-byte CONTRACT §4.4).
    pub fn command(self) -> &'static str {
        match self {
            Remote::OsRelease => "LC_ALL=C cat /etc/os-release",
            Remote::DpkgList => {
                r"LC_ALL=C dpkg-query -W -f '${Package}\t${Version}\t${source:Package}\t${source:Version}\n'"
            }
            Remote::Listen => "LC_ALL=C ss -H -tln",
            Remote::SshdConfig => "sudo -n LC_ALL=C sshd -T",
            Remote::Ufw => "sudo -n LC_ALL=C ufw status",
            Remote::Nft => "sudo -n LC_ALL=C nft list ruleset",
            Remote::AptStamp => "LC_ALL=C stat -c %Y /var/lib/apt/periodic/update-success-stamp",
            Remote::AptLists => "LC_ALL=C stat -c %Y /var/lib/apt/lists",
            Remote::RebootRequired => "LC_ALL=C stat -c %Y /var/run/reboot-required",
            Remote::UnattendedUnit => "LC_ALL=C systemctl is-enabled unattended-upgrades",
            Remote::AutoUpgrades => "LC_ALL=C cat /etc/apt/apt.conf.d/20auto-upgrades",
            Remote::Date => "LC_ALL=C date +%s",
        }
    }

    pub fn id(self) -> &'static str {
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
        }
    }

    /// W7a/W7b/W8/W10: "No such file" is an observation ("absent").
    fn absent_is_observation(self) -> bool {
        matches!(
            self,
            Remote::AptStamp | Remote::AptLists | Remote::RebootRequired | Remote::AutoUpgrades
        )
    }
}

/// Outcome of one whitelist invocation (CONTRACT §4.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Obs {
    /// exit 0, stdout.
    Out(String),
    /// the file does not exist (W7a/W7b/W8/W10 only).
    Absent,
    /// W9 unit state read from stdout regardless of exit code.
    Unit(String),
    /// could not observe; carries why.
    Undet(String),
}

/// Classify a finished invocation.
pub fn classify(r: Remote, code: i32, stdout: &str, stderr: &str) -> Obs {
    let tail: String = stderr.trim().chars().take(300).collect();
    if code == 255 {
        return Obs::Undet(format!(
            "{} ssh failed (exit 255): {}",
            r.id(),
            if tail.is_empty() {
                "<no stderr>"
            } else {
                &tail
            }
        ));
    }
    if r == Remote::UnattendedUnit {
        let first = stdout.lines().next().map(str::trim).unwrap_or("");
        if matches!(first, "disabled" | "masked" | "not-found") {
            return Obs::Unit(first.to_string());
        }
    }
    if code == 0 {
        return Obs::Out(stdout.to_string());
    }
    if r.absent_is_observation() && code == 1 && stderr.contains("No such file or directory") {
        return Obs::Absent;
    }
    Obs::Undet(format!(
        "{} `{}` exited {code}: {}",
        r.id(),
        r.command(),
        if tail.is_empty() {
            "<no stderr>"
        } else {
            &tail
        }
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
pub fn run(ctx: &Ctx, host: &Host, r: Remote) -> Obs {
    let dest = host.destination();
    if !dest_ok(dest) {
        return Obs::Undet(format!(
            "{}: destination {dest:?} is not a plain host name; refusing to pass it to ssh",
            r.id()
        ));
    }
    let mut c = std::process::Command::new(&ctx.ssh_bin);
    c.arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg(format!("ConnectTimeout={}", ctx.ssh_connect_timeout))
        .arg(dest)
        .arg(r.command())
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null());
    // Bound the whole call, not only the connect: a remote command that hangs
    // must not hang the audit. Timeout → undetermined.
    let limit = Duration::from_secs(ctx.ssh_connect_timeout as u64 + 120);
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
        Required::Blocked(why) => Obs::Undet(format!("{}: {why}", r.id())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(
            classify(Remote::AptStamp, 1, "", "stat: No such file or directory"),
            Obs::Absent
        );
        assert!(matches!(
            classify(Remote::Date, 1, "", "No such file or directory"),
            Obs::Undet(_)
        ));
        assert_eq!(
            classify(Remote::UnattendedUnit, 1, "masked\n", ""),
            Obs::Unit("masked".into())
        );
        assert!(matches!(
            classify(Remote::UnattendedUnit, 255, "", ""),
            Obs::Undet(_)
        ));
        assert!(matches!(
            classify(Remote::SshdConfig, 1, "", "sudo: a password is required"),
            Obs::Undet(_)
        ));
    }
}
