//! `sitecraft serve start|stop|status` — the dev-server manager.
//!
//! `start` spawns the trusted `sitecraft.toml` `command` in its own process
//! group, after checking the port is free, and waits (with a deadline) for an
//! HTTP 2xx/3xx response on `http://127.0.0.1:<port><ready_path>` or, when
//! the server listens on the IPv6 loopback only (a `localhost` bind that
//! resolved to `::1`), `http://[::1]:<port><ready_path>`; the address that
//! answered is the recorded `url`. Its state is a JSON
//! file under `<state>/serve/`, keyed by the canonical project root. `stop`
//! terminates the whole process group (SIGTERM, grace, SIGKILL, wait).
//!
//! Every failure — untrusted config, port taken, early exit, ready deadline
//! missed, corrupt state file — is `Undetermined` (exit 2). Nothing here
//! maps an error to "running" or "stopped". Every undetermined message says
//! how to fix it.
//!
//! Limitation: the state file records only the process-group id. If the
//! recorded group died without `stop` and the OS reused its id for an
//! unrelated group of the same user, `stop` cannot tell the two apart.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{self, Loaded};
use crate::trust::{self, Trust};
use crate::Determination;

/// Sub-directory of the state dir holding the dev-server state files.
const SERVE_DIR: &str = "serve";
/// Time the group gets between SIGTERM and SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(3);
/// Time the group gets to disappear after SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(3);
/// Pause between readiness probes.
const PROBE_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound of one readiness probe's connect / IO timeout.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Persisted state of a started dev server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerState {
    /// Process-group id (= pid of the spawned leader).
    pub pgid: i32,
    /// Listening port.
    pub port: u16,
    /// Base URL of the loopback address that answered the ready probe:
    /// `http://127.0.0.1:<port>` or `http://[::1]:<port>`.
    pub url: String,
    /// Canonical project root the server belongs to.
    pub root: PathBuf,
}

/// Answer of [`status`] (printed as JSON by `serve status`:
/// `{"status":"running",…state}` or `{"status":"stopped"}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Status {
    /// A state file exists and its process group is alive.
    Running(ServerState),
    /// No state file for this root (or it was cleaned up after the group died).
    Stopped,
}

/// Answer of [`stop`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Stopped {
    /// The recorded process group was terminated and reaped.
    Stopped(ServerState),
    /// There was no state file for this root: nothing to stop.
    NotRunning,
}

/// Lift an internal `Result` into the crate's three-valued answer.
#[track_caller]
fn lift<T>(r: Result<T, String>) -> Determination<T> {
    match r {
        Ok(v) => Determination::known(v),
        Err(why) => Determination::undetermined(why),
    }
}

fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(root).map_err(|e| {
        format!(
            "cannot canonicalize project root {}: {e}; pass --root <existing project directory>",
            root.display()
        )
    })
}

fn state_file_for(state: &Path, canonical: &Path) -> PathBuf {
    let hex: String = Sha256::digest(canonical.as_os_str().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    state.join(SERVE_DIR).join(format!("{hex}.json"))
}

/// Path of the state file for `root` under `state`.
///
/// Contract: `root` that cannot be canonicalized → `Undetermined`.
pub fn state_file(state: &Path, root: &Path) -> Determination<PathBuf> {
    lift(canonical_root(root).map(|c| state_file_for(state, &c)))
}

/// What is on disk for a root: no state file, or a validated state.
enum OnDisk {
    Absent,
    Present(ServerState),
}

/// Read and validate the state file. NotFound → `Absent`; any other read
/// error, invalid JSON, wrong shape, unknown key, or a state that does not
/// belong to `canonical` → `Err` (nothing may be acted on).
fn read_state(path: &Path, canonical: &Path) -> Result<OnDisk, String> {
    let fix = format!(
        "inspect it; if no dev server of yours is running for this root, delete {} and retry",
        path.display()
    );
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(OnDisk::Absent),
        Err(e) => {
            return Err(format!(
                "cannot read state file {}: {e}; {fix}",
                path.display()
            ))
        }
    };
    let st: ServerState = serde_json::from_str(&text)
        .map_err(|e| format!("state file {} is invalid: {e}; {fix}", path.display()))?;
    if st.pgid <= 1 {
        return Err(format!(
            "state file {} records pgid {} which is not a dev-server group; {fix}",
            path.display(),
            st.pgid
        ));
    }
    // SAFETY: getpgrp has no preconditions.
    let own = unsafe { libc::getpgrp() };
    if st.pgid == own {
        return Err(format!(
            "state file {} records sitecraft's own process group {}; {fix}",
            path.display(),
            st.pgid
        ));
    }
    if st.root != canonical {
        return Err(format!(
            "state file {} belongs to {}, not {}; {fix}",
            path.display(),
            st.root.display(),
            canonical.display()
        ));
    }
    if st.url != base_url(st.port) && st.url != base_url_v6(st.port) {
        return Err(format!(
            "state file {} has url {:?} inconsistent with port {}; {fix}",
            path.display(),
            st.url,
            st.port
        ));
    }
    Ok(OnDisk::Present(st))
}

fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn base_url_v6(port: u16) -> String {
    format!("http://[::1]:{port}")
}

/// `http://<addr>` — `http://127.0.0.1:<port>` or `http://[::1]:<port>` for
/// the loopback addresses the ready probe uses.
fn url_for(addr: SocketAddr) -> String {
    format!("http://{addr}")
}

/// Liveness of a process group.
#[derive(Debug, PartialEq, Eq)]
enum Group {
    Alive,
    Gone,
}

/// `killpg(pgid, sig)`: `None` on success, else the errno (`Err` when the
/// OS reported a failure without one — nothing to classify).
fn killpg_errno(pgid: i32, sig: libc::c_int) -> Result<Option<i32>, String> {
    // SAFETY: signal delivery (or, for 0, only the existence/permission
    // check) to a validated (> 1, not our own) group.
    let r = unsafe { libc::killpg(pgid, sig) };
    if r == 0 {
        return Ok(None);
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(code) => Ok(Some(code)),
        None => Err(format!(
            "killpg({pgid}, {sig}) failed without an errno: {e}"
        )),
    }
}

/// `strerror` text of `code`, for messages.
fn errno_text(code: i32) -> std::io::Error {
    std::io::Error::from_raw_os_error(code)
}

/// How to reap our own terminated leader before re-probing an EPERM group.
type Reap<'a> = Option<&'a mut dyn FnMut() -> Result<(), String>>;

/// `killpg(pgid, 0)`: success → alive; ESRCH → gone; EPERM → settled by
/// [`settle_eperm`] (`reap` first, then the group's members); anything else → `Err`.
///
/// Why EPERM needs settling: macOS answers EPERM both for a live group owned
/// by another user (we may not signal it: undetermined) **and** for a group
/// whose only members are unreaped zombies (every process has exited: gone)
/// or are still inside `exit` (not gone yet: alive). EPERM alone is never
/// read as gone. Off macOS there is no member listing and EPERM stays `Err`.
fn group_liveness(pgid: i32, reap: Reap<'_>) -> Result<Group, String> {
    match killpg_errno(pgid, 0)? {
        None => Ok(Group::Alive),
        Some(libc::ESRCH) => Ok(Group::Gone),
        Some(libc::EPERM) => settle_eperm(
            pgid,
            reap,
            format!(
                "cannot probe process group {pgid}: {} (the id may now belong to another user's group)",
                errno_text(libc::EPERM)
            ),
        ),
        Some(code) => Err(format!(
            "cannot probe process group {pgid}: {} (the id may now belong to another user's group)",
            errno_text(code)
        )),
    }
}

/// Settle an EPERM from `killpg(pgid, 0)` without guessing.
///
/// 1. If `reap` is given (the leader may be our own child), reap it and
///    probe again: ESRCH → gone, success → alive, EPERM → step 2, other → `Err`.
/// 2. Classify the group's members ([`classify_members`]): every member a
///    zombie → gone; some member still in `exit` → alive (not gone *yet*:
///    the caller keeps waiting); any other member → `Err(eperm)`, and the
///    listing failing or being unparseable → `Err`.
fn settle_eperm(pgid: i32, reap: Reap<'_>, eperm: String) -> Result<Group, String> {
    if let Some(reap) = reap {
        reap()?;
        match killpg_errno(pgid, 0)? {
            None => return Ok(Group::Alive),
            Some(libc::ESRCH) => return Ok(Group::Gone),
            Some(libc::EPERM) => {}
            Some(code) => {
                return Err(format!(
                    "cannot probe process group {pgid} after reaping: {}",
                    errno_text(code)
                ))
            }
        }
    }
    classify_members(pgid, eperm).map(group_of)
}

/// Liveness of a settled EPERM group: all zombies → gone; a member still
/// exiting → alive (not gone *yet*; never gone before every member is `SZOMB`).
fn group_of(s: Settled) -> Group {
    match s {
        Settled::AllZombies => Group::Gone,
        Settled::Exiting => Group::Alive,
    }
}

/// What the member listing of an EPERM group shows, when it settles anything.
#[derive(Debug, PartialEq, Eq)]
enum Settled {
    /// No member, or every member a zombie (`SZOMB`): every process of the
    /// group has exited.
    AllZombies,
    /// Every member is a zombie or still inside `exit` (`P_WEXIT` set, not
    /// yet `SZOMB`), and at least one is the latter. Measured on Darwin 25.6:
    /// right after SIGTERM, `killpg` already answers EPERM while the dying
    /// process is still listed `p_stat == SRUN` with `p_flag` 0x6004
    /// (`P_WEXIT` among it), for microseconds, before it turns `SZOMB`.
    Exiting,
}

