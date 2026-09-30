//! The gate **protection statement**: what a gate protects, from what, and on
//! what grounds (backlog 3a8e3b73).
//!
//! A refusal that names only its MECHANISM ("this command matched rule X")
//! leaves the user unable to tell whether the concern is real. Each blocking
//! gate therefore declares, as a `const` literal of this type, the asset it
//! defends ([`Protection::protects`]), the failure or adversary it defends it
//! against ([`Protection::against`]), and the basis for believing that threat
//! is real ([`Protection::grounds`] — cited code or a recorded incident, not a
//! promise).
//!
//! Every field is a `&'static str` so the declaration is a plain literal that
//! `scripts/check-gate-protection.py` can read out of the gate's `src/`
//! without building it. That checker is what pins the declarations: it exits
//! non-zero when a gate listed in [`crate::fleet::BLOCKING_GATES`] has an
//! empty field, has no declaration and is not listed as PENDING in
//! `scripts/check-gate-protection.baseline`, or when that PENDING list grows
//! relative to HEAD. The type itself enforces nothing about emptiness — a
//! `const` cannot — so the checker, not this struct, is the gate.
//!
//! Keep each field a single ordinary string literal (`"..."`, optionally with
//! `\` line continuations). A field built with `concat!`, a raw string or a
//! reference to another `const` is not something the checker can read, and it
//! reports that as undetermined (exit 2) rather than guessing.

/// One gate's protection statement. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Protection {
    /// The asset the gate defends.
    pub protects: &'static str,
    /// The failure or adversary the gate defends that asset against.
    pub against: &'static str,
    /// The basis for the threat: cited code, a recorded incident, a
    /// measurement. Not a restatement of the mechanism.
    pub grounds: &'static str,
}
