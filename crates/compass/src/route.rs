//! route (DESIGN §13) — the deterministic size triage (B-plan = focus
//! protection) over an already-produced condukt Decomposition.
//!
//! # Architecture constraint
//!
//! A Rust binary cannot call an LLM, so the DECOMPOSITION into tasks is
//! condukt-interpreter's job (a SKILL) — NOT here. This module CONSUMES a
//! decomposition JSON and triages it deterministically by `size`. The only
//! "centrality" judgment is a deterministic graph proxy (in-degree), never a
//! semantic one.

use serde::{Deserialize, Serialize};

use crate::charter::Charter;
use crate::config::Config;
use crate::opportunity::Opportunity;

/// One decomposition task. Mirrors condukt's schema (DESIGN §6) with the new
/// optional `size`; deserializes the JSON condukt-interpreter emits (with or
/// without `size`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub touched_files: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub suggested_model: Option<String>,
    #[serde(default)]
    pub done_criteria: String,
    /// `xs|s|m|l|xl`; optional — missing means "size unknown → needs attention".
    #[serde(default)]
    pub size: Option<String>,
}

/// A condukt Decomposition (DESIGN §6): a goal plus its tasks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Decomposition {
    pub goal: String,
    #[serde(default)]
    pub tasks: Vec<Task>,
}

/// A right-size-0 edge state (DESIGN §13): no task was right-sized.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "message")]
pub enum RouteEdge {
    /// All tasks are l/xl: the goal is too big for a right-sized move.
    GoalTooBig(String),
    /// All tasks are xs: only noise; the north_star may be exhausted.
    OnlyNoise(String),
}

/// The triage result. `to_condukt` is the minimal coupled set to act on now;
/// `parked` is everything held back; `edge` is set only in the right-size-0
/// states.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Routing {
    pub to_condukt: Vec<Task>,
    pub parked: Vec<Task>,
    pub edge: Option<RouteEdge>,
}

/// Whether a `size` value is one of the configured right sizes (default s/m).
fn is_right_size(size: &Option<String>, cfg: &Config) -> bool {
    match size {
        Some(s) => cfg.routing.right_size.iter().any(|r| r == s),
        None => false,
    }
}

/// Whether a `size` value is a recognized rubric size (DESIGN §13).
fn is_known_size(size: &Option<String>) -> bool {
    matches!(
        size.as_deref(),
        Some("xs") | Some("s") | Some("m") | Some("l") | Some("xl")
    )
}

/// Deterministic size triage (DESIGN §13). Selects the single most-central
/// right-sized move (plus its right-sized, coupled dependency closure) for
/// condukt, parks the rest, and sets `edge` in the right-size-0 states.
pub fn route(dec: &Decomposition, cfg: &Config) -> Routing {
    let tasks = &dec.tasks;

    // Unsized / unknown-size tasks are "needs attention": never silently parked.
    // They go to condukt as flagged candidates regardless of selection.
    let needs_attention: Vec<&Task> = tasks.iter().filter(|t| !is_known_size(&t.size)).collect();

    // Right-sized candidates (known size ∈ cfg.routing.right_size).
    let right_sized: Vec<&Task> = tasks
        .iter()
        .filter(|t| is_known_size(&t.size) && is_right_size(&t.size, cfg))
        .collect();

    // --- right-size-0 edge states (DESIGN §13) ---
    if right_sized.is_empty() && needs_attention.is_empty() {
        if let Some(edge) = right_size_zero_edge(tasks) {
            // Everything is parked; the edge tells the skill how to re-carve.
            return Routing {
                to_condukt: Vec::new(),
                parked: tasks.clone(),
                edge: Some(edge),
            };
        }
    }

    // --- select the single most-central right-sized move ---
    let chosen: Option<&Task> = right_sized
        .iter()
        .copied()
        .max_by(|a, b| centrality_cmp(a, b, tasks));

    // Minimal coupled set = chosen + its right-sized, non-`parallel` dep closure.
    let mut selected_ids: Vec<String> = Vec::new();
    if let Some(chosen) = chosen {
        collect_coupled_set(chosen, tasks, cfg, &mut selected_ids);
    }
    // Plus all needs-attention tasks (flagged, never silently parked).
    for t in &needs_attention {
        if !selected_ids.contains(&t.id) {
            selected_ids.push(t.id.clone());
        }
    }

    // Partition tasks preserving input order; selected → condukt, rest → parked.
    let mut to_condukt = Vec::new();
    let mut parked = Vec::new();
    for t in tasks {
        if selected_ids.contains(&t.id) {
            to_condukt.push(t.clone());
        } else {
            parked.push(t.clone());
        }
    }

    Routing {
        to_condukt,
        parked,
        edge: None,
    }
}

