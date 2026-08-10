//! The exit from spec to queue: a ratified spec's requirements become backlog
//! items, so specforge can be a SOURCE that hands implementation off rather
//! than implementing itself.
//!
//! Before this existed a ratified spec had no path into the queue at all — a
//! grep for `backlog`, `condukt` or `overwatch` across `src/forge/` returned a
//! single hit and it was a comment. The only spec→backlog handoff anywhere was
//! prose telling an LLM to run the CLI by hand.
//!
//! ## Why the CLI and not a library
//!
//! `backlog` is a bin-only crate with no `[lib]` target, and this shells out to
//! its CLI rather than growing one (user decision, 2026-08-02). It matches how
//! `flow`, `condukt` and `scout` already reach the queue, and it keeps the spec
//! harness from being coupled to the queue's internals. The cost is that the
//! contract is argv and JSON rather than types — which is why every subprocess
//! goes through [`harness_core::boundary::run`] and reads its stdout only via
//! [`CommandOutput::stdout_on_success`]. That accessor takes the acceptable exit
//! codes as an argument, so "I forgot to check whether the checker crashed"
//! is not expressible here as a one-character omission.
//!
//! Passing argv directly — never a shell string — also means requirement text
//! reaches the queue verbatim. A backtick in an acceptance criterion would be
//! command-substituted by a shell and would silently delete that span from the
//! record.
//!
//! ## Idempotency is the property that matters
//!
//! Re-ratifying a spec must add ZERO items, or the queue silently fills with
//! duplicates of work already tracked. Identity is carried by a per-requirement
//! tag ([`req_tag`]) derived only from the spec id and requirement id, so the
//! same requirement re-derives the same tag on every run.
//!
//! When the existing queue cannot be READ, this does not fall back to either
//! answer. Assuming "not present" duplicates the queue; assuming "present"
//! silently drops requirements a human ratified. Both are a guess dressed as a
//! result, so [`enqueue_with`] returns `Undetermined` and the caller exits
//! non-zero with the spec left ratified-but-unqueued — a state `specforge
//! queue` can retry once the queue is readable again.
//!
//! The listing deliberately passes NO `--status` filter. Measured 2026-08-02
//! against `backlog` on PATH: a task marked `done` is still returned by
//! `backlog list --project <p> --json`, so a completed requirement keeps
//! suppressing its own re-queue. Filtering to `pending` would resurrect finished
//! work on every re-ratification — the duplicate this module exists to prevent,
//! reintroduced through the query.

use crate::ir;
use harness_core::boundary::{self, CommandOutput};
use harness_core::verdict::Determination;
use std::collections::HashSet;

/// Tag marking every item this spec produced (`spec:<spec id>`).
pub fn spec_tag(spec_id: &str) -> String {
    format!("spec:{spec_id}")
}

/// The identity tag for one requirement (`req:<spec id>:<requirement id>`).
///
/// Derived ONLY from the two ids, never from the statement text, so editing a
/// requirement's wording does not orphan its queue item into a duplicate.
pub fn req_tag(spec_id: &str, req_id: &str) -> String {
    format!("req:{spec_id}:{req_id}")
}

/// What one `enqueue` call did. `already` is not a failure — it is the
/// idempotent path, and it is reported separately so a caller can never print
/// "queued N" for work that was already tracked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[must_use]
pub struct Enqueued {
    pub queued: Vec<String>,
    pub already: Vec<String>,
}

/// The backlog item title for a requirement.
pub fn item_title(spec: &ir::Spec, r: &ir::Requirement) -> String {
    format!("[{}:{}] {}", spec.spec.id, r.id, r.statement.trim())
}

/// The backlog item notes.
///
/// The acceptance criteria travel VERBATIM as the done-criteria block: they are
/// the spec's falsifiable conditions, and condukt's interpreter must be able to
/// lift them into `done_criteria` without re-deriving them from the statement
/// (re-derivation is where a requirement quietly loses its teeth).
pub fn item_notes(spec: &ir::Spec, r: &ir::Requirement) -> String {
    let mut s = format!(
        "specforge が spec {} の requirement {} から自動起票 (ratify 時).\n",
        spec.spec.id, r.id
    );
    if !spec.spec.title.trim().is_empty() {
        s.push_str(&format!("spec title: {}\n", spec.spec.title.trim()));
    }
    s.push_str(&format!("statement: {}\n", r.statement.trim()));
    s.push_str("\ndone_criteria (spec の acceptance を逐語で。言い換えないこと):\n");
    for a in &r.acceptance {
        s.push_str(&format!("  - {}\n", a.trim()));
    }
    if !r.canon.is_empty() {
        s.push_str("\ncanon:\n");
        for c in &r.canon {
            s.push_str(&format!("  - {}\n", c.trim()));
        }
    }
    s
}

