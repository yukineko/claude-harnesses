//! The pure decision core of `flow stop-gate`.
//!
//! Every observation the gate makes of the world (driver registry, `backlog
//! driver status`, `backlog next`, `condukt state claims`, `condukt state
//! is-claimed`, `condukt circuit check`) is reached through the [`Probes`]
//! trait, so the decision below is a function of those observations alone and
//! is unit-tested with a fake.
//!
//! Each observation is an [`Obs`]: `Known(T)` or `Unknown(why)`. `Unknown` is
//! never folded into an empty/clean value here — it reaches [`Outcome::Undetermined`]
//! unless a *determined* allow (stale registration, circuit trip, ...) or a
//! determined block (known remaining work) already decides the stop.
//!
//! User ruling 2026-11-21: a /flow driver never stops until the problem is
//! solved; the only stop reason with work remaining is the condukt circuit
//! breaker; a session that is not a flow driver is never blocked.
//!
//! Backlog e1e8c632: the pending task `backlog next` returns is NOT this
//! driver's work when `condukt state is-claimed` positively reports its
//! hashkey held by a run other than `flow-<SESSION>` — the skill's claim-skip
//! gate forbids this session to take it, so blocking on it could never be
//! satisfied. Only that positive answer removes the task: no hashkey, an
//! unclaimed answer, a claim held by `flow-<SESSION>`, or an undetermined
//! lookup all keep it as remaining work (block). `backlog next` names only the
//! first pending task; tasks queued behind a task claimed elsewhere are not
//! examined, and the allow message says so.

/// One observation: seen, or could not be seen (with why).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Obs<T> {
    Known(T),
    Unknown(String),
}

/// What `backlog driver status` says about this session's registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// Listed under `drivers` (heartbeat within TTL).
    Live,
    /// Listed only under `stale`.
    Stale,
    /// Listed in neither.
    Absent,
}

/// The next pending backlog task (`backlog next`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub title: String,
    /// The task's condukt claim key (`hashkey` in `backlog next` output), when
    /// present. Without it the claim cannot be looked up, so the task stays
    /// remaining work.
    pub hashkey: Option<String>,
}

/// Who holds a task hashkey's condukt claim (`condukt state is-claimed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// Exit 1 with `"claimed": false`.
    Unclaimed,
    /// Exit 0, held by this run id (`holder_run`).
    Run(String),
}

/// `condukt circuit check` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Circuit {
    Continue,
    /// Tripped, with the reason from its JSON.
    Trip(String),
}

/// The gate's answer before the bounded-undetermined rule is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Not a driver (or nothing to say): no stdout decision at all.
    AllowSilent,
    /// Determined allow with a visible `systemMessage`.
    Allow(String),
    /// Determined block: work remains and the circuit says continue.
    Block(String),
    /// Could not determine; each entry names what was unreadable.
    Undetermined(Vec<String>),
}

/// Access to the world, one call per observation.
pub trait Probes {
    fn liveness(&mut self, project: &str) -> Obs<Liveness>;
    fn next(&mut self, project: &str) -> Obs<Option<Task>>;
    /// Keys of claims held by this session's flow run (`run_id == flow-<S>`).
    fn claims(&mut self, project: &str) -> Obs<Vec<String>>;
    /// Holder of `hashkey`'s condukt task claim.
    fn holder(&mut self, project: &str, hashkey: &str) -> Obs<Holder>;
    fn circuit(&mut self, project: &str) -> Obs<Circuit>;
}

/// The pending task after the claim-skip filter.
enum Pending {
    None,
    /// Remaining work; `note` says why a claim lookup did not exclude it.
    Work(Task, Option<String>),
    /// Positively held by another run: not this driver's work.
    Elsewhere(Task, String),
}

fn filter_pending<P: Probes>(
    session: &str,
    project: &str,
    task: Option<Task>,
    probes: &mut P,
) -> Pending {
    let Some(t) = task else {
        return Pending::None;
    };
    let Some(hk) = t.hashkey.clone() else {
        return Pending::Work(
            t,
            Some("it carries no hashkey, so its condukt claim could not be looked up".into()),
        );
    };
    let own = format!("flow-{session}");
    match probes.holder(project, &hk) {
        Obs::Known(Holder::Run(r)) if r != own => Pending::Elsewhere(t, r),
        Obs::Known(_) => Pending::Work(t, None),
        Obs::Unknown(why) => Pending::Work(
            t,
            Some(format!(
                "its condukt claim could not be determined (`condukt state is-claimed \
                 --hashkey {hk}`: {why}), so it is not excluded as another session's task"
            )),
        ),
    }
}

