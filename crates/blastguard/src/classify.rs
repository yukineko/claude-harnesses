//! Graded risk / reversibility classifier for a *planned* action.
//!
//! [`crate::detect`] answers a binary allow/deny for a single tool call. The
//! autonomy policy engine needs finer axes: *how risky* is an action and *is it
//! reversible*. This module supplies them as a pure, deterministic function over
//! the action's free text (a shell command, or a task's title + done-criteria +
//! touched files) so callers — e.g. condukt's scheduler — can force a high-risk
//! irreversible action (a deploy / `git push` / release) through the outward
//! GATED gate even when an upstream LLM mislabelled it. Pure (no I/O).

use serde::Serialize;
use serde_json::json;

use crate::detect;
use crate::diffrisk::{self, SensitiveConfig};

/// How much blast radius an action carries. Serialises lowercase
/// (`"low"`/`"medium"`/`"high"`) so callers can emit `{risk, reversible}`.
///
/// Ordered `Low < Medium < High` so callers (e.g. [`classify_change`]) can
/// merge multiple risk signals by taking the max tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Medium,
    High,
}

/// The graded verdict for a planned action: its risk tier and whether it can be
/// undone. Supplies the two axes the policy engine reasons over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RiskAssessment {
    pub risk: Risk,
    pub reversible: bool,
}

impl RiskAssessment {
    /// The deterministic predicate the condukt scheduler uses to force an action
    /// through the outward-facing GATED gate: **high risk AND not reversible**.
    /// A mis-tagged deploy still trips this, which is the whole point.
    pub fn requires_gate(&self) -> bool {
        matches!(self.risk, Risk::High) && !self.reversible
    }

    /// Merge two assessments of the same planned action into one: the higher
    /// risk tier wins, and the result is reversible only if BOTH inputs are
    /// (an irreversible signal from either source must not be diluted away).
    /// This is how command-text classification and diff-level semantic
    /// classification ride the same [`RiskAssessment`]/`requires_gate` axis
    /// instead of two parallel paths.
    pub fn merge(self, other: RiskAssessment) -> RiskAssessment {
        RiskAssessment {
            risk: self.risk.max(other.risk),
            reversible: self.reversible && other.reversible,
        }
    }
}

/// True when `needle` occurs in `hay` bounded by a non-alphanumeric char (or the
/// string edge) on both sides — so `"ship"` matches "ship it" / "will ship" but
/// NOT "ownership"/"relationship"/"membership", and `"origin"` matches "to
/// origin" but not "originally". Both args must already be lowercase. Multi-word
/// needles are bounded only at the phrase ends. This is what makes the bare
/// deploy verbs/targets safe: naive substring `contains` re-introduces exactly
/// the "reproduction" ⊃ "prod" / "ownership" ⊃ "ship" false-positive class.
fn has_word(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hb = hay.as_bytes();
    hay.match_indices(needle).any(|(start, _)| {
        let end = start + needle.len();
        let before_ok = start == 0 || !hb[start - 1].is_ascii_alphanumeric();
        let after_ok = end == hb.len() || !hb[end].is_ascii_alphanumeric();
        before_ok && after_ok
    })
}

/// Tier 1 — outward-facing publish / deploy signals that gate *unconditionally*,
/// matched as plain lowercase substrings. Every entry is either a bare token
/// whose substring only ever appears in deploy-family words (`"deploy"` ⊂
/// deploy/deployment/redeploy; `"rollout"`), a distinctive tool/scheme
/// (`"vercel"`, `"s3://"`, `"gh release"`), or a contiguous git/registry phrase
/// (`"git push"`, `"publish package"`).
///
/// Deliberately EXCLUDED from tier 1 (they substring-match benign prose and are
/// handled by the word-boundary tier-2 rule instead): bare `ship`/`push`/
/// `release`/`promote` and the traps `"ship to"` (owner**ship to**), `"to
/// origin"` (transform **to origin**), `"new release"` (the **new release** of
/// tokio), `"release to"` (**release to**ggle), `"promote the"` (**promote the**
/// local var). PaaS brand names that recur in docs are scoped to a deploy verb
/// (`"heroku deploy"`, not bare `"heroku"`).
///
/// Over-gating is the *safe* direction for an outward gate (a false gate only
/// asks a human), but needless gating of ordinary tasks trains humans to
/// rubber-stamp the gate, so the list balances recall against that.
const DEPLOY_SIGNALS: &[&str] = &[
    // generic deploy (bare — substring only in deploy-family words)
    "deploy",
    "rollout",
    "roll out",
    "go live",
    "going live",
    // infra / orchestration apply
    "kubectl apply",
    "terraform apply",
    "terraform destroy",
    "helm upgrade",
    "helm install",
    "ansible-playbook",
    "docker-compose up",
    "docker compose up",
    // package / registry publish (registry-qualified; bare "publish" is tier 2)
    "docker push",
    "npm publish",
    "cargo publish",
    "twine upload",
    "gem push",
    "mvn deploy",
    "publish package",
    "publish the package",
    "publish to",
    // git push (contiguous git-specific phrasings)
    "git push",
    "push origin",
    "push upstream",
    "push to origin",
    "push to remote",
    "push the tag",
    "push a tag",
    "push new tag",
    "push tags",
    "push --force",
    "force-push",
    "force push",
    // artifact / host copy deploys
    "s3 sync",
    "s3 cp",
    "s3://",
    "scp ",
    "rsync ",
    "sftp ",
    // PaaS / CLI deploy tools (brand names scoped to a deploy verb where they
    // otherwise appear in prose)
    "vercel",
    "netlify",
    "flyctl",
    "fly deploy",
    "aws deploy",
    "gcloud app deploy",
    "gcloud run deploy",
    "heroku deploy",
    "heroku container",
    "push heroku",
    // store submission
    "app store",
    "google play",
    // release (contiguous, distinctive phrasings only)
    "gh release",
    "cut a release",
    "cut the release",
    "tag and release",
    // promote to an environment (distinctive)
    "promote build",
];