/// Classify the members of an EPERM group: [`pgrp_members`] (its failure →
/// `Err`), then [`classify_member_list`].
fn classify_members(pgid: i32, eperm: String) -> Result<Settled, String> {
    let members = pgrp_members(pgid).map_err(|why| format!("{eperm}; {why}"))?;
    classify_member_list(&members, eperm)
}

/// The pure decision over an EPERM group's member list.
///
/// Any member that is neither a zombie nor exiting → `Err(eperm)`: EPERM on
/// a live group means we may not touch it (another user's group), which is
/// undetermined. Otherwise all zombies (or no member) → `AllZombies`, else
/// `Exiting`. EPERM is never read as gone by itself.
fn classify_member_list(members: &[Member], eperm: String) -> Result<Settled, String> {
    let live: Vec<String> = members
        .iter()
        .filter(|m| !m.zombie && !m.exiting)
        .map(|m| format!("pid {} p_stat {} p_flag {:#x}", m.pid, m.stat, m.flag))
        .collect();
    if !live.is_empty() {
        return Err(format!(
            "{eperm}; members that have not exited: {}",
            live.join(", ")
        ));
    }
    if members.iter().all(|m| m.zombie) {
        Ok(Settled::AllZombies)
    } else {
        Ok(Settled::Exiting)
    }
}

/// One process of a process group, as the kernel lists it.
#[derive(Debug)]
#[cfg_attr(
    not(all(target_os = "macos", target_pointer_width = "64")),
    allow(dead_code, reason = "only the macOS member listing constructs it")
)]
struct Member {
    pid: i32,
    /// `p_stat`.
    stat: u32,
    /// `p_flag`.
    flag: i32,
    /// `p_stat == SZOMB`.
    zombie: bool,
    /// `p_flag & P_WEXIT` and not yet a zombie.
    exiting: bool,
}

/// Members of process group `pgid`, zombies included, from
/// `sysctl(CTL_KERN, KERN_PROC, KERN_PROC_PGRP, pgid)`.
///
/// `libc` (0.2.186) does not define `struct kinfo_proc` for macOS, so the
/// four fields read here are taken at their byte offsets. Layout measured
/// with clang `offsetof`/`sizeof` on Darwin 25.6 arm64 (`<sys/sysctl.h>`,
/// LP64): `sizeof(struct kinfo_proc)` = 648, `kp_proc` at 0, `kp_eproc` at
/// 296, `extern_proc.p_flag` (int) at 32, `p_stat` (char) at 36, `p_pid` at
/// 40, `eproc.e_pgid` at 268; `P_WEXIT` = 0x2000 (`<sys/proc.h>`).
/// Every record is cross-checked (its `e_pgid` must be `pgid`, `p_pid` > 0,
/// `p_stat` one of SIDL..=SZOMB) so a layout mismatch is an `Err`, never a
/// misread "zombie". A zombie whose parent has not reaped it is listed with
/// `p_stat == SZOMB` (measured on the same system; `proc_pidinfo` instead
/// fails with ESRCH for such a zombie, which is why `sysctl` is used).
#[cfg(all(target_os = "macos", target_pointer_width = "64"))]
fn pgrp_members(pgid: i32) -> Result<Vec<Member>, String> {
    const KINFO_PROC_SIZE: usize = 648;
    const P_FLAG_OFFSET: usize = 32;
    const P_STAT_OFFSET: usize = 36;
    const P_PID_OFFSET: usize = 40;
    const E_PGID_OFFSET: usize = 296 + 268;
    const P_WEXIT: i32 = 0x2000;
    // `SIDL`..=`SZOMB` are 1..=5; `libc` exposes them as u32.
    const SIDL: u32 = libc::SIDL;
    const SZOMB: u32 = libc::SZOMB;

    let read_i32 = |rec: &[u8], at: usize| -> Result<i32, String> {
        rec.get(at..at + 4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(i32::from_ne_bytes)
            .ok_or_else(|| format!("kinfo_proc record too short for offset {at}"))
    };

    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PGRP, pgid];
    // The group can grow between the size query and the fetch (ENOMEM): retry
    // with headroom a bounded number of times, then give up as `Err`.
    let mut last_err = String::new();
    for _ in 0..4 {
        let mut len: libc::size_t = 0;
        // SAFETY: size query — null old buffer, `len` receives the size.
        let r = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                4,
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if r != 0 {
            return Err(format!(
                "cannot list the members of process group {pgid} (sysctl size query): {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut buf = vec![0u8; len + 4 * KINFO_PROC_SIZE];
        let mut got: libc::size_t = buf.len();
        // SAFETY: `buf` is writable for `got` bytes; sysctl writes at most that.
        let r = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                4,
                buf.as_mut_ptr().cast(),
                &mut got,
                std::ptr::null_mut(),
                0,
            )
        };
        if r != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ENOMEM) {
                last_err = e.to_string();
                continue;
            }
            return Err(format!(
                "cannot list the members of process group {pgid} (sysctl): {e}"
            ));
        }
        let data = buf.get(..got).ok_or_else(|| {
            format!(
                "sysctl reported {got} bytes for a {}-byte buffer",
                buf.len()
            )
        })?;
        if data.len() % KINFO_PROC_SIZE != 0 {
            return Err(format!(
                "cannot parse the members of process group {pgid}: {} bytes is not a multiple of sizeof(kinfo_proc) = {KINFO_PROC_SIZE}",
                data.len()
            ));
        }
        let mut members = Vec::new();
        for rec in data.chunks_exact(KINFO_PROC_SIZE) {
            let pid = read_i32(rec, P_PID_OFFSET)?;
            let rec_pgid = read_i32(rec, E_PGID_OFFSET)?;
            let stat = rec
                .get(P_STAT_OFFSET)
                .map(|&b| u32::from(b))
                .ok_or_else(|| "kinfo_proc record too short for p_stat".to_string())?;
            if pid <= 0 || rec_pgid != pgid || !(SIDL..=SZOMB).contains(&stat) {
                return Err(format!(
                    "cannot parse the members of process group {pgid}: implausible kinfo_proc record (pid {pid}, pgid {rec_pgid}, p_stat {stat})"
                ));
            }
            let flag = read_i32(rec, P_FLAG_OFFSET)?;
            let zombie = stat == SZOMB;
            members.push(Member {
                pid,
                stat,
                flag,
                zombie,
                exiting: !zombie && flag & P_WEXIT != 0,
            });
        }
        return Ok(members);
    }
    Err(format!(
        "cannot list the members of process group {pgid}: the group kept growing ({last_err})"
    ))
}

