//! Three-valued availability: is jev usable right now?
//!
//! The third answer is the point. "The key is absent" (observed) and "I could
//! not observe whether the key is present" are different facts, and collapsing
//! the second into the first is the §3 fail-open wearing a report's clothes —
//! it would let a broken environment read as a deliberate opt-out. Both mean
//! *do not call jev*, but they are reported, exit-coded, and logged apart.

use std::fmt;

/// The only place the key is read from. Deliberately **not** a CLI flag: a
/// flag lands in shell history and in `ps`.
pub const KEY_ENV: &str = "TYPESAFE_API_KEY";

/// Why the key is observably absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotConfiguredReason {
    /// `TYPESAFE_API_KEY` is not in the environment at all.
    EnvUnset,
    /// `TYPESAFE_API_KEY` is present but empty (or only whitespace). Treated as
    /// a deliberate opt-out, the same as unset — an empty string is how a
    /// shell spells "off".
    EnvEmpty,
}

impl NotConfiguredReason {
    pub fn as_str(self) -> &'static str {
        match self {
            NotConfiguredReason::EnvUnset => "TYPESAFE_API_KEY is not set",
            NotConfiguredReason::EnvEmpty => "TYPESAFE_API_KEY is set but empty",
        }
    }
}

/// A resolved API key.
///
/// `Debug` is hand-written to print only the last four characters, so a
/// `{:?}` added later by someone debugging cannot leak the credential into a
/// log, a panic message, or an error chain. There is no `Display`, no
/// `Serialize`, and no accessor that returns the whole key to another crate:
/// the full value leaves this type only through
/// [`Credential::authorization_header`], which is `pub(crate)` and is consumed
/// by the transport writing a `curl` stdin config.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential {
    key: String,
}

impl Credential {
    pub(crate) fn new(key: String) -> Self {
        Credential { key }
    }

    /// The last four characters, for human confirmation ("is the key I think
    /// is exported the one that is?"). Shorter keys yield fewer characters
    /// rather than the whole value.
    pub fn tail(&self) -> String {
        let n = self.key.chars().count();
        if n <= 4 {
            // Too short to show any of safely: a 4-char "tail" of a 4-char key
            // is the key.
            return "****".to_string();
        }
        self.key.chars().skip(n - 4).collect()
    }

    /// The `Authorization` header value. `pub(crate)` on purpose.
    pub(crate) fn authorization_header(&self) -> String {
        format!("Bearer {}", self.key)
    }

    /// The raw key, for redaction by exact match only.
    pub(crate) fn secret(&self) -> &str {
        &self.key
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Credential(****{})", self.tail())
    }
}

/// Can jev be called? Three answers, not two.
#[must_use = "an Availability decides whether a network call may happen; do not drop it"]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// A key was observed. Carries it, so a caller cannot reach the key
    /// without first having proved it exists.
    Configured(Credential),
    /// Observed absence. jev must not be called.
    NotConfigured { reason: NotConfiguredReason },
    /// Could not observe. jev must not be called — and this is **not**
    /// `NotConfigured`.
    Undetermined { reason: String },
}

impl Availability {
    /// Read the environment. The only entry point.
    pub fn detect() -> Self {
        Self::from_env_os(std::env::var_os(KEY_ENV))
    }

    /// The pure core of [`Availability::detect`], separated so the three
    /// branches are testable without mutating a process-global environment.
    pub fn from_env_os(raw: Option<std::ffi::OsString>) -> Self {
        let Some(raw) = raw else {
            return Availability::NotConfigured {
                reason: NotConfiguredReason::EnvUnset,
            };
        };
        // A key that is not UTF-8 cannot be a bearer token, but neither is it
        // evidence that the operator meant to turn jev off: something is wrong
        // with the environment. That is "could not determine", and mapping it
        // to NotConfigured would report a broken setup as an intentional
        // opt-out.
        let Some(s) = raw.to_str() else {
            return Availability::Undetermined {
                reason: format!("{KEY_ENV} is set but is not valid UTF-8"),
            };
        };
        if s.trim().is_empty() {
            return Availability::NotConfigured {
                reason: NotConfiguredReason::EnvEmpty,
            };
        }
        Availability::Configured(Credential::new(s.trim().to_string()))
    }

    /// Process exit code for `jev check`.
    ///
    /// `0` configured / `1` observed-absent / `10` undetermined. `1` and `10`
    /// both mean "do not use jev"; they are separate codes so a caller cannot
    /// confuse a deliberate opt-out with a broken environment. `10` follows the
    /// `specguard brief` convention for "undetermined".
    pub fn exit_code(&self) -> i32 {
        match self {
            Availability::Configured(_) => 0,
            Availability::NotConfigured { .. } => 1,
            Availability::Undetermined { .. } => 10,
        }
    }

    /// JSON for `jev check`. Contains at most the key's last four characters.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Availability::Configured(c) => serde_json::json!({
                "availability": "configured",
                "usable": true,
                "key_tail": c.tail(),
            }),
            Availability::NotConfigured { reason } => serde_json::json!({
                "availability": "not-configured",
                "usable": false,
                "reason": reason.as_str(),
            }),
            Availability::Undetermined { reason } => serde_json::json!({
                "availability": "undetermined",
                "usable": false,
                "reason": reason,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_is_not_configured() {
        let a = Availability::from_env_os(None);
        assert_eq!(
            a,
            Availability::NotConfigured {
                reason: NotConfiguredReason::EnvUnset
            }
        );
        assert_eq!(a.exit_code(), 1);
    }

    #[test]
    fn empty_and_whitespace_are_not_configured() {
        for raw in ["", "   ", "\t\n"] {
            let a = Availability::from_env_os(Some(raw.into()));
            assert_eq!(
                a,
                Availability::NotConfigured {
                    reason: NotConfiguredReason::EnvEmpty
                },
                "input {raw:?}"
            );
            assert_eq!(a.exit_code(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_is_undetermined_not_not_configured() {
        use std::os::unix::ffi::OsStringExt;
        let bad = std::ffi::OsString::from_vec(vec![0x61, 0x80, 0xff]);
        let a = Availability::from_env_os(Some(bad));
        match &a {
            Availability::Undetermined { reason } => {
                assert!(reason.contains("not valid UTF-8"), "reason: {reason}");
            }
            other => unreachable!("expected Undetermined, got {other:?}"),
        }
        // The distinction this crate exists to keep.
        assert_eq!(a.exit_code(), 10);
        assert_ne!(a.exit_code(), 1);
    }

    #[test]
    fn configured_exposes_only_the_tail() {
        let a = Availability::from_env_os(Some("apikey_supersecretvalue1234".into()));
        match &a {
            Availability::Configured(c) => {
                assert_eq!(c.tail(), "1234");
                let shown = format!("{c:?}");
                assert!(!shown.contains("supersecret"), "Debug leaked: {shown}");
                let json = a.to_json().to_string();
                assert!(!json.contains("supersecret"), "json leaked: {json}");
            }
            other => unreachable!("expected Configured, got {other:?}"),
        }
        assert_eq!(a.exit_code(), 0);
    }

    #[test]
    fn short_key_tail_is_fully_masked() {
        let c = Credential::new("abcd".to_string());
        assert_eq!(c.tail(), "****");
        let c = Credential::new("ab".to_string());
        assert_eq!(c.tail(), "****");
    }
}