/// Tier 2 — a deploy *verb* co-occurring with an outward *target* also gates.
/// Both matched at word boundaries via [`has_word`] so a bare verb (`push onto
/// the vec`) or a trap word (`reproduction`, `originally`) never fires. Catches
/// reordered phrasings tier 1 misses: "release the build to prod", "push the fix
/// to the registry", "ship the update to customers".
const DEPLOY_VERBS: &[&str] = &["release", "publish", "promote", "deploy", "push", "ship"];
const OUTWARD_TARGETS: &[&str] = &[
    "prod",
    "production",
    "origin",
    "remote",
    "registry",
    "customers",
    "upstream",
];

/// Local history-rewrite signals: recoverable via reflog, but risky enough to
/// flag as Medium (reversible). Kept distinct from the destructive denies that
/// [`detect`] already catches (`git reset --hard`, `git clean -fdx`, ...).
const HISTORY_SIGNALS: &[&str] = &["git rebase", "rebase -i", "--amend", "filter-branch"];

/// Classify a free-text action description into `{risk, reversible}`.
///
/// Precedence (first match wins):
///   1. outward deploy/publish/push  → High, irreversible
///   2. locally destructive (whatever [`detect`] would DENY as a Bash command)
///      → High, irreversible
///   3. local history rewrite        → Medium, reversible
///   4. everything else              → Low, reversible
pub fn classify(text: &str) -> RiskAssessment {
    let lower = text.to_ascii_lowercase();

    // 1a. Tier-1 outward publish/deploy signal — leaves the machine, hard to undo.
    // 1b. Tier-2: a deploy verb co-occurring with an outward target, both matched
    //     at word boundaries so bare verbs / trap words never fire.
    let tier1 = DEPLOY_SIGNALS.iter().any(|s| lower.contains(s));
    let tier2 = DEPLOY_VERBS.iter().any(|v| has_word(&lower, v))
        && OUTWARD_TARGETS.iter().any(|t| has_word(&lower, t));
    if tier1 || tier2 {
        return RiskAssessment {
            risk: Risk::High,
            reversible: false,
        };
    }

    // 2. Locally destructive — reuse the binary detector so the two stay in
    //    lockstep. Anything blastguard would DENY is irreversible data loss.
    if detect::detect("Bash", Some(&json!({ "command": text }))).is_deny() {
        return RiskAssessment {
            risk: Risk::High,
            reversible: false,
        };
    }

    // 3. History rewrite — recoverable via reflog, but risky.
    if HISTORY_SIGNALS.iter().any(|s| lower.contains(s)) {
        return RiskAssessment {
            risk: Risk::Medium,
            reversible: true,
        };
    }

    // 4. Default — ordinary, reversible work.
    RiskAssessment {
        risk: Risk::Low,
        reversible: true,
    }
}