/// Deterministic centrality proxy (DESIGN §13). Higher is "more central":
///   1. higher in-degree (more OTHER tasks depend on this task via `deps`),
///   2. tie-break: fewer own `deps`,
///   3. tie-break: earlier in the input order.
///
/// Returns an [`Ordering`] suitable for `max_by` (so "greater" = more central).
fn centrality_cmp(a: &Task, b: &Task, tasks: &[Task]) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let a_in = in_degree(&a.id, tasks);
    let b_in = in_degree(&b.id, tasks);
    // 1. higher in-degree wins.
    match a_in.cmp(&b_in) {
        Ordering::Equal => {}
        other => return other,
    }
    // 2. fewer own deps wins → reverse so "fewer" maps to "greater".
    match a.deps.len().cmp(&b.deps.len()) {
        Ordering::Equal => {}
        other => return other.reverse(),
    }
    // 3. earlier input order wins → reverse index comparison.
    let a_idx = tasks
        .iter()
        .position(|t| t.id == a.id)
        .unwrap_or(usize::MAX);
    let b_idx = tasks
        .iter()
        .position(|t| t.id == b.id)
        .unwrap_or(usize::MAX);
    a_idx.cmp(&b_idx).reverse()
}

/// How many OTHER tasks list `id` in their `deps`.
fn in_degree(id: &str, tasks: &[Task]) -> usize {
    tasks
        .iter()
        .filter(|t| t.id != id)
        .filter(|t| t.deps.iter().any(|d| d == id))
        .count()
}

/// Build the minimal coupled set: the chosen task plus the transitive closure of
/// its `deps` that are themselves right-sized AND coupled (class != "parallel",
/// i.e. not independently parallelizable). Parallel-independent deps are left to
/// be parked rather than dragged in, keeping the move focused (DESIGN §13).
fn collect_coupled_set(chosen: &Task, tasks: &[Task], cfg: &Config, acc: &mut Vec<String>) {
    if acc.contains(&chosen.id) {
        return;
    }
    acc.push(chosen.id.clone());
    for dep_id in &chosen.deps {
        let Some(dep) = tasks.iter().find(|t| &t.id == dep_id) else {
            continue;
        };
        // Only pull in right-sized, coupled (non-parallel) dependencies.
        if is_right_size(&dep.size, cfg) && dep.class != "parallel" {
            collect_coupled_set(dep, tasks, cfg, acc);
        }
    }
}