/// Every tag present on any task in `backlog list --json` output.
///
/// A task with no `tags` key is skipped, but an output that does not parse AS A
/// WHOLE is `Undetermined`: an unreadable queue is unknown, not empty, and
/// "empty" here would mean "nothing is queued yet" — the answer that duplicates
/// everything.
pub fn parse_existing_tags(stdout: &str) -> Determination<HashSet<String>> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return Determination::undetermined(
            "`backlog list --json` output is not JSON; the queue is unknown, not empty",
        );
    };
    let tasks = match &v {
        serde_json::Value::Array(a) => a.clone(),
        serde_json::Value::Object(o) => {
            match o
                .get("tasks")
                .or_else(|| o.get("items"))
                .and_then(|t| t.as_array())
            {
                Some(a) => a.clone(),
                None => {
                    return Determination::undetermined(
                        "`backlog list --json` object has no tasks/items array; \
                         the queue is unknown, not empty",
                    )
                }
            }
        }
        _ => {
            return Determination::undetermined(
                "`backlog list --json` returned neither array nor object; \
                 the queue is unknown, not empty",
            )
        }
    };
    let mut out = HashSet::new();
    for t in tasks {
        if let Some(tags) = t.get("tags").and_then(|t| t.as_array()) {
            for tag in tags {
                if let Some(s) = tag.as_str() {
                    out.insert(s.to_string());
                }
            }
        }
    }
    Determination::known(out)
}

/// Runs one `backlog` invocation. Injectable so the exit-status and
/// parse-failure paths can be driven deterministically in tests — the paths
/// that are hardest to reach against a real binary are exactly the ones that
/// must not fall open.
pub trait BacklogRunner {
    fn run(&mut self, args: &[&str]) -> Determination<CommandOutput>;
}

/// Calls the real `backlog` binary on PATH.
pub struct CliRunner;

impl BacklogRunner for CliRunner {
    fn run(&mut self, args: &[&str]) -> Determination<CommandOutput> {
        boundary::run(std::process::Command::new("backlog").args(args))
    }
}