/// Not macOS (or not LP64): no member listing, so an EPERM stays `Err`.
#[cfg(not(all(target_os = "macos", target_pointer_width = "64")))]
fn pgrp_members(pgid: i32) -> Result<Vec<Member>, String> {
    Err(format!(
        "listing the members of process group {pgid} is not implemented on this platform"
    ))
}

/// Reap our own terminated child `pid` if there is one: `waitpid(pid,
/// WNOHANG)`. ECHILD (not our child, or already reaped) is the expected
/// "nothing of ours to reap"; EINTR is retried; any other failure is `Err`.
fn reap_own_child(pid: i32) -> Result<(), String> {
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: waitpid on one specific pid with WNOHANG; it only ever
        // reaps a terminated child of this process.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r >= 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::ECHILD) => return Ok(()),
            Some(libc::EINTR) => continue,
            _ => return Err(format!("cannot reap dev-server leader {pid}: {e}")),
        }
    }
}

/// Send `sig` to the group. ESRCH (already gone) is fine; EPERM is settled by
/// [`classify_members`]: macOS answers TERM/KILL to a group whose members
/// have all exited or are exiting with EPERM, and there is nothing left to
/// signal, so that is `Ok` (whether the group is *gone* is still decided by
/// the [`wait_gone`] that follows). EPERM with any other member stays `Err`,
/// as do all other errors.
fn signal_group(pgid: i32, sig: libc::c_int) -> Result<(), String> {
    match killpg_errno(pgid, sig)? {
        None | Some(libc::ESRCH) => Ok(()),
        Some(libc::EPERM) => {
            let eperm = format!(
                "cannot signal process group {pgid}: {}",
                errno_text(libc::EPERM)
            );
            match classify_members(pgid, eperm)? {
                Settled::AllZombies | Settled::Exiting => Ok(()),
            }
        }
        Some(code) => Err(format!(
            "cannot signal process group {pgid}: {}",
            errno_text(code)
        )),
    }
}