/// Decide the right-size-0 edge (DESIGN §13). Called only when NO task is
/// right-sized and there are no unsized tasks. Classifies by the size mix:
///   - all l/xl → [`RouteEdge::GoalTooBig`],
///   - all xs   → [`RouteEdge::OnlyNoise`],
///   - mixed but none right-sized → pick the closer of the two by majority.
fn right_size_zero_edge(tasks: &[Task]) -> Option<RouteEdge> {
    if tasks.is_empty() {
        return None;
    }
    let xs = tasks
        .iter()
        .filter(|t| t.size.as_deref() == Some("xs"))
        .count();
    let big = tasks
        .iter()
        .filter(|t| matches!(t.size.as_deref(), Some("l") | Some("xl")))
        .count();

    let goal_too_big = RouteEdge::GoalTooBig(
        "no right-sized move: the goal is too big — re-carve it smaller, prefer a \
         validation slice (DESIGN §7)."
            .to_string(),
    );
    let only_noise = RouteEdge::OnlyNoise(
        "no right-sized move: only xs noise — question the north_star; the direction \
         may be exhausted."
            .to_string(),
    );

    // All l/xl, or mixed-but-majority-big → GoalTooBig.
    // All xs, or mixed-but-majority-xs → OnlyNoise. Ties default to GoalTooBig
    // (re-carving smaller is the safer move than abandoning direction).
    if big == 0 && xs > 0 {
        Some(only_noise)
    } else if xs == 0 && big > 0 {
        Some(goal_too_big)
    } else if xs > big {
        Some(only_noise)
    } else {
        Some(goal_too_big)
    }
}