/// Decide one registered project for `session`.
pub fn decide_project<P: Probes>(session: &str, project: &str, probes: &mut P) -> Outcome {
    match probes.liveness(project) {
        Obs::Unknown(why) => {
            return Outcome::Undetermined(vec![format!(
                "driver liveness (`backlog driver status --project {project}`): {why}"
            )])
        }
        Obs::Known(Liveness::Absent) => return Outcome::AllowSilent,
        Obs::Known(Liveness::Stale) => {
            return Outcome::Allow(format!(
                "flow stop-gate: this session's /flow driver registration for {project} is \
                 STALE (heartbeat past the TTL), so the gate is not blocking this stop. If \
                 /flow is still meant to be driving, re-register (`backlog driver register`); \
                 otherwise run Step 4 (`backlog driver unregister`) to remove the record."
            ))
        }
        Obs::Known(Liveness::Live) => {}
    }

    let next = probes.next(project);
    let claims = probes.claims(project);
    let mut unknown = Vec::new();
    let pending = match &next {
        Obs::Known(t) => t.clone(),
        Obs::Unknown(why) => {
            unknown.push(format!(
                "pending backlog task (`backlog next --project {project}`): {why}"
            ));
            None
        }
    };
    let held = match &claims {
        Obs::Known(c) => c.clone(),
        Obs::Unknown(why) => {
            unknown.push(format!("held flow claims (`condukt state claims`): {why}"));
            Vec::new()
        }
    };
    let (pending, note, elsewhere) = match filter_pending(session, project, pending, probes) {
        Pending::None => (None, None, None),
        Pending::Work(t, note) => (Some(t), note, None),
        Pending::Elsewhere(t, run) => (None, None, Some((t, run))),
    };
    let work_known = pending.is_some() || !held.is_empty();
    if unknown.is_empty() && !work_known {
        if let Some((t, run)) = elsewhere {
            return Outcome::Allow(format!(
                "flow stop-gate: the pending backlog task `backlog next` returns for {project}, \
                 {} \"{}\", is claimed by condukt run {run} (not this session's run \
                 flow-{session}), so per the /flow claim-skip gate it is not this driver's work, \
                 and this session holds no flow claims: this stop is allowed. `backlog next` \
                 names only the first pending task, so tasks queued behind it were NOT examined \
                 by this gate. If none remain, run /flow Step 4 (`backlog driver unregister`) \
                 so the driver registration does not linger.",
                t.id, t.title
            ));
        }
        return Outcome::Allow(format!(
            "flow stop-gate: the backlog queue for {project} is empty and this session holds no \
             flow claims, so this stop is allowed. Run /flow Step 4 now \
             (`backlog driver unregister`) so the driver registration does not linger."
        ));
    }

    match probes.circuit(project) {
        Obs::Known(Circuit::Trip(reason)) => Outcome::Allow(format!(
            "flow stop-gate: the condukt circuit breaker TRIPPED (reason: {reason}) for run \
             flow-{session}, so this stop is allowed even though work may remain in {project}. \
             Run /flow Step 4 (`backlog driver unregister`) and report the trip."
        )),
        Obs::Unknown(why) => {
            unknown.push(format!(
                "circuit breaker (`condukt circuit check --run flow-{session}`): {why}"
            ));
            Outcome::Undetermined(unknown)
        }
        Obs::Known(Circuit::Continue) if work_known => Outcome::Block(work_reason(
            session,
            project,
            pending.as_ref(),
            note.as_deref(),
            &held,
        )),
        Obs::Known(Circuit::Continue) => Outcome::Undetermined(unknown),
    }
}

fn work_reason(
    session: &str,
    project: &str,
    pending: Option<&Task>,
    note: Option<&str>,
    held: &[String],
) -> String {
    let mut why = Vec::new();
    if let Some(t) = pending {
        match note {
            Some(n) => why.push(format!(
                "pending backlog task {} \"{}\" ({n})",
                t.id, t.title
            )),
            None => why.push(format!("pending backlog task {} \"{}\"", t.id, t.title)),
        }
    }
    if !held.is_empty() {
        why.push(format!(
            "{} claim(s) held by run flow-{session} ({})",
            held.len(),
            held.join(", ")
        ));
    }
    format!(
        "flow stop-gate: this session is a live /flow driver for {project} and work remains: {}. \
         The condukt circuit breaker says continue, so this stop is BLOCKED. Continue /flow \
         Step 3: `backlog next --claim`, run condukt on it, sink the result with `backlog done` \
         / `backlog fail`, then release the task — and repeat. Stopping is allowed only when \
         the queue is empty, when the circuit breaker trips, or after Step 4 \
         (`backlog driver unregister`).",
        why.join("; ")
    )
}

