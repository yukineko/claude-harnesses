//! The task status TYPE (user ruling 2026-10-15: 「needs-rulingやunconfirmedは
//! 今後、型のレベルで拒否」).
//!
//! The set of statuses is closed: there is no catch-all variant and no
//! `#[serde(other)]`, so a stored `status` outside this enum (the removed
//! `needs-ruling` / `unconfirmed`, a typo, anything) fails deserialization with
//! serde's unknown-variant error, and naming a removed VARIANT
//! (`Status::NeedsRuling`, `Status::Unconfirmed`) is a compile error. Comparing
//! a `Status` with a string still compiles (`PartialEq<str>`); such a
//! comparison against a removed status is simply never true. See `tests/status_type.rs` and `tests/status_compile_fail.md`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Queued work (backed by a reproduced repro).
    Pending,
    /// Terminal: finished with recorded evidence.
    Done,
    /// Not terminal: requeued after its defer window.
    Failed,
    /// Terminal: decided not to do it (a recorded discard).
    Cancelled,
    /// DERIVED, never written by this binary: a pending/failed row holding a
    /// live claim-ledger lease. A legacy row persisted with `claimed` is read
    /// back as `Pending` by `store::load`.
    Claimed,
}

impl Status {
    /// Every status, in lifecycle order (the derived `Claimed` last).
    pub const ALL: [Status; 5] = [
        Status::Pending,
        Status::Done,
        Status::Failed,
        Status::Cancelled,
        Status::Claimed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
            Status::Claimed => "claimed",
        }
    }

    /// `done` and `cancelled`.
    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Done | Status::Cancelled)
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The unknown-status error of [`Status::from_str`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStatus(pub String);

impl std::fmt::Display for UnknownStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown status '{}'", self.0)
    }
}

impl std::error::Error for UnknownStatus {}

impl std::str::FromStr for Status {
    type Err = UnknownStatus;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Status::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or_else(|| UnknownStatus(s.to_string()))
    }
}

impl PartialEq<str> for Status {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Status {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