/// Produce the 課題 statement to hand to condukt (DESIGN §13): the chosen
/// move(s) + project context (north_star / current_gap / measuring_stick) +
/// the named opportunities (PDO OST) sitting under the active outcome, so the
/// solution handed to condukt carries the named opportunity refs it serves.
/// Plain markdown; condukt-interpreter re-decomposes from this.
pub fn condukt_handoff(
    chosen: &[Task],
    charter: &Charter,
    dec_goal: &str,
    opportunities: &[Opportunity],
) -> String {
    let mut out = String::new();
    out.push_str("# 課題（compass → condukt 受け渡し）\n\n");

    out.push_str("## goal\n");
    out.push_str(dec_goal.trim());
    out.push_str("\n\n");

    out.push_str("## 今コミットする一手\n");
    if chosen.is_empty() {
        out.push_str("(右サイズの一手なし — ゴールを彫り直す)\n");
    } else {
        for t in chosen {
            out.push_str(&format!("- [{}] {}", t.id, t.title.trim()));
            if let Some(size) = &t.size {
                out.push_str(&format!(" (size={size})"));
            }
            out.push('\n');
            if !t.done_criteria.trim().is_empty() {
                out.push_str(&format!("  - done: {}\n", t.done_criteria.trim()));
            }
        }
    }
    out.push('\n');

    out.push_str("## 文脈\n");
    out.push_str(&format!("- north_star: {}\n", charter.north_star.trim()));
    out.push_str(&format!("- current_gap: {}\n", charter.current_gap.trim()));
    // measuring_stick is written into the handoff here AND is now read back by
    // the `compass outcome` path (§7): once the move completes, its judged
    // outcome is recorded against this same stick and surfaced in `compass gap`.
    out.push_str(&format!(
        "- measuring_stick: {}\n",
        charter.measuring_stick.trim()
    ));

    // 機会 (PDO OST): the named opportunities under the active outcome. Printing
    // them here is what makes the handed-off solution "carry" its named
    // opportunity refs (charter DoD#2) — the interpreter sees which bet(s) under
    // the outcome this move serves.
    out.push_str("\n## 機会（opportunity / この outcome 配下）\n");
    if opportunities.is_empty() {
        out.push_str("(この outcome 配下に登録された opportunity なし)\n");
    } else {
        for o in opportunities {
            out.push_str(&format!("- [{}] {}\n", o.id, o.title.trim()));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, deps: &[&str], size: Option<&str>) -> Task {
        Task {
            id: id.to_string(),
            title: format!("title-{id}"),
            touched_files: Vec::new(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            class: String::new(),
            suggested_model: None,
            done_criteria: String::new(),
            size: size.map(|s| s.to_string()),
        }
    }

    fn dec(tasks: Vec<Task>) -> Decomposition {
        Decomposition {
            goal: "ship the slice".to_string(),
            tasks,
        }
    }

    #[test]
    fn picks_highest_in_degree_right_sized() {
        // t1 has in-degree 2 (t2,t3 depend on it); t2,t3 in-degree 0.
        let d = dec(vec![
            task("t1", &[], Some("s")),
            task("t2", &["t1"], Some("m")),
            task("t3", &["t1"], Some("m")),
        ]);
        let r = route(&d, &Config::default());
        // Only the chosen central move (t1) goes to condukt; its dependents are
        // NOT pulled in (closure follows deps, not dependents). Siblings parked.
        assert_eq!(
            r.to_condukt
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["t1"]
        );
        assert_eq!(
            r.parked.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["t2", "t3"]
        );
        assert!(r.edge.is_none());
    }

    #[test]
    fn pulls_in_right_sized_coupled_dep_closure() {
        // t_top is most-central: two dependents (d1,d2) give it in-degree 2, beating
        // its own right-sized serial dep t_dep (in-degree 1). The closure pulls the
        // right-sized serial dep t_dep along with the chosen t_top.
        let mut t_dep = task("t_dep", &[], Some("s"));
        t_dep.class = "serial".to_string();
        let mut t_top = task("t_top", &["t_dep"], Some("m"));
        t_top.class = "serial".to_string();
        let d1 = task("d1", &["t_top"], Some("l")); // depends on t_top; itself parked (l)
        let d2 = task("d2", &["t_top"], Some("l"));
        let d = dec(vec![t_dep, t_top, d1, d2]);
        let r = route(&d, &Config::default());
        let to: Vec<&str> = r.to_condukt.iter().map(|t| t.id.as_str()).collect();
        assert!(to.contains(&"t_top"), "chosen central task (in-degree 2)");
        assert!(to.contains(&"t_dep"), "right-sized serial dep pulled in");
        // dependents are NOT pulled in (closure follows deps, not dependents).
        assert!(!to.contains(&"d1"));
        assert!(!to.contains(&"d2"));
        let parked: Vec<&str> = r.parked.iter().map(|t| t.id.as_str()).collect();
        assert!(parked.contains(&"d1"));
        assert!(parked.contains(&"d2"));
    }

    #[test]
    fn parallel_dep_not_dragged_in() {
        // t_top is chosen (in-degree 2 via d1,d2) and depends on a parallel
        // (independent) dep t_par → t_par is NOT dragged in; it is parked.
        let mut t_par = task("t_par", &[], Some("s"));
        t_par.class = "parallel".to_string();
        let mut t_top = task("t_top", &["t_par"], Some("m"));
        t_top.class = "serial".to_string();
        let d1 = task("d1", &["t_top"], Some("l"));
        let d2 = task("d2", &["t_top"], Some("l"));
        let d = dec(vec![t_par, t_top, d1, d2]);
        let r = route(&d, &Config::default());
        let to: Vec<&str> = r.to_condukt.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(to, vec!["t_top"]);
        let parked: Vec<&str> = r.parked.iter().map(|t| t.id.as_str()).collect();
        assert!(parked.contains(&"t_par"), "parallel dep not dragged in");
    }

    #[test]
    fn xs_and_big_all_parked() {
        let d = dec(vec![
            task("a", &[], Some("xs")),
            task("b", &[], Some("l")),
            task("c", &[], Some("xl")),
        ]);
        let r = route(&d, &Config::default());
        assert!(r.to_condukt.is_empty());
        assert_eq!(r.parked.len(), 3);
        // mixed xs + l/xl, majority big → GoalTooBig.
        assert!(matches!(r.edge, Some(RouteEdge::GoalTooBig(_))));
    }

    #[test]
    fn goal_too_big_when_all_l_xl() {
        let d = dec(vec![task("a", &[], Some("l")), task("b", &[], Some("xl"))]);
        let r = route(&d, &Config::default());
        assert!(r.to_condukt.is_empty());
        assert!(matches!(r.edge, Some(RouteEdge::GoalTooBig(_))));
    }

    #[test]
    fn only_noise_when_all_xs() {
        let d = dec(vec![task("a", &[], Some("xs")), task("b", &[], Some("xs"))]);
        let r = route(&d, &Config::default());
        assert!(r.to_condukt.is_empty());
        assert!(matches!(r.edge, Some(RouteEdge::OnlyNoise(_))));
    }

    #[test]
    fn unsized_task_not_silently_parked() {
        // No right-sized task; one unsized → it must surface in to_condukt flagged,
        // and there must be NO right-size-0 edge (unsized ≠ empty).
        let d = dec(vec![task("a", &[], Some("l")), task("u", &[], None)]);
        let r = route(&d, &Config::default());
        assert!(r.to_condukt.iter().any(|t| t.id == "u"));
        assert!(r.edge.is_none());
        assert!(r.parked.iter().any(|t| t.id == "a"));
    }

    #[test]
    fn deserialize_with_and_without_size() {
        // WITH size.
        let with = r#"{
            "goal": "g",
            "tasks": [
                {"id":"t1","title":"a","touched_files":["x.rs"],"deps":[],
                 "class":"serial","suggested_model":"sonnet","done_criteria":"d","size":"m"}
            ]
        }"#;
        let d: Decomposition = serde_json::from_str(with).expect("with size");
        assert_eq!(d.tasks[0].size.as_deref(), Some("m"));

        // WITHOUT size (and several optional fields omitted).
        let without = r#"{
            "goal": "g",
            "tasks": [
                {"id":"t1","title":"a","deps":[],"class":"serial","done_criteria":"d"}
            ]
        }"#;
        let d2: Decomposition = serde_json::from_str(without).expect("without size");
        assert_eq!(d2.tasks[0].size, None);
        assert!(d2.tasks[0].touched_files.is_empty());
        assert_eq!(d2.tasks[0].suggested_model, None);

        // round-trip the WITH form serializes back losslessly for size.
        let json = serde_json::to_string(&d).expect("serialize");
        let d3: Decomposition = serde_json::from_str(&json).expect("reparse");
        assert_eq!(d, d3);
    }

    #[test]
    fn centrality_tiebreak_fewer_deps_then_order() {
        // a and b both in-degree 0 and both right-sized; a has fewer deps → chosen.
        let d = dec(vec![
            task("dep", &[], Some("xs")), // noise, parked
            task("a", &[], Some("s")),
            task("b", &["a"], Some("s")), // a now has in-degree 1; b in-degree 0
        ]);
        // a in-degree 1 (b depends on it) beats b in-degree 0 → a chosen.
        let r = route(&d, &Config::default());
        assert!(r.to_condukt.iter().any(|t| t.id == "a"));
        assert!(r.parked.iter().any(|t| t.id == "b"));
        assert!(r.parked.iter().any(|t| t.id == "dep"));
    }

    fn opp(id: &str, title: &str) -> Opportunity {
        Opportunity {
            id: id.to_string(),
            title: title.to_string(),
            outcome_ref: "active".to_string(),
            weight: crate::opportunity::DEFAULT_WEIGHT,
            created_at: 0,
        }
    }

    #[test]
    fn handoff_carries_opportunities() {
        let charter = Charter {
            north_star: "ship OST".to_string(),
            ..Charter::default()
        };
        let chosen = vec![task("t1", &[], Some("s"))];
        let opps = vec![
            opp(
                "users-cant-see-why-ab12cd",
                "users can't see why a move was chosen",
            ),
            opp(
                "flat-list-grouping-34ef56",
                "opportunities can't be grouped",
            ),
        ];

        let out = condukt_handoff(&chosen, &charter, "ship the slice", &opps);

        // the opportunity section is present and carries each id + title.
        assert!(out.contains("## 機会"));
        assert!(out.contains("[users-cant-see-why-ab12cd]"));
        assert!(out.contains("users can't see why a move was chosen"));
        assert!(out.contains("[flat-list-grouping-34ef56]"));
    }

    #[test]
    fn handoff_notes_when_no_opportunities() {
        let out = condukt_handoff(&[], &Charter::default(), "g", &[]);
        assert!(out.contains("## 機会"));
        assert!(out.contains("登録された opportunity なし"));
    }
}