/// Wait (bounded) until the group is gone, reaping `child` (our own leader,
/// if we spawned it) so its zombie does not keep the group alive.
///
/// Without a `child` handle the leader may still be our own child: a
/// process that ran [`start`] and then [`stop`] (the Stop gate, `check
/// capability`) dropped the handle in `start`. It is reaped here with
/// `waitpid(pgid, WNOHANG)`; that reaps only a child of this process (for
/// any other pid it fails with ECHILD: nothing of ours to reap).
///
/// That reap runs *before* the probe, so it only helps when the leader was
/// already dead at that moment. When the leader dies between the reap and
/// the probe (typically just after the SIGTERM), or its parent is another
/// process, the probe sees a zombie-only group, which macOS answers with
/// EPERM. [`group_liveness`] settles that EPERM: it reaps again, re-probes,
/// and then reads the group's members — all zombies is gone, a member still
/// exiting is alive (so this loop polls again), any other member stays `Err`.
fn wait_gone(pgid: i32, child: &mut Option<&mut Child>, within: Duration) -> Result<bool, String> {
    let deadline = Instant::now() + within;
    loop {
        let mut reap = || -> Result<(), String> {
            match child.as_mut() {
                Some(c) => c
                    .try_wait()
                    .map(|_| ())
                    .map_err(|e| format!("cannot reap dev-server leader {pgid}: {e}")),
                None => reap_own_child(pgid),
            }
        };
        reap()?;
        if group_liveness(pgid, Some(&mut reap))? == Group::Gone {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Terminate the whole group: SIGTERM, grace, SIGKILL, wait, reap.
/// `Ok` only when the group is observed gone.
fn terminate_group(pgid: i32, mut child: Option<&mut Child>) -> Result<(), String> {
    signal_group(pgid, libc::SIGTERM)?;
    if !wait_gone(pgid, &mut child, TERM_GRACE)? {
        signal_group(pgid, libc::SIGKILL)?;
        if !wait_gone(pgid, &mut child, KILL_GRACE)? {
            return Err(format!(
                "process group {pgid} survived SIGKILL; kill it by hand (`kill -KILL -{pgid}`)"
            ));
        }
    }
    if let Some(c) = child {
        c.wait()
            .map_err(|e| format!("cannot reap dev-server leader {pgid}: {e}"))?;
    }
    Ok(())
}

/// Write the state file atomically (tmp file in the same dir, fsync, rename).
fn write_state(path: &Path, st: &ServerState) -> Result<(), String> {
    let text =
        serde_json::to_string_pretty(st).map_err(|e| format!("cannot encode the state: {e}"))?;
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    if let Err(e) = write() {
        // Best-effort cleanup of our own tmp file; the write error is reported.
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "cannot write state file {}: {e}; check that the state dir is writable",
            path.display()
        ));
    }
    Ok(())
}

/// The loopback families a dev server may bind. A listener on only one of
/// them is still a listener on "localhost", so every family must be checked.
fn loopbacks(port: u16) -> [(SocketAddr, bool); 2] {
    [
        (SocketAddr::from(([127, 0, 0, 1], port)), false),
        (
            SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)),
            true,
        ),
    ]
}

/// `EADDRNOTAVAIL` / `EAFNOSUPPORT`: the host has no such address family.
fn family_missing(e: &std::io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EADDRNOTAVAIL) | Some(libc::EAFNOSUPPORT)
    )
}

/// Outcome of probing one loopback family.
enum Probe {
    /// Connect refused (and, when asked, bind succeeded).
    Free,
    /// The family does not exist on this host: a `::1` socket cannot be
    /// connected/bound with EADDRNOTAVAIL/EAFNOSUPPORT, and a bind fails the
    /// same way, so nothing can be held there. Only ever returned for `::1`.
    Absent,
    /// Held, or unknown (every other error): not free.
    NotFree(String),
}

fn probe_family(addr: SocketAddr, v6: bool, bind: bool) -> Probe {
    let absent_by_bind = |why: String| match TcpListener::bind(addr) {
        Err(be) if v6 && family_missing(&be) => Probe::Absent,
        _ => Probe::NotFree(why),
    };
    match TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
        Ok(_) => return Probe::NotFree("still accepting connections".to_string()),
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {}
        Err(e) if v6 && family_missing(&e) => {
            return absent_by_bind(format!("cannot be probed ({e})"))
        }
        Err(e) => return Probe::NotFree(format!("cannot be probed ({e})")),
    }
    if !bind {
        return Probe::Free;
    }
    match TcpListener::bind(addr) {
        Ok(l) => {
            drop(l);
            Probe::Free
        }
        Err(e) if v6 && family_missing(&e) => Probe::Absent,
        Err(e) => Probe::NotFree(format!("not bindable ({e})")),
    }
}

/// Is `port` free on every loopback family (127.0.0.1 and [::1])? Free only
/// when each existing family refuses a connect **and** can be bound; anything
/// else is in use or undetermined (`Err`).
fn check_port_free(port: u16) -> Result<(), String> {
    let fix = format!("stop whatever listens on port {port} or change `port` in sitecraft.toml (then `sitecraft trust`)");
    for (addr, v6) in loopbacks(port) {
        if let Probe::NotFree(why) = probe_family(addr, v6, true) {
            return Err(format!(
                "port {port} is already in use or not probeable on {addr} ({why}); {fix}"
            ));
        }
    }
    Ok(())
}