/// Combine per-project outcomes: Block > Undetermined > Allow > AllowSilent.
/// Messages of the same rank are joined.
pub fn combine(outcomes: Vec<Outcome>) -> Outcome {
    let mut blocks = Vec::new();
    let mut unknown = Vec::new();
    let mut allows = Vec::new();
    for o in outcomes {
        match o {
            Outcome::Block(r) => blocks.push(r),
            Outcome::Undetermined(u) => unknown.extend(u),
            Outcome::Allow(m) => allows.push(m),
            Outcome::AllowSilent => {}
        }
    }
    if !blocks.is_empty() {
        Outcome::Block(blocks.join("\n"))
    } else if !unknown.is_empty() {
        Outcome::Undetermined(unknown)
    } else if !allows.is_empty() {
        Outcome::Allow(allows.join("\n"))
    } else {
        Outcome::AllowSilent
    }
}

/// State of the per-session undetermined marker, read before deciding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    Absent,
    Present,
    /// No session id to key it on, or it could not be read.
    Unusable(String),
}

/// The final hook answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Final {
    Silent,
    SystemMessage(String),
    Block(String),
}

/// What to do with the marker after deciding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerOp {
    Clear,
    Write,
}

/// Apply the bounded-undetermined rule.
///
/// * a determined outcome passes through and clears the marker;
/// * undetermined on a chain-first stop (`stop_hook_active == Some(false)`)
///   blocks and records the marker;
/// * undetermined on a re-entry with the marker present is the bounded allow
///   (visible `systemMessage`, marker cleared);
/// * undetermined on a re-entry with the marker absent is this gate's FIRST
///   undetermined stop in the chain: block and record — unless the record
///   cannot be written, which [`on_write_failure`] turns into a visible allow;
/// * undetermined on a re-entry with the marker unusable: visible allow
///   (blocking without a record could loop forever).
///
/// `stop_hook_active == None` (payload unreadable) is bounded by the marker
/// alone: present or unusable → visible allow, absent → block.
pub fn resolve(
    outcome: Outcome,
    stop_hook_active: Option<bool>,
    marker: &Marker,
) -> (Final, MarkerOp) {
    let reasons = match outcome {
        Outcome::AllowSilent => return (Final::Silent, MarkerOp::Clear),
        Outcome::Allow(m) => return (Final::SystemMessage(m), MarkerOp::Clear),
        Outcome::Block(r) => return (Final::Block(r), MarkerOp::Clear),
        Outcome::Undetermined(u) => u,
    };
    let what = reasons.join("; ");
    let block = Final::Block(format!(
        "flow stop-gate: could not determine whether this /flow driver session may stop. \
         Unreadable: {what}. This stop is blocked fail-closed (cannot-determine is not \
         \"no work\"). Fix what is unreadable, or continue /flow Step 3 (`backlog next --claim`, \
         run condukt, `backlog done` / `backlog fail`, release the task); stopping is allowed \
         when the queue is empty, when the circuit breaker trips, or after Step 4 \
         (`backlog driver unregister`). Only this gate's next consecutive undetermined stop \
         is let through, so this cannot loop."
    ));
    let allow = |extra: &str| {
        Final::SystemMessage(format!(
            "flow stop-gate: could not determine whether this /flow driver session may stop \
             ({what}){extra}; this stop is allowed UNCHECKED so the session is not trapped. \
             Treat it as not verified by flow stop-gate."
        ))
    };
    match (stop_hook_active, marker) {
        (Some(false), _) => (block, MarkerOp::Write),
        (Some(true), Marker::Present) | (None, Marker::Present) => (
            allow(", and the previous stop was already blocked for the same reason"),
            MarkerOp::Clear,
        ),
        (Some(true), Marker::Unusable(why)) | (None, Marker::Unusable(why)) => (
            allow(&format!(", and the bound marker is unusable ({why})")),
            MarkerOp::Clear,
        ),
        (Some(true), Marker::Absent) | (None, Marker::Absent) => (block, MarkerOp::Write),
    }
}

