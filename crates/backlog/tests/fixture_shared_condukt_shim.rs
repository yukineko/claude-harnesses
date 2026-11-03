//! GH #1510 — `Fixture::new` writes a NEW `condukt` shim into a unique
//! per-fixture dir. On this host the first exec of a freshly written
//! executable cost ~2.3s (measured 2026-11-03 at main fcbcfc3f: a new 3-line
//! `#!/bin/sh` script took real 2.35s / 2.32s on first exec, 0.00s after),
//! which uses up nearly all of `IS_CLAIMED_TIMEOUT` (2.5s), so `add` was
//! refused with "the cross-session claim check could not be made (timed out
//! after 2.5s)" in 14-17 of 31 `closure_evidence` tests.
//!
//! The shared, already-published `condukt_shim_dir()` does not pay that cost
//! per fixture. Coordinator ruling: do NOT fix this by raising the timeout.
//!
//! This test pins the cause, not the host timing (the timing is not
//! reproducible on demand): a default fixture must not mint a fresh `condukt`
//! executable of its own, and its default claim check must still be answered
//! "not claimed" so `add` of a reproduced ticket lands.
mod common;
use common::*;

#[test]
fn default_fixture_does_not_write_a_fresh_condukt_executable() {
    let f = Fixture::new("shared-condukt-shim");
    let own = f.shim.join("condukt");
    assert!(
        !own.exists(),
        "Fixture::new wrote a fresh per-fixture condukt at {} — its first exec pays the \
         host's new-executable cost against the 2.5s claim-check timeout (GH #1510); \
         resolve condukt from the shared condukt_shim_dir() instead",
        own.display()
    );
}

#[test]
fn default_fixture_add_of_a_reproduced_ticket_still_lands() {
    let f = Fixture::new("shared-condukt-shim-add");
    let out = f.run(&[
        "add",
        "--title",
        "shared shim answers the claim check",
        "--project",
        f.repo.to_str().unwrap(),
        "--repro-test",
        "bash tests/repro_yes.sh",
    ]);
    assert!(
        out.code == 0,
        "add with a reproduced repro must land through the fixture's default claim check; \
         code={}\nstdout={}\nstderr={}",
        out.code,
        out.stdout,
        out.stderr
    );
}