/// After the group kill, confirm nothing still accepts on `port` on any
/// loopback family. Polls (up to ~2 s) until every family refuses. Anything
/// else is not a confirmation, so it stays `Err` (fail-closed).
fn confirm_port_released(port: u16) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let held = loop {
        let mut held = None;
        for (addr, v6) in loopbacks(port) {
            if let Probe::NotFree(why) = probe_family(addr, v6, false) {
                held = Some(format!("{addr}: {why}"));
                break;
            }
        }
        match held {
            None => return Ok(()),
            Some(h) if Instant::now() >= deadline => break h,
            Some(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    Err(format!(
        "port {port} is still in use ({held}) after the process group was killed; \
         a process that escaped the group (e.g. via setsid) may survive; \
         find and kill the listener (`lsof -iTCP:{port} -sTCP:LISTEN`), then retry"
    ))
}

/// One HTTP probe. `Some(code)` when an HTTP status line was read; `None`
/// when nothing answered with HTTP yet (refused / reset / timeout / garbage).
/// `None` only ever means "not ready": the caller keeps waiting until its
/// deadline, so an IO error here never becomes a ready answer.
fn probe_http(addr: SocketAddr, path: &str, budget: Duration) -> Option<u16> {
    let timeout = budget.min(PROBE_TIMEOUT).max(Duration::from_millis(10));
    let mut s = TcpStream::connect_timeout(&addr, timeout).ok()?;
    s.set_read_timeout(Some(timeout)).ok()?;
    s.set_write_timeout(Some(timeout)).ok()?;
    let req = format!("GET {path} HTTP/1.0\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).ok()?;
    let mut buf = [0u8; 64];
    let mut got = 0;
    while got < buf.len() {
        match s.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => {
                got += n;
                if buf[..got].contains(&b'\n') {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    parse_status_line(&buf[..got])
}

/// `HTTP/1.x NNN …` → `NNN`.
fn parse_status_line(b: &[u8]) -> Option<u16> {
    let line = std::str::from_utf8(b).ok()?.lines().next()?;
    let mut parts = line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    let code: u16 = parts.next()?.parse().ok()?;
    (100..=599).contains(&code).then_some(code)
}

/// Start the dev server for `root`.
///
/// Contract: config not `Config` or not `Trusted` → `Undetermined` without
/// spawning; port already listening → `Undetermined` without spawning; child
/// exits before ready, or ready deadline passes → group killed, `Undetermined`;
/// already running for this root → `Undetermined`; else `Known(state)` with the
/// state file written.
pub fn start(state: &Path, root: &Path) -> Determination<ServerState> {
    lift(start_inner(state, root))
}

fn start_inner(state: &Path, root: &Path) -> Result<ServerState, String> {
    let cfg_path = config::path_for(root);
    let (cfg, bytes) = match config::load(root) {
        Loaded::Config(c, b) => (c, b),
        Loaded::Absent => {
            return Err(format!(
                "no {} at {}; create it (command, port, ready_path, …) and run `sitecraft trust`",
                config::FILE_NAME,
                root.display()
            ))
        }
        Loaded::Undetermined(why) => {
            return Err(format!(
                "{why}; fix {} and run `sitecraft trust`",
                cfg_path.display()
            ))
        }
    };
    match trust::check_bytes(state, root, &cfg_path, &bytes) {
        Trust::Trusted => {}
        Trust::Untrusted(why) => return Err(format!("refusing to run the command: {why}")),
        Trust::Undetermined(why) => {
            return Err(format!(
                "cannot verify trust of {}: {why}; fix the trust store or run `sitecraft trust`",
                cfg_path.display()
            ))
        }
    }

    let canonical = canonical_root(root)?;
    let sf = state_file_for(state, &canonical);
    match read_state(&sf, &canonical)? {
        OnDisk::Absent => {}
        OnDisk::Present(st) => {
            return match group_liveness(st.pgid, None)? {
                Group::Alive => Err(format!(
                    "a dev server is already running for {} (pgid {}, {}); run `sitecraft serve stop` first",
                    canonical.display(),
                    st.pgid,
                    st.url
                )),
                Group::Gone => Err(format!(
                    "a stale state file {} names process group {} which is gone; run `sitecraft serve stop` to clear it",
                    sf.display(),
                    st.pgid
                )),
            }
        }
    }

    check_port_free(cfg.port)?;

    let dir = sf
        .parent()
        .ok_or_else(|| format!("state file {} has no parent dir", sf.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| {
        format!(
            "cannot create {}: {e}; check that the state dir is writable",
            dir.display()
        )
    })?;
    let log_path = sf.with_extension("log");
    let log = File::create(&log_path).map_err(|e| {
        format!(
            "cannot create the server log {}: {e}; check that the state dir is writable",
            log_path.display()
        )
    })?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("cannot duplicate the log handle: {e}"))?;

    let (prog, args) = cfg
        .command
        .split_first()
        .ok_or_else(|| format!("`command` in {} is empty", cfg_path.display()))?;
    let mut child = Command::new(prog)
        .args(args)
        .current_dir(&canonical)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .process_group(0)
        .spawn()
        .map_err(|e| {
            format!(
                "cannot spawn {prog:?}: {e}; fix `command` in {} (then `sitecraft trust`)",
                cfg_path.display()
            )
        })?;
    let pgid = match i32::try_from(child.id()) {
        Ok(p) => p,
        Err(e) => {
            // Cannot address the group by id: kill the leader directly.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("spawned pid {} is not a pgid: {e}", child.id()));
        }
    };

    let fail = |child: &mut Child, why: String| -> String {
        match terminate_group(pgid, Some(child)) {
            // Only claim success when the port is observed free: a daemon that
            // escaped the group (setsid) can survive a confirmed group kill.
            Ok(()) => match confirm_port_released(cfg.port) {
                Ok(()) => format!(
                    "{why} (process group {pgid} terminated; log: {})",
                    log_path.display()
                ),
                Err(held) => format!("{why}; {held} (log: {})", log_path.display()),
            },
            Err(kill_err) => format!("{why}; additionally, cleanup failed: {kill_err}"),
        }
    };

    let deadline = Instant::now() + Duration::from_secs(cfg.ready_timeout_secs);
    let mut last_code: Option<u16> = None;
    let ready_addr = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(fail(
                    &mut child,
                    format!(
                        "dev server exited before it was ready ({status}); check the log and fix `command` in {}",
                        cfg_path.display()
                    ),
                ))
            }
            Ok(None) => {}
            Err(e) => {
                return Err(fail(
                    &mut child,
                    format!("cannot observe the dev server process: {e}; retry `sitecraft serve start`"),
                ))
            }
        }
        let now = Instant::now();
        if now >= deadline {
            let seen = match last_code {
                Some(c) => format!("last answer was HTTP {c}"),
                None => "no HTTP answer".to_string(),
            };
            return Err(fail(
                &mut child,
                format!(
                    "dev server not ready on {}{} or {}{} within {}s ({seen}); raise `ready_timeout_secs` or fix `command`/`ready_path` in {} (then `sitecraft trust`)",
                    base_url(cfg.port),
                    cfg.ready_path,
                    base_url_v6(cfg.port),
                    cfg.ready_path,
                    cfg.ready_timeout_secs,
                    cfg_path.display()
                ),
            ));
        }
        // Every loopback family, IPv4 first: a server bound to `localhost`
        // may listen on [::1] only (F13). The first family answering 2xx/3xx
        // is the one recorded.
        let mut ready_on = None;
        for (addr, _) in loopbacks(cfg.port) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            if let Some(code) = probe_http(addr, &cfg.ready_path, left) {
                if (200..400).contains(&code) {
                    ready_on = Some(addr);
                    break;
                }
                last_code = Some(code);
            }
        }
        if let Some(addr) = ready_on {
            break addr;
        }
        std::thread::sleep(PROBE_INTERVAL);
    };

    // Ready — but a different process may have answered; the leader must
    // still be ours and alive.
    match child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => {
            return Err(fail(
                &mut child,
                format!(
                    "dev server exited ({status}) while the ready probe was answered (by another process?); free port {} and retry",
                    cfg.port
                ),
            ))
        }
        Err(e) => {
            return Err(fail(
                &mut child,
                format!("cannot observe the dev server process: {e}; retry `sitecraft serve start`"),
            ))
        }
    }

    let st = ServerState {
        pgid,
        port: cfg.port,
        url: url_for(ready_addr),
        root: canonical,
    };
    if let Err(why) = write_state(&sf, &st) {
        return Err(fail(&mut child, why));
    }
    Ok(st)
}

/// Stop the dev server for `root`.
///
/// Contract: no state file → `Known(NotRunning)`; state file unreadable or
/// invalid JSON → `Undetermined` (nothing killed); group terminated → state
/// file removed, `Known(Stopped)` only once the recorded port is observed free;
/// group survives SIGKILL, or the port still accepts → `Undetermined` (state kept).
pub fn stop(state: &Path, root: &Path) -> Determination<Stopped> {
    lift(stop_inner(state, root))
}

fn stop_inner(state: &Path, root: &Path) -> Result<Stopped, String> {
    let canonical = canonical_root(root)?;
    let sf = state_file_for(state, &canonical);
    let st = match read_state(&sf, &canonical)? {
        OnDisk::Absent => return Ok(Stopped::NotRunning),
        OnDisk::Present(st) => st,
    };
    terminate_group(st.pgid, None)?;
    // "The group is gone" is not "the server is stopped": a daemon that
    // escaped the group may still hold the port. Undetermined, and the state
    // file is deliberately KEPT so `stop`/`status` can be retried once the
    // listener is dealt with (removing it would lose the only record of the port).
    confirm_port_released(st.port)?;
    std::fs::remove_file(&sf).map_err(|e| {
        format!(
            "process group {} stopped but the state file {} cannot be removed: {e}; delete it by hand",
            st.pgid,
            sf.display()
        )
    })?;
    Ok(Stopped::Stopped(st))
}

/// Status of the dev server for `root`.
///
/// Contract: no state file → `Known(Stopped)`; unreadable / invalid →
/// `Undetermined`; recorded group alive → `Known(Running)`; recorded group dead
/// → `Undetermined` (it died without `stop`; report, do not guess).
pub fn status(state: &Path, root: &Path) -> Determination<Status> {
    lift(status_inner(state, root))
}

fn status_inner(state: &Path, root: &Path) -> Result<Status, String> {
    let canonical = canonical_root(root)?;
    let sf = state_file_for(state, &canonical);
    let st = match read_state(&sf, &canonical)? {
        OnDisk::Absent => return Ok(Status::Stopped),
        OnDisk::Present(st) => st,
    };
    match group_liveness(st.pgid, None) {
        Ok(Group::Alive) => Ok(Status::Running(st)),
        Ok(Group::Gone) => Err(format!(
            "process group {} recorded in {} is gone (the dev server died without `stop`); run `sitecraft serve stop` to clear the state, then `sitecraft serve start`",
            st.pgid,
            sf.display()
        )),
        Err(why) => Err(format!(
            "{why}; inspect it, then run `sitecraft serve stop` or delete {}",
            sf.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_parsing() {
        assert_eq!(parse_status_line(b"HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(parse_status_line(b"HTTP/1.0 404 Not Found\r\n"), Some(404));
        assert_eq!(parse_status_line(b"SSH-2.0-OpenSSH\r\n"), None);
        assert_eq!(parse_status_line(b"HTTP/1.1 abc\r\n"), None);
        assert_eq!(parse_status_line(b""), None);
    }

    fn member(pid: i32, zombie: bool, exiting: bool) -> Member {
        Member {
            pid,
            stat: if zombie { 5 } else { 2 },
            flag: if exiting { 0x2000 } else { 0 },
            zombie,
            exiting,
        }
    }

    const EPERM_TEXT: &str = "killpg(4242): EPERM";

    fn classify(members: &[Member]) -> Result<Settled, String> {
        classify_member_list(members, EPERM_TEXT.to_string())
    }

    #[test]
    fn eperm_empty_member_list_is_all_zombies() {
        assert_eq!(classify(&[]), Ok(Settled::AllZombies));
    }

    #[test]
    fn eperm_all_zombies_is_all_zombies() {
        let ms = [member(10, true, false), member(11, true, false)];
        assert_eq!(classify(&ms), Ok(Settled::AllZombies));
    }

    #[test]
    fn eperm_zombie_plus_exiting_is_exiting() {
        let ms = [member(10, true, false), member(11, false, true)];
        assert_eq!(classify(&ms), Ok(Settled::Exiting));
    }

    #[test]
    fn eperm_exiting_only_is_exiting() {
        let ms = [member(11, false, true)];
        assert_eq!(classify(&ms), Ok(Settled::Exiting));
    }

    #[test]
    fn eperm_live_member_is_err_with_text_and_pid() {
        let ms = [member(31337, false, false)];
        let err = classify(&ms).unwrap_err();
        assert!(err.contains(EPERM_TEXT), "missing eperm text: {err}");
        assert!(err.contains("31337"), "missing live pid: {err}");
    }

    #[test]
    fn eperm_live_plus_zombie_err_wins() {
        let ms = [member(10, true, false), member(777, false, false)];
        let err = classify(&ms).unwrap_err();
        assert!(err.contains(EPERM_TEXT), "missing eperm text: {err}");
        assert!(err.contains("777"), "missing live pid: {err}");
    }

    #[test]
    fn eperm_live_plus_exiting_err_wins() {
        let ms = [member(11, false, true), member(888, false, false)];
        let err = classify(&ms).unwrap_err();
        assert!(err.contains("888"), "missing live pid: {err}");
    }

    #[test]
    fn group_of_all_zombies_is_gone() {
        assert_eq!(group_of(Settled::AllZombies), Group::Gone);
    }

    #[test]
    fn group_of_exiting_is_alive_never_gone() {
        assert_eq!(group_of(Settled::Exiting), Group::Alive);
    }
}