/// A block whose marker could not be written on a re-entry would be
/// unbounded: turn it into a visible allow. On a chain-first stop the block
/// stands (the next re-entry sees an unusable/absent marker and is bounded
/// there).
pub fn on_write_failure(fin: Final, stop_hook_active: Option<bool>, err: &str) -> Final {
    match (fin, stop_hook_active) {
        (Final::Block(r), Some(false)) => Final::Block(r),
        (Final::Block(r), _) => Final::SystemMessage(format!(
            "{r}\n(flow stop-gate: the bound marker could not be written ({err}), so instead of \
             blocking without a bound this stop is allowed UNCHECKED.)"
        )),
        (other, _) => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        live: Option<Obs<Liveness>>,
        next: Option<Obs<Option<Task>>>,
        claims: Option<Obs<Vec<String>>>,
        holder: Option<Obs<Holder>>,
        circuit: Option<Obs<Circuit>>,
        calls: Vec<&'static str>,
    }

    impl Probes for Fake {
        fn liveness(&mut self, _: &str) -> Obs<Liveness> {
            self.calls.push("liveness");
            self.live.clone().unwrap_or(Obs::Unknown("unset".into()))
        }
        fn next(&mut self, _: &str) -> Obs<Option<Task>> {
            self.calls.push("next");
            self.next.clone().unwrap_or(Obs::Unknown("unset".into()))
        }
        fn claims(&mut self, _: &str) -> Obs<Vec<String>> {
            self.calls.push("claims");
            self.claims.clone().unwrap_or(Obs::Unknown("unset".into()))
        }
        fn holder(&mut self, _: &str, _: &str) -> Obs<Holder> {
            self.calls.push("holder");
            self.holder.clone().unwrap_or(Obs::Unknown("unset".into()))
        }
        fn circuit(&mut self, _: &str) -> Obs<Circuit> {
            self.calls.push("circuit");
            self.circuit.clone().unwrap_or(Obs::Unknown("unset".into()))
        }
    }

    fn task() -> Option<Task> {
        Some(Task {
            id: "t1".into(),
            title: "do it".into(),
            hashkey: Some("hk1".into()),
        })
    }

    fn live(next: Obs<Option<Task>>, claims: Obs<Vec<String>>, circuit: Obs<Circuit>) -> Fake {
        Fake {
            live: Some(Obs::Known(Liveness::Live)),
            next: Some(next),
            claims: Some(claims),
            holder: Some(Obs::Known(Holder::Unclaimed)),
            circuit: Some(circuit),
            ..Fake::default()
        }
    }

    #[test]
    fn absent_registration_is_silent_and_probes_nothing_else() {
        let mut f = Fake {
            live: Some(Obs::Known(Liveness::Absent)),
            ..Fake::default()
        };
        assert_eq!(decide_project("s", "/p", &mut f), Outcome::AllowSilent);
        assert_eq!(f.calls, vec!["liveness"]);
    }

    #[test]
    fn stale_registration_allows_with_message() {
        let mut f = Fake {
            live: Some(Obs::Known(Liveness::Stale)),
            ..Fake::default()
        };
        let o = decide_project("s", "/p", &mut f);
        assert!(matches!(o, Outcome::Allow(m) if m.contains("STALE")));
    }

    #[test]
    fn liveness_unknown_is_undetermined() {
        let mut f = Fake::default();
        assert!(matches!(
            decide_project("s", "/p", &mut f),
            Outcome::Undetermined(_)
        ));
    }

    #[test]
    fn empty_queue_allows_without_circuit() {
        let mut f = live(
            Obs::Known(None),
            Obs::Known(vec![]),
            Obs::Known(Circuit::Continue),
        );
        let o = decide_project("s", "/p", &mut f);
        assert!(matches!(o, Outcome::Allow(m) if m.contains("unregister")));
        assert!(!f.calls.contains(&"circuit"));
    }

    #[test]
    fn pending_task_and_continue_blocks_naming_task() {
        let mut f = live(
            Obs::Known(task()),
            Obs::Known(vec![]),
            Obs::Known(Circuit::Continue),
        );
        let o = decide_project("s", "/p", &mut f);
        assert!(matches!(&o, Outcome::Block(r) if r.contains("t1") && r.contains("circuit")));
    }

    #[test]
    fn held_claim_alone_blocks() {
        let mut f = live(
            Obs::Known(None),
            Obs::Known(vec!["hk".into()]),
            Obs::Known(Circuit::Continue),
        );
        assert!(matches!(
            decide_project("s", "/p", &mut f),
            Outcome::Block(r) if r.contains("hk")
        ));
    }

    #[test]
    fn known_work_with_unknown_claims_still_blocks_determined() {
        let mut f = live(
            Obs::Known(task()),
            Obs::Unknown("x".into()),
            Obs::Known(Circuit::Continue),
        );
        assert!(matches!(
            decide_project("s", "/p", &mut f),
            Outcome::Block(_)
        ));
    }

    #[test]
    fn trip_allows_even_with_work() {
        let mut f = live(
            Obs::Known(task()),
            Obs::Known(vec![]),
            Obs::Known(Circuit::Trip("failure_streak".into())),
        );
        assert!(matches!(
            decide_project("s", "/p", &mut f),
            Outcome::Allow(m) if m.contains("failure_streak") && m.contains("circuit")
        ));
    }

    #[test]
    fn trip_allows_even_when_work_unknown() {
        let mut f = live(
            Obs::Unknown("x".into()),
            Obs::Known(vec![]),
            Obs::Known(Circuit::Trip("r".into())),
        );
        assert!(matches!(
            decide_project("s", "/p", &mut f),
            Outcome::Allow(_)
        ));
    }

    #[test]
    fn unknown_work_and_continue_is_undetermined() {
        let mut f = live(
            Obs::Unknown("next broke".into()),
            Obs::Known(vec![]),
            Obs::Known(Circuit::Continue),
        );
        assert!(matches!(
            decide_project("s", "/p", &mut f),
            Outcome::Undetermined(u) if u.iter().any(|s| s.contains("next broke"))
        ));
    }

    #[test]
    fn circuit_unknown_with_work_is_undetermined() {
        let mut f = live(
            Obs::Known(task()),
            Obs::Known(vec![]),
            Obs::Unknown("c".into()),
        );
        assert!(matches!(
            decide_project("s", "/p", &mut f),
            Outcome::Undetermined(_)
        ));
    }

    #[test]
    fn combine_precedence() {
        assert!(matches!(
            combine(vec![Outcome::Allow("a".into()), Outcome::Block("b".into())]),
            Outcome::Block(_)
        ));
        assert!(matches!(
            combine(vec![
                Outcome::Allow("a".into()),
                Outcome::Undetermined(vec!["u".into()])
            ]),
            Outcome::Undetermined(_)
        ));
        assert_eq!(combine(vec![]), Outcome::AllowSilent);
    }

    fn und() -> Outcome {
        Outcome::Undetermined(vec!["x".into()])
    }

    #[test]
    fn resolve_determined_clears_marker() {
        let (f, op) = resolve(Outcome::Block("b".into()), Some(true), &Marker::Present);
        assert_eq!((f, op), (Final::Block("b".into()), MarkerOp::Clear));
        let (f, op) = resolve(Outcome::AllowSilent, Some(false), &Marker::Present);
        assert_eq!((f, op), (Final::Silent, MarkerOp::Clear));
    }

    #[test]
    fn resolve_undetermined_first_stop_blocks_and_writes() {
        let (f, op) = resolve(und(), Some(false), &Marker::Present);
        assert!(matches!(f, Final::Block(r) if r.contains("determine")));
        assert_eq!(op, MarkerOp::Write);
    }

    #[test]
    fn resolve_undetermined_reentry_with_marker_allows_loudly() {
        let (f, op) = resolve(und(), Some(true), &Marker::Present);
        assert!(matches!(f, Final::SystemMessage(_)));
        assert_eq!(op, MarkerOp::Clear);
    }

    #[test]
    fn resolve_undetermined_reentry_without_marker_blocks() {
        let (f, op) = resolve(und(), Some(true), &Marker::Absent);
        assert!(matches!(f, Final::Block(_)));
        assert_eq!(op, MarkerOp::Write);
    }

    #[test]
    fn resolve_undetermined_reentry_unusable_marker_allows() {
        let (f, _) = resolve(und(), Some(true), &Marker::Unusable("no id".into()));
        assert!(matches!(f, Final::SystemMessage(m) if m.contains("no id")));
    }

    #[test]
    fn resolve_unknown_stop_hook_active_alternates_on_marker() {
        assert!(matches!(
            resolve(und(), None, &Marker::Absent).0,
            Final::Block(_)
        ));
        assert!(matches!(
            resolve(und(), None, &Marker::Present).0,
            Final::SystemMessage(_)
        ));
    }

    #[test]
    fn resolve_unknown_stop_hook_active_unusable_marker_allows() {
        assert!(matches!(
            resolve(und(), None, &Marker::Unusable("u".into())).0,
            Final::SystemMessage(_)
        ));
    }

    #[test]
    fn write_failure_on_reentry_turns_block_into_allow() {
        let b = Final::Block("r".into());
        assert_eq!(on_write_failure(b.clone(), Some(false), "e"), b);
        assert!(matches!(
            on_write_failure(b.clone(), Some(true), "e"),
            Final::SystemMessage(_)
        ));
        assert!(matches!(
            on_write_failure(b, None, "e"),
            Final::SystemMessage(_)
        ));
    }
}
