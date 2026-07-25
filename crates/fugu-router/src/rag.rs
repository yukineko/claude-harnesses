//! Lexical k-NN over episodes. Subscription-native: no embedding service, so
//! similarity is a token/file-overlap Jaccard. Crude but dependency-free and
//! good enough to bias model selection toward what worked on similar tasks.

use std::collections::BTreeSet;

use crate::store::Episode;

const STOP: &[&str] = &[
    "the", "a", "an", "to", "of", "and", "or", "for", "in", "on", "with", "add", "update", "fix",
    "make", "use", "via", "into", "from", "that", "this", "be", "is", "are", "new",
];

/// Normalise free text into a token set: lowercased, alnum, stopwords dropped,
/// then stemmed + concept-expanded (see `semantic`) so synonyms overlap.
pub fn tokenize(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for t in s.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        if t.len() >= 3 && !STOP.contains(&t) {
            crate::semantic::expand_into(&mut out, t);
        }
    }
    out
}

/// Generic path segments / extensions that carry no "what kind of work" signal.
/// Dropping these keeps `src/auth/login.ts` vs `src/billing/report.ts` from
/// looking similar just because they share `src` and `ts`.
const FILE_STOP: &[&str] = &[
    "src",
    "lib",
    "app",
    "pkg",
    "internal",
    "cmd",
    "test",
    "tests",
    "spec",
    "dist",
    "build",
    "target",
    "node_modules",
    "index",
    "mod",
    "main",
    "crates",
    "ts",
    "tsx",
    "js",
    "jsx",
    "py",
    "rs",
    "go",
    "rb",
    "java",
    "c",
    "cpp",
    "h",
    "hpp",
    "md",
    "json",
    "toml",
    "yaml",
    "yml",
    "txt",
];

/// Path-derived tokens: meaningful path segments, so `src/auth/login.ts`
/// contributes `auth`, `login` (generic `src`/`ts` are dropped). File overlap is
/// the strongest "same kind of work" signal, kept distinct from title tokens.
pub fn file_tokens(files: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in files {
        for seg in f.to_lowercase().split(['/', '\\', '.']) {
            let seg = seg.trim_matches('*');
            if seg.len() >= 2 && !FILE_STOP.contains(&seg) {
                crate::semantic::expand_into(&mut out, seg);
            }
        }
    }
    out
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        a.intersection(b).count() as f64 / union
    }
}

/// Similarity of a query task to an episode. Files weigh as much as title text
/// when both sides have files; otherwise title tokens carry the whole score.
pub fn similarity(
    q_tok: &BTreeSet<String>,
    q_files: &BTreeSet<String>,
    e_tok: &BTreeSet<String>,
    e_files: &BTreeSet<String>,
) -> f64 {
    let tok = jaccard(q_tok, e_tok);
    if q_files.is_empty() || e_files.is_empty() {
        tok
    } else {
        0.5 * tok + 0.5 * jaccard(q_files, e_files)
    }
}

pub struct Neighbor {
    pub ep: Episode,
    pub sim: f64,
}

/// Top-k episodes by similarity at/above `threshold`, highest first.
pub fn knn(
    title: &str,
    files: &[String],
    episodes: &[Episode],
    k: usize,
    threshold: f64,
) -> Vec<Neighbor> {
    let q_tok = tokenize(title);
    let q_files = file_tokens(files);
    let mut scored: Vec<Neighbor> = episodes
        .iter()
        .map(|e| {
            let e_tok = tokenize(&e.title);
            let e_files = file_tokens(&e.touched_files);
            Neighbor {
                ep: e.clone(),
                sim: similarity(&q_tok, &q_files, &e_tok, &e_files),
            }
        })
        .filter(|n| n.sim >= threshold)
        .collect();
    scored.sort_by(|a, b| {
        b.sim
            .partial_cmp(&a.sim)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(k);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(title: &str, files: &[&str], model: &str, pass: bool) -> Episode {
        Episode {
            ts: 0,
            title: title.into(),
            touched_files: files.iter().map(|s| s.to_string()).collect(),
            class: "parallel".into(),
            model: model.into(),
            role: "worker".into(),
            pass,
            cost_usd: 0.0,
            human_label: None,
            labeled_by: None,
            skill_fingerprint: None,
            duration_secs: None,
            delegation: None,
            ..Default::default()
        }
    }

    #[test]
    fn tokenize_drops_stopwords_and_short() {
        let t = tokenize("Add the login endpoint to API");
        assert!(t.contains("login"));
        assert!(t.contains("endpoint"));
        assert!(!t.contains("add")); // stopword
        assert!(!t.contains("to")); // short + stopword
    }

    #[test]
    fn shared_files_raise_similarity() {
        let episodes = vec![
            ep("rework login handler", &["src/auth/login.ts"], "opus", true),
            ep(
                "update billing report",
                &["src/billing/report.ts"],
                "haiku",
                true,
            ),
        ];
        let q = vec!["src/auth/login.ts".to_string()];
        let nb = knn("fix login validation", &q, &episodes, 5, 0.0);
        // the auth/login episode must rank first
        assert_eq!(nb[0].ep.model, "opus");
        assert!(nb[0].sim > nb[1].sim);
    }

    #[test]
    fn concept_bridges_synonyms_without_shared_words() {
        // "login flow" and "authentication token" share no surface word, but
        // both map to the ~auth concept, so the auth episode must rank first.
        let episodes = vec![
            ep("refresh authentication token", &[], "sonnet", true),
            ep("update billing total", &[], "haiku", true),
        ];
        let nb = knn("fix the login flow", &[], &episodes, 5, 0.0);
        assert_eq!(nb[0].ep.model, "sonnet");
        assert!(nb[0].sim > 0.0);
    }

    #[test]
    fn threshold_filters_unrelated() {
        let episodes = vec![ep(
            "update billing report",
            &["src/billing/report.ts"],
            "haiku",
            true,
        )];
        let q = vec!["src/auth/oauth.ts".to_string()];
        let nb = knn("design auth protocol", &q, &episodes, 5, 0.15);
        assert!(nb.is_empty());
    }
}