/// One `backlog` invocation's stdout, available only if the process both RAN
/// and exited 0.
///
/// `Determination` has no `and_then` by design — it is deliberately thin so
/// nothing resembling `unwrap_or` can grow on it — so the two undetermined arms
/// (could not start / did not exit 0) are threaded here explicitly. Forwarding
/// an existing `Undetermined` does not re-record telemetry; only minting does.
fn stdout_of<R: BacklogRunner>(runner: &mut R, args: &[&str]) -> Determination<String> {
    match runner.run(args) {
        Determination::Known(out) => out.stdout_on_success(),
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Queue one backlog item per requirement that does not have one yet.
///
/// `Undetermined` rather than a guess when the existing queue cannot be read or
/// parsed. A failure PART WAY THROUGH carries the count of items that DID land,
/// so the caller never reports a number larger than reality; re-running is safe
/// because the tag check runs against the queue as it then stands.
pub fn enqueue_with<R: BacklogRunner>(
    spec: &ir::Spec,
    project: &str,
    priority: &str,
    runner: &mut R,
) -> Determination<Enqueued> {
    let listing = match stdout_of(runner, &["list", "--project", project, "--json"]) {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => {
            return Determination::undetermined(format!(
                "既存キューを読めなかったので起票しない (重複も取りこぼしも防げないため): {}",
                why.as_str()
            ))
        }
    };
    let existing = match parse_existing_tags(&listing) {
        Determination::Known(t) => t,
        Determination::Undetermined(why) => return Determination::Undetermined(why),
    };

    let mut out = Enqueued::default();
    for r in &spec.requirements {
        let tag = req_tag(&spec.spec.id, &r.id);
        if existing.contains(&tag) {
            out.already.push(r.id.clone());
            continue;
        }
        let title = item_title(spec, r);
        let notes = item_notes(spec, r);
        let stag = spec_tag(&spec.spec.id);
        let args = [
            "add",
            "--title",
            title.as_str(),
            "--project",
            project,
            "--priority",
            priority,
            "--notes",
            notes.as_str(),
            "--tag",
            "spec",
            "--tag",
            stag.as_str(),
            "--tag",
            tag.as_str(),
        ];
        match stdout_of(runner, &args) {
            Determination::Known(_) => out.queued.push(r.id.clone()),
            Determination::Undetermined(why) => {
                return Determination::undetermined(format!(
                    "requirement {} を起票できなかった; その前に {} 件が起票済み: {}",
                    r.id,
                    out.queued.len(),
                    why.as_str()
                ))
            }
        }
    }
    Determination::known(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the tests destructure `require()`'s result; production code here
    // stays on `Determination`, so importing this at module scope would warn.
    use harness_core::verdict::Required;

    fn spec_with(ids: &[&str]) -> ir::Spec {
        ir::Spec {
            spec: ir::SpecMeta {
                id: "sl".to_string(),
                title: "spec loop".to_string(),
                status: "ratified".to_string(),
                provenance_commit: "abc".to_string(),
                date: "2026-08-02".to_string(),
                canon: vec!["CLAUDE.md".to_string()],
                ratification: None,
            },
            requirements: ids
                .iter()
                .map(|id| ir::Requirement {
                    id: (*id).to_string(),
                    statement: format!("statement for {id}"),
                    acceptance: vec![format!("acceptance for {id}")],
                    canon: vec!["CLAUDE.md".to_string()],
                    falsifiable: true,
                })
                .collect(),
        }
    }

    /// A REAL process producing `stdout` and exiting `code`.
    ///
    /// `CommandOutput` has private fields and no constructor on purpose — it
    /// must not be possible to mint "this checker succeeded" out of thin air —
    /// so the fixtures run `sh` rather than fabricating one. That also means
    /// these tests exercise `boundary::run` itself, not a stand-in for it.
    fn out(stdout: &str, code: i32) -> Determination<CommandOutput> {
        let quoted = format!("'{}'", stdout.replace('\'', r"'\''"));
        boundary::run(
            std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf '%s' {quoted}; exit {code}")),
        )
    }

    fn ok_out(stdout: &str) -> Determination<CommandOutput> {
        out(stdout, 0)
    }

    fn fail_out(code: i32) -> Determination<CommandOutput> {
        out("[]", code)
    }

    /// What `boundary::run` returns when the program does not exist.
    fn no_binary() -> Determination<CommandOutput> {
        boundary::run(&mut std::process::Command::new(
            "specforge-no-such-backlog-binary",
        ))
    }

    /// Replays a scripted sequence of outputs and records the argv it saw.
    struct FakeRunner {
        listing: Box<dyn Fn() -> Determination<CommandOutput>>,
        adds: Vec<Determination<CommandOutput>>,
        seen: Vec<Vec<String>>,
    }

    impl FakeRunner {
        fn new(listing: impl Fn() -> Determination<CommandOutput> + 'static) -> Self {
            Self {
                listing: Box::new(listing),
                adds: Vec::new(),
                seen: Vec::new(),
            }
        }
        fn with_adds(mut self, adds: Vec<Determination<CommandOutput>>) -> Self {
            self.adds = adds;
            self
        }
        fn adds_seen(&self) -> Vec<&Vec<String>> {
            self.seen
                .iter()
                .filter(|a| a.first().map(|s| s.as_str()) == Some("add"))
                .collect()
        }
    }

    impl BacklogRunner for FakeRunner {
        fn run(&mut self, args: &[&str]) -> Determination<CommandOutput> {
            self.seen.push(args.iter().map(|s| s.to_string()).collect());
            if args.first() == Some(&"list") {
                return (self.listing)();
            }
            if self.adds.is_empty() {
                return ok_out("");
            }
            self.adds.remove(0)
        }
    }

    /// Assert `Known` and hand over the value. Goes through `require`, the only
    /// extractor, so the tests use the same door production code has to.
    fn known(d: Determination<Enqueued>) -> Enqueued {
        d.require().expect("expected Known")
    }

    /// Assert `Undetermined` and hand over its reason text.
    ///
    /// The panic is the assertion: `Required` deliberately has no `unwrap_err`,
    /// so a test that wanted the blocked side has to say what it does with the
    /// determined side rather than get a silent default.
    #[allow(clippy::panic)]
    fn undetermined(d: Determination<Enqueued>) -> String {
        match d.require() {
            Required::Determined(v) => panic!("expected Undetermined, got Determined({v:?})"),
            Required::Blocked(verdict) => verdict
                .reason()
                .expect("an Undetermined verdict carries a reason")
                .as_str()
                .to_string(),
        }
    }

    fn tags_of(stdout: &str) -> HashSet<String> {
        parse_existing_tags(stdout)
            .require()
            .expect("expected Known tags")
    }

    #[test]
    fn ratifying_a_spec_queues_one_item_per_requirement() {
        let spec = spec_with(&["R0", "R1", "R2"]);
        let mut runner = FakeRunner::new(|| ok_out("[]"));
        let out = known(enqueue_with(&spec, "/p", "p1", &mut runner));
        assert_eq!(out.queued, vec!["R0", "R1", "R2"]);
        assert!(out.already.is_empty());

        let adds = runner.adds_seen();
        assert_eq!(adds.len(), 3);
        // The identity tag is what makes the second ratify a no-op.
        assert!(adds[0].contains(&"req:sl:R0".to_string()), "{:?}", adds[0]);
        assert!(adds[0].contains(&"spec:sl".to_string()), "{:?}", adds[0]);
        // Acceptance travels verbatim so condukt need not re-derive it.
        let notes = &adds[0][adds[0].iter().position(|s| s == "--notes").unwrap() + 1];
        assert!(notes.contains("acceptance for R0"), "{notes}");
    }

    #[test]
    fn a_second_ratify_of_the_same_spec_queues_nothing() {
        let spec = spec_with(&["R0", "R1"]);
        let mut runner = FakeRunner::new(|| {
            ok_out(
                r#"[{"id":"a","tags":["spec","spec:sl","req:sl:R0"]},
                    {"id":"b","tags":["spec","spec:sl","req:sl:R1"]}]"#,
            )
        });
        let out = known(enqueue_with(&spec, "/p", "p1", &mut runner));
        assert!(out.queued.is_empty(), "{out:?}");
        assert_eq!(out.already, vec!["R0", "R1"]);
        assert!(
            runner.adds_seen().is_empty(),
            "no add may be attempted: {:?}",
            runner.seen
        );
    }

    /// ANTI-VACUITY CONTROL for the idempotency test: the skip must key off the
    /// requirement tag, not suppress everything once the spec has any item. A
    /// requirement ADDED to an already-queued spec must still be queued.
    #[test]
    fn a_requirement_added_later_is_still_queued() {
        let spec = spec_with(&["R0", "R1"]);
        let mut runner =
            FakeRunner::new(|| ok_out(r#"[{"id":"a","tags":["spec","spec:sl","req:sl:R0"]}]"#));
        let out = known(enqueue_with(&spec, "/p", "p1", &mut runner));
        assert_eq!(out.queued, vec!["R1"]);
        assert_eq!(out.already, vec!["R0"]);
    }

    /// A DIFFERENT spec's requirement with the same requirement id must not be
    /// mistaken for this one — the tag is namespaced by spec id.
    #[test]
    fn a_same_named_requirement_of_another_spec_does_not_suppress_this_one() {
        let spec = spec_with(&["R0"]);
        let mut runner = FakeRunner::new(|| {
            ok_out(r#"[{"id":"a","tags":["spec","spec:other","req:other:R0"]}]"#)
        });
        let out = known(enqueue_with(&spec, "/p", "p1", &mut runner));
        assert_eq!(out.queued, vec!["R0"]);
        // The namespacing itself, not just this listing: a tag that dropped the
        // spec id would collide across every spec that numbers requirements R0.
        assert_ne!(req_tag("sl", "R0"), req_tag("other", "R0"));
    }

    #[test]
    fn a_nonzero_backlog_list_is_undetermined_not_an_empty_queue() {
        let spec = spec_with(&["R0"]);
        let mut runner = FakeRunner::new(|| fail_out(1));
        let why = undetermined(enqueue_with(&spec, "/p", "p1", &mut runner));
        assert!(why.contains("exited 1"), "{why}");
        assert!(
            runner.adds_seen().is_empty(),
            "a failed listing must not be treated as an empty queue"
        );
    }

    #[test]
    fn an_unparseable_listing_is_undetermined_not_an_empty_queue() {
        let spec = spec_with(&["R0"]);
        let mut runner = FakeRunner::new(|| ok_out("not json at all"));
        let why = undetermined(enqueue_with(&spec, "/p", "p1", &mut runner));
        assert!(why.contains("unknown, not empty"), "{why}");
        assert!(runner.adds_seen().is_empty());
    }

    #[test]
    fn a_missing_backlog_binary_is_undetermined_not_a_skip() {
        let spec = spec_with(&["R0"]);
        let mut runner = FakeRunner::new(no_binary);
        let why = undetermined(enqueue_with(&spec, "/p", "p1", &mut runner));
        assert!(runner.adds_seen().is_empty(), "{why}");
    }

    /// A partial failure must not report a count larger than what landed.
    #[test]
    fn a_failed_add_part_way_through_reports_how_many_landed() {
        let spec = spec_with(&["R0", "R1", "R2"]);
        let mut runner =
            FakeRunner::new(|| ok_out("[]")).with_adds(vec![ok_out(""), fail_out(3), ok_out("")]);
        let why = undetermined(enqueue_with(&spec, "/p", "p1", &mut runner));
        assert!(why.contains("requirement R1"), "{why}");
        assert!(why.contains("その前に 1 件"), "{why}");
    }

    #[test]
    fn parse_existing_tags_accepts_both_array_and_wrapped_object() {
        assert!(tags_of(r#"[{"tags":["x"]}]"#).contains("x"));
        assert!(tags_of(r#"{"tasks":[{"tags":["y"]}]}"#).contains("y"));
    }

    #[test]
    fn a_task_with_no_tags_is_skipped_not_fatal() {
        let t = tags_of(r#"[{"id":"a"},{"tags":["z"]}]"#);
        assert_eq!(t.len(), 1);
        assert!(t.contains("z"));
    }
}