/// Generalized classification: merge command-text risk ([`classify`]) with
/// diff-level semantic risk — configurable sensitive-path globs and
/// public/exported-symbol changes (see [`crate::diffrisk`]) — into ONE
/// [`RiskAssessment`]. Both signals ride the same `requires_gate` escalation
/// axis, so a change that touches an auth/payment/PII path or an exported API
/// surface is force-gated exactly like a mislabelled `git push`, without
/// callers needing a second parallel risk path.
///
/// condukt's gate wires this in at two sites — [`crate`]'s consumer is
/// condukt's `schedule::schedule` force-gate and `gate_exec::gather_assessment`
/// (both `crates/condukt/src`) — but only for the **sensitive-path** signal:
/// neither site has a diff at the time it classifies (schedule-time /
/// pre-execution), so `diff_text` is passed empty and the public-symbol-diff
/// signal does not fire there. A future diff-aware call site (e.g. post-
/// implementation, pre-merge) would additionally light up the public-symbol
/// signal by passing real `diff_text`.
///
/// `text` is the free-text action description ([`classify`]'s existing
/// input — title + done-criteria + touched files, unchanged from today).
/// `paths` are the changed files (may overlap with what's embedded in
/// `text`; kept separate so callers with a real diff/file list don't need to
/// serialize it into prose). `diff_text` is optional unified-diff content
/// (empty string if unavailable — the public-symbol signal simply won't
/// fire, matching backward-compatible/additive behavior).
pub fn classify_change(
    text: &str,
    paths: &[String],
    diff_text: &str,
    sensitive: &SensitiveConfig,
) -> RiskAssessment {
    let command = classify(text);
    let diff = diffrisk::classify_diff(paths, diff_text, sensitive);
    command.merge(diff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deploy_and_push_are_high_irreversible_and_gated() {
        for action in [
            "deploy to production",
            "run the deploy script",
            "git push origin main",
            "docker push myimage:latest",
            "cargo publish the crate",
            "npm publish",
            "kubectl apply -f k8s/",
            "terraform apply",
            "rollout the new version",
        ] {
            let a = classify(action);
            assert_eq!(a.risk, Risk::High, "{action:?} must be High risk");
            assert!(!a.reversible, "{action:?} must be irreversible");
            assert!(a.requires_gate(), "{action:?} must require the GATED gate");
        }
    }

    #[test]
    fn realistic_deploy_phrasings_do_not_evade_the_gate() {
        // Regression guard: adversarial verification found these reordered /
        // synonym / CLI-tool deploy phrasings evading a narrower signal list.
        // Every one is an outward action that MUST reach the GATED gate.
        for action in [
            "push the release branch to origin",
            "release v1.2.0 to prod",
            "cut a release and publish it",
            "publish package via npm",
            "publish the package to the registry",
            "aws s3 sync ./dist s3://prod-bucket",
            "gh release create v1.0.0",
            "vercel --prod",
            "scp build/ user@prodserver:/var/www",
            "rsync -av dist/ prod:/var/www/html",
            "promote build 42 to production",
            "ship the update to customers",
            "go live with the new checkout flow",
            "tag and release the new version",
            "push new tag to remote",
            "docker-compose up -d --build production",
            "deploy to production",
            "heroku container:push web",
            "netlify deploy --prod",
            "upload the ipa to the app store",
        ] {
            let a = classify(action);
            assert_eq!(
                a.risk,
                Risk::High,
                "{action:?} must be High risk (must gate)"
            );
            assert!(!a.reversible, "{action:?} must be irreversible");
            assert!(a.requires_gate(), "{action:?} MUST require the GATED gate");
        }
    }

    #[test]
    fn benign_lookalikes_are_not_force_gated() {
        // False-positive guards: these contain deploy-adjacent words but are NOT
        // outward actions. Word-boundary matching (not substring `contains`) must
        // keep them Low despite traps like "ownership"⊃"ship", "originally"⊃
        // "origin", "reproduction"⊃"prod", "release build", "new release of X".
        for action in [
            "cargo build --release",
            "optimize the release build profile",
            "release the mutex lock after the loop",
            "add reproduction tests for the parser bug",
            "push a new field onto the struct",
            "push the value onto the vec",
            "originally the parser used a different approach",
            "publish a blog post about the refactor",
            "refactor the production of the report string",
            // second-round adversarial false positives (word-boundary traps):
            "transfer ownership to the callee",
            "clarify the relationship to the parent node",
            "add membership to the group model",
            "refactor membership then reload the cache",
            "explain the relationship theory in docs",
            "reset the sprite transform to origin",
            "compute distance to origin point",
            "translate the vector to origin",
            "convert the payload to remote representation",
            "upgrade to the new release of tokio",
            "promote the local variable to a field",
            "remove heroku references from the docs",
            "ship the log lines to a buffer",
        ] {
            let a = classify(action);
            assert!(
                !a.requires_gate(),
                "{action:?} must NOT be force-gated, got {a:?}",
            );
        }
    }

    #[test]
    fn locally_destructive_is_high_irreversible() {
        // Anything the binary detector denies is irreversible data loss.
        for action in ["rm -rf build", "git reset --hard HEAD~1", "git clean -fdx"] {
            let a = classify(action);
            assert_eq!(a.risk, Risk::High, "{action:?} must be High risk");
            assert!(!a.reversible, "{action:?} must be irreversible");
            assert!(a.requires_gate());
        }
    }

    #[test]
    fn history_rewrite_is_medium_reversible_and_not_gated() {
        for action in [
            "git rebase -i HEAD~3",
            "git commit --amend",
            "run filter-branch",
        ] {
            let a = classify(action);
            assert_eq!(a.risk, Risk::Medium, "{action:?} must be Medium risk");
            assert!(a.reversible, "{action:?} is recoverable via reflog");
            assert!(!a.requires_gate(), "{action:?} must NOT be force-gated");
        }
    }

    #[test]
    fn ordinary_work_is_low_reversible_and_not_gated() {
        for action in [
            "add a --cost flag to the CLI",
            "refactor the parser",
            "fix a typo in the README",
            "push a new field onto the struct",
        ] {
            let a = classify(action);
            assert_eq!(a.risk, Risk::Low, "{action:?} must be Low risk");
            assert!(a.reversible);
            assert!(!a.requires_gate(), "{action:?} must NOT be force-gated");
        }
    }

    #[test]
    fn has_word_respects_boundaries() {
        assert!(has_word("ship it to prod", "ship"));
        assert!(!has_word("transfer ownership now", "ship"));
        assert!(!has_word("the relationship model", "ship"));
        assert!(has_word("push to origin", "origin"));
        assert!(!has_word("originally planned", "origin"));
        assert!(has_word("deploy to prod", "prod"));
        assert!(!has_word("add reproduction tests", "prod"));
        assert!(has_word("scale to production", "production"));
    }

    #[test]
    fn assessment_serialises_to_lowercase_axes() {
        let json = serde_json::to_value(classify("git push origin main")).unwrap();
        assert_eq!(json["risk"], "high");
        assert_eq!(json["reversible"], false);
    }

    #[test]
    fn merge_takes_higher_risk_tier() {
        let low = RiskAssessment {
            risk: Risk::Low,
            reversible: true,
        };
        let medium = RiskAssessment {
            risk: Risk::Medium,
            reversible: true,
        };
        let merged = low.clone().merge(medium.clone());
        assert_eq!(merged.risk, Risk::Medium);
        assert_eq!(medium.merge(low).risk, Risk::Medium);
    }

    #[test]
    fn merge_reversible_only_if_both_reversible() {
        let reversible = RiskAssessment {
            risk: Risk::Low,
            reversible: true,
        };
        let irreversible = RiskAssessment {
            risk: Risk::Low,
            reversible: false,
        };
        assert!(!reversible.merge(irreversible).reversible);
    }

    #[test]
    fn classify_change_baseline_matches_command_only_classification() {
        // No sensitive path, no public-symbol diff → identical to classify().
        let cfg = SensitiveConfig::default();
        let text = "refactor the parser";
        let a = classify_change(text, &["src/parser.rs".to_string()], "", &cfg);
        assert_eq!(a, classify(text));
    }

    #[test]
    fn classify_change_sensitive_path_raises_risk_over_baseline() {
        let cfg = SensitiveConfig::default();
        let text = "add a login retry limit";
        let baseline = classify(text);
        let a = classify_change(text, &["src/auth/login.rs".to_string()], "", &cfg);
        assert!(a.risk > baseline.risk, "sensitive path must raise risk");
    }

    #[test]
    fn classify_change_public_symbol_raises_risk_over_baseline() {
        let cfg = SensitiveConfig::default();
        let text = "add a helper function";
        let baseline = classify(text);
        let a = classify_change(
            text,
            &["src/lib.rs".to_string()],
            "+pub fn new_helper() {}",
            &cfg,
        );
        assert!(a.risk > baseline.risk, "public API change must raise risk");
    }

    #[test]
    fn classify_change_still_force_gates_deploy_text_regardless_of_diff_signals() {
        // The existing destructive/deploy precedence is untouched: merge only
        // ever raises risk, never masks the pre-existing High+irreversible verdict.
        let cfg = SensitiveConfig::default();
        let a = classify_change("git push origin main", &["README.md".to_string()], "", &cfg);
        assert_eq!(a.risk, Risk::High);
        assert!(!a.reversible);
        assert!(a.requires_gate());
    }
}
