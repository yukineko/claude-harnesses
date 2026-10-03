//! blastguard's protection statement (backlog 3a8e3b73): what this gate
//! protects, from what, and on what grounds. Printed verbatim by
//! `blastguard --protects`, and read out of this file by
//! `scripts/check-gate-protection.py`, which requires every field non-empty.
//!
//! Keep each field one ordinary string literal (`\` line continuations are
//! fine): the checker parses the literal without building the crate and
//! reports anything else as undetermined. `grounds` must cite something that
//! can be checked — code or a measured incident — never a promise.

use harness_core::gate::Protection;

pub const PROTECTION: Protection = Protection {
    protects: "The session's files and uncommitted work (the project tree and \
        its git state under .git/), system directories such as /etc and \
        /Library, and the gate/hook/policy files (exclude::is_protected_path) \
        whose modification switches a guard off - except copies of those \
        files strictly inside a linked git worktree checkout under a worktree \
        storage root, which are that checkout's own tracked work \
        (scope::SafeRoots::classify_worktree_checkout).",
    against: "An agent tool call that irreversibly destroys, overwrites or \
        disarms them: a Bash command doing recursive or wildcard rm, git reset \
        --hard, git clean -fd/-fdx, working-tree discard, a truncating > \
        redirect, truncate/shred, mkfs or dd of=, recursive chmod/chown, or \
        find -delete; a Write that empties a file or writes into .git/ or a \
        system directory; an Edit/MultiEdit/NotebookEdit of a protected \
        gate/hook/policy path (other partial edits are allowed); and any \
        matched call blastguard cannot read or analyse, which it refuses (ask, \
        hardened to deny with no human present) rather than allows.",
    grounds: "Rationale, not a recorded data-loss incident: README.md 'Why it \
        exists' - these operations are irreversible and arrive buried in a \
        stream of tool calls, so a human catching each by eye is not \
        realistic. The refuse-when-undetermined half is grounded in measured \
        failures of blastguard itself: src/model.rs Decision ('Three answers, \
        not two') - the two-valued Allow/Deny forced every construct it could \
        not analyse into Allow; backlog 70883137 (src/main.rs) - with stdout \
        closed a DENY print panicked and the process exited 0, an allow; and \
        0.2.60 - writes creating /etc/sudoers.d/evil, /etc/paths.d/evil and \
        /Library/LaunchDaemons/evil.plist were measured ALLOWED by the \
        deployed 0.2.59 binary (scope::is_inside_system_dir now denies them).",
};
