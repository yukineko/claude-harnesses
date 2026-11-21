//! Group state (SPEC §2/§3): `state.json` under an exclusive flock, written
//! atomically (tmp + rename), and master election.
//!
//! Liveness is three-valued ([`Liveness`]). `Unknown` (heartbeat in the
//! future, or missing) never promotes anyone and never demotes a master: a
//! master whose liveness cannot be judged keeps the role, and a candidate with
//! an older-or-equal undeterminable session ahead of it is not promoted.

use crate::config;
use crate::group::{self, Group};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Heartbeats more than this far in the future are undeterminable.
pub const FUTURE_SKEW_SECS: i64 = 60;
const LOCK_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub registered_at: i64,
    #[serde(default)]
    pub last_heartbeat: Option<i64>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub requester: String,
    pub command: String,
    pub cwd: String,
    #[serde(default)]
    pub note: Option<String>,
    pub created_at: i64,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub completed_at: Option<i64>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub delivered: bool,
}

impl Item {
    pub fn is_open(&self) -> bool {
        self.status == "pending" || self.status == "in_progress"
    }
    pub fn is_completed(&self) -> bool {
        self.status == "done" || self.status == "failed"
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct State {
    #[serde(default)]
    pub sessions: BTreeMap<String, Session>,
    #[serde(default)]
    pub master: Option<String>,
    #[serde(default)]
    pub promoted_at: Option<i64>,
    #[serde(default)]
    pub queue: Vec<Item>,
    #[serde(default)]
    pub next_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Alive,
    Dead,
    Unknown,
}

impl Liveness {
    pub fn json(self) -> serde_json::Value {
        match self {
            Liveness::Alive => serde_json::Value::Bool(true),
            Liveness::Dead => serde_json::Value::Bool(false),
            Liveness::Unknown => serde_json::Value::Null,
        }
    }
}

pub fn liveness(s: &Session, now: i64, ttl: i64) -> Liveness {
    match s.last_heartbeat {
        None => Liveness::Unknown,
        Some(hb) if hb > now + FUTURE_SKEW_SECS => Liveness::Unknown,
        Some(hb) if now - hb <= ttl => Liveness::Alive,
        Some(_) => Liveness::Dead,
    }
}

/// Outcome of a `request`-style enqueue.
pub struct Enqueued {
    pub id: String,
    pub deduped: bool,
}

impl State {
    pub fn is_master(&self, session: &str) -> bool {
        self.master.as_deref() == Some(session)
    }

    /// Register or heartbeat `session` (keeps `registered_at`).
    pub fn touch(&mut self, session: &str, cwd: &str, now: i64) {
        match self.sessions.get_mut(session) {
            Some(s) => {
                s.last_heartbeat = Some(now);
                s.cwd = cwd.to_string();
            }
            None => {
                self.sessions.insert(
                    session.to_string(),
                    Session {
                        registered_at: now,
                        last_heartbeat: Some(now),
                        cwd: cwd.to_string(),
                        name: None,
                    },
                );
            }
        }
    }

    /// SPEC §3 election.
    pub fn elect(&mut self, now: i64, ttl: i64) {
        let master_live = match &self.master {
            None => None,
            Some(m) => self.sessions.get(m).map(|s| liveness(s, now, ttl)),
        };
        match master_live {
            Some(Liveness::Alive) | Some(Liveness::Unknown) => return,
            Some(Liveness::Dead) | None => {}
        }
        let master = self.master.clone();
        let mut ordered: Vec<(&String, &Session)> = self
            .sessions
            .iter()
            .filter(|(id, _)| Some(*id) != master.as_ref())
            .collect();
        ordered.sort_by(|a, b| a.1.registered_at.cmp(&b.1.registered_at).then(a.0.cmp(b.0)));
        let candidate = ordered
            .iter()
            .find(|(_, s)| liveness(s, now, ttl) == Liveness::Alive)
            .map(|(id, s)| ((*id).clone(), s.registered_at));
        let Some((cand, cand_reg)) = candidate else {
            return;
        };
        let blocked = self.sessions.iter().any(|(id, s)| {
            *id != cand && s.registered_at <= cand_reg && liveness(s, now, ttl) == Liveness::Unknown
        });
        if blocked {
            return;
        }
        self.master = Some(cand);
        self.promoted_at = Some(now);
    }

    pub fn enqueue(
        &mut self,
        requester: &str,
        kind: &str,
        command: &str,
        cwd: &str,
        note: Option<String>,
        now: i64,
    ) -> Enqueued {
        if let Some(it) = self.queue.iter().find(|i| {
            i.is_open()
                && i.requester == requester
                && i.kind == kind
                && i.command == command
                && i.cwd == cwd
        }) {
            return Enqueued {
                id: it.id.clone(),
                deduped: true,
            };
        }
        let max_existing = self
            .queue
            .iter()
            // Ids not of the `q<N>` shape are not ours to number past; they
            // only matter for uniqueness, which `next_id` already guarantees.
            .filter_map(|i| match i.id.strip_prefix('q').map(str::parse::<u64>) {
                Some(Ok(n)) => Some(n),
                _ => None,
            })
            .max();
        let mut n = self.next_id.max(1);
        if let Some(m) = max_existing {
            n = n.max(m + 1);
        }
        let id = format!("q{n}");
        self.next_id = n + 1;
        self.queue.push(Item {
            id: id.clone(),
            kind: kind.to_string(),
            status: "pending".to_string(),
            requester: requester.to_string(),
            command: command.to_string(),
            cwd: cwd.to_string(),
            note,
            created_at: now,
            started_at: None,
            completed_at: None,
            outcome: None,
            result: None,
            delivered: false,
        });
        Enqueued { id, deduped: false }
    }

    pub fn open_counts(&self) -> (usize, usize) {
        let p = self.queue.iter().filter(|i| i.status == "pending").count();
        let ip = self
            .queue
            .iter()
            .filter(|i| i.status == "in_progress")
            .count();
        (p, ip)
    }

    /// The item the master should look at first: the in-progress one, else
    /// the oldest pending.
    pub fn head(&self) -> Option<&Item> {
        self.queue
            .iter()
            .find(|i| i.status == "in_progress")
            .or_else(|| self.queue.iter().find(|i| i.status == "pending"))
    }

    pub fn master_label(&self) -> String {
        match &self.master {
            None => "(なし)".to_string(),
            Some(m) => match self.sessions.get(m).and_then(|s| s.name.clone()) {
                Some(n) => format!("{m} (name {n})"),
                None => m.clone(),
            },
        }
    }
}

/// Everything a locked operation needs to know.
pub struct Ctx {
    pub group: Group,
    pub now: i64,
    pub ttl: i64,
}

struct Lock {
    _file: std::fs::File,
}

fn acquire(path: &Path) -> Result<Lock, String> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("lock {} を開けない ({e})", path.display()))?;
    let start = Instant::now();
    loop {
        // SAFETY: flock on a valid, owned fd; released when the File drops.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Lock { _file: file });
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(format!("lock {} を取得できない ({err})", path.display()));
        }
        if start.elapsed() >= LOCK_WAIT {
            return Err(format!(
                "lock {} を {}s 以内に取得できない",
                path.display(),
                LOCK_WAIT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn group_dir(root: &Path, key: &str) -> PathBuf {
    root.join("groups").join(key)
}

fn load(path: &Path) -> Result<State, String> {
    match group::read_opt(path)? {
        None => Ok(State::default()),
        Some(text) => serde_json::from_str(&text)
            .map_err(|e| format!("{} を parse できない ({e})", path.display())),
    }
}

fn store(path: &Path, st: &State) -> Result<(), String> {
    let body = serde_json::to_string_pretty(st)
        .map_err(|e| format!("state を serialize できない ({e})"))?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let mut f =
        std::fs::File::create(&tmp).map_err(|e| format!("{} を作れない ({e})", tmp.display()))?;
    f.write_all(body.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("{} に書けない ({e})", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{} を rename できない ({e})", tmp.display()))
}

/// Resolve the group of `cwd`, lock it, load state, run `f`, write back if
/// `f` (or the election it ran) changed anything. Any failure is `Err`.
pub fn with_state<T>(cwd: &Path, f: impl FnOnce(&mut State, &Ctx) -> T) -> Result<T, String> {
    let now = config::now()?;
    let ttl = config::ttl_secs();
    let root = config::state_root()?;
    let group = group::resolve(cwd)?;
    let dir = group_dir(&root, &group.key);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{} を作れない ({e})", dir.display()))?;
    let _lock = acquire(&dir.join("lock"))?;
    let path = dir.join("state.json");
    let mut st = load(&path)?;
    let before = st.clone();
    let ctx = Ctx { group, now, ttl };
    let out = f(&mut st, &ctx);
    if st != before {
        store(&path, &st)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess(reg: i64, hb: Option<i64>) -> Session {
        Session {
            registered_at: reg,
            last_heartbeat: hb,
            cwd: String::new(),
            name: None,
        }
    }

    #[test]
    fn unknown_older_session_blocks_promotion() {
        let mut st = State::default();
        st.sessions.insert("A".into(), sess(0, Some(0)));
        st.sessions.insert("U".into(), sess(5, None));
        st.sessions.insert("B".into(), sess(10, Some(100)));
        st.master = Some("A".into());
        st.elect(100, 60);
        assert_eq!(st.master.as_deref(), Some("A"));
    }

    #[test]
    fn dead_master_replaced_by_oldest_live() {
        let mut st = State::default();
        st.sessions.insert("A".into(), sess(0, Some(0)));
        st.sessions.insert("C".into(), sess(20, Some(100)));
        st.sessions.insert("B".into(), sess(10, Some(100)));
        st.master = Some("A".into());
        st.elect(100, 60);
        assert_eq!(st.master.as_deref(), Some("B"));
    }
}
