//! Graded risk / reversibility classifier for a *planned* action.
//!
//! [`crate::detect`] answers a binary allow/deny for a single tool call. The
//! autonomy policy engine needs finer axes: *how risky* is an action and *is it
//! reversible*. This module supplies them as pure, deterministic functions over
//! a task's prose (title + done-criteria + touched files: [`classify`]) and,
//! SEPARATELY, over the commands the task will actually run
//! ([`classify_command`] / [`classify_commands`]) — prose is never parsed as a
//! command (backlog dc955812) — so callers — e.g. condukt's scheduler — can force a high-risk
//! irreversible action (a deploy / `git push` / release) through the outward
//! GATED gate even when an upstream LLM mislabelled it. Pure (no I/O).

use harness_core::verdict::Determination;
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

/// File extensions that make a whitespace-delimited token a *file-path token*
/// for [`mask_file_path_tokens`]. A fixed list (not "any `.xyz` suffix") so a
/// version string like `v1.2.0` or a hostname is never mistaken for a file.
const FILE_PATH_EXTENSIONS: &[&str] = &[
    ".py", ".rs", ".sh", ".md", ".toml", ".json", ".yml", ".yaml", ".txt", ".js", ".ts", ".lock",
    ".cfg", ".conf", ".ini",
];

/// Words that, immediately before a path token, put it in *command position*
/// (it is being run, not named). A previous token ending in a shell separator
/// (`;`, `|`, `&`, `(`) does too.
const RUNNER_WORDS: &[&str] = &[
    "run", "runs", "execute", "executes", "invoke", "invokes", "exec", "bash", "sh", "zsh",
    "source", ".", "python", "python3", "node", "sudo", "env", "then", "&&", "||", "|", ";",
];

/// Is `tok` (lowercase, no whitespace, no backtick) a bare file-path token?
///
/// Narrow by design (backlog 11be9484, user ruling 2026-10-16): the token
/// contains a `/` or ends in one of [`FILE_PATH_EXTENSIONS`], AND contains
/// neither `:` nor `@` — so URLs (`s3://bucket`), remote specs
/// (`user@host:/srv`, `prod:/var/www`) and `scheme:` forms stay visible to the
/// deploy needles.
fn is_file_path_token(tok: &str) -> bool {
    if tok.contains(':') || tok.contains('@') {
        return false;
    }
    let trimmed = tok.trim_end_matches([',', '.', ';', ')', '"', '\'']);
    tok.contains('/') || FILE_PATH_EXTENSIONS.iter().any(|e| trimmed.ends_with(e))
}

/// Blank (replace with spaces) every bare file-path token in `lower` that is
/// NOT being invoked, so a deploy needle that occurs only INSIDE a file name
/// (`scripts/check-plugin-rollout.py` contains `rollout`) does not force-gate
/// a task that merely edits that file (backlog 11be9484, file-name half; user
/// ruling 2026-10-16).
///
/// Kept visible (NOT blanked) — each is a restrictive choice:
/// - everything inside a backtick span, and everything after an unbalanced
///   backtick: quoted command prose gates exactly as before this rule;
/// - a path token in command position: the first token of the text or of the
///   text following a backtick span, a token right after a [`RUNNER_WORDS`]
///   word or after a token ending in `;`/`|`/`&`/`(`, and any token starting
///   with `./` — so `scripts/rollout-plugins.sh`, `run scripts/rollout-plugins.sh`
///   and `./deploy.sh` still gate;
/// - tokens that are not file-path tokens per [`is_file_path_token`].
///
/// Only the deploy-needle tiers read the masked text; the [`detect`] step and
/// the history step still see the original text, so a destructive command is
/// judged with its path argument intact.
///
/// Known limit: a script that is meant to be run but is named outside the
/// command positions above (e.g. "run the tests and scripts/rollout.sh",
/// where the previous token is `and`) is blanked like any other file name;
/// quote it in backticks or put it after a runner word to keep it visible.
fn mask_file_path_tokens(lower: &str) -> String {
    let mut out = String::with_capacity(lower.len());
    for (i, seg) in lower.split('`').enumerate() {
        if i > 0 {
            out.push('`');
        }
        // Odd segments are inside a backtick span. A trailing segment after an
        // unbalanced backtick also has an odd index, so it is kept too.
        if i % 2 == 1 {
            out.push_str(seg);
            continue;
        }
        // `None` == start of a segment == command position.
        let mut prev: Option<&str> = None;
        let mut cursor = 0usize;
        while let Some(start_rel) = seg[cursor..].find(|c: char| !c.is_ascii_whitespace()) {
            let start = cursor + start_rel;
            let len = seg[start..]
                .find(|c: char| c.is_ascii_whitespace())
                .unwrap_or(seg.len() - start);
            let tok = &seg[start..start + len];
            out.push_str(&seg[cursor..start]);
            let command_position = match prev {
                None => true,
                Some(p) => RUNNER_WORDS.contains(&p) || p.ends_with([';', '|', '&', '(']),
            };
            let invoked = command_position || tok.starts_with("./");
            if !invoked && is_file_path_token(tok) {
                out.extend(std::iter::repeat_n(' ', tok.len()));
            } else {
                out.push_str(tok);
            }
            prev = Some(tok);
            cursor = start + len;
        }
        out.push_str(&seg[cursor..]);
    }
    out
}

/// The outward deploy/publish/push needles (tiers 1 and 2), matched over
/// `lower` with bare, non-invoked file-path tokens blanked (see
/// [`mask_file_path_tokens`]). Shared by [`classify`] (task prose) and
/// [`classify_command`] (a command) so the two can never drift apart.
fn deploy_signal(lower: &str) -> bool {
    let deploy_hay = mask_file_path_tokens(lower);
    let tier1 = DEPLOY_SIGNALS.iter().any(|s| deploy_hay.contains(s));
    let tier2 = DEPLOY_VERBS.iter().any(|v| has_word(&deploy_hay, v))
        && OUTWARD_TARGETS.iter().any(|t| has_word(&deploy_hay, t));
    tier1 || tier2
}

/// Classify a task's PROSE (title + done-criteria + touched files) into
/// `{risk, reversible}`.
///
/// Prose is NOT a command (backlog dc955812, user ruling 2026-10-20): it is
/// never handed to [`detect`] as if it were a Bash command line, so prose that
/// merely DESCRIBES a command (e.g. quotes `git config core.hooksPath` in
/// backticks) is not judged as running it. The commands a task will actually
/// run are classified separately by [`classify_command`] /
/// [`classify_commands`], and [`classify_change`] refuses to answer (returns
/// `Undetermined`) when the caller cannot say what those commands are.
///
/// What prose still carries — intent-level needles, which are read here
/// exactly as before:
///   1. outward deploy/publish/push  → High, irreversible (needles are
///      matched with bare, non-invoked file-path tokens blanked — a needle
///      that appears only inside a file NAME does not gate; see
///      [`mask_file_path_tokens`])
///   2. local history rewrite        → Medium, reversible
///   3. everything else              → Low, reversible
///
/// `Low` + reversible from this function is therefore NOT "the task is safe":
/// it only says the prose names no outward/history action. A caller that has
/// no declared command list must not read it as the whole answer — use
/// [`classify_change`], which is undetermined in that case.
pub fn classify(prose: &str) -> RiskAssessment {
    let lower = prose.to_ascii_lowercase();
    if deploy_signal(&lower) {
        return RiskAssessment {
            risk: Risk::High,
            reversible: false,
        };
    }
    if HISTORY_SIGNALS.iter().any(|s| lower.contains(s)) {
        return RiskAssessment {
            risk: Risk::Medium,
            reversible: true,
        };
    }
    RiskAssessment {
        risk: Risk::Low,
        reversible: true,
    }
}

/// Classify ONE command a task will actually run into `{risk, reversible}`.
///
/// This is the full analysis the pre-dc955812 `classify` applied to every
/// input (prose included); it is unchanged, only scoped to real commands.
///
/// Precedence (first match wins):
///   1. outward deploy/publish/push  → High, irreversible (needles are
///      matched with bare, non-invoked file-path tokens blanked — a needle
///      that appears only inside a file NAME does not gate; see
///      [`mask_file_path_tokens`])
///   2. locally destructive (whatever [`detect`] would DENY as a Bash command)
///      → High, irreversible
///   3. [`detect`] answered ASK — it refused to analyse the construct
///      → Medium, NOT reversible (see the note below)
///   4. local history rewrite        → Medium, reversible
///   5. everything else              → Low, reversible
///
/// ## Why an `Ask` is not `Low` + reversible
///
/// `Low` + `reversible: true` is not a neutral default here: it is the exact
/// pair `condukt::gate_exec::decide_gate_exec` reads as
/// `GateExec::AutoExec`, i.e. "run this without human sign-off". Routing an
/// `Ask` — which `crate::model::Decision` defines as "NOT a verdict about the
/// command, it is a refusal to guess about one" — into that pair turns
/// blastguard's refusal to analyse into permission to proceed. That is the
/// fail-open CLAUDE.md §3 forbids, one layer up from where `Ask` was
/// introduced (backlog `ed941047`).
///
/// `reversible: false` is NOT a claim that the action cannot be undone; it is
/// the restrictive resolution of "we do not know", because `reversible: true`
/// is the claim we have no basis to make. `Medium` rather than `High` is a
/// measured choice, not a hedge: [`RiskAssessment::requires_gate`] fires on
/// `High && !reversible`, so mapping `Ask` to `High` would push every
/// unanalysable action through condukt's outward GATED gate. Measured
/// 2026-09-24 at rev `b47f08e3` over the 1040 real task descriptions in this
/// repo's `.backlog/tasks.toml` (title + done-criteria + notes, fed to
/// `detect` exactly as condukt feeds them): DENY 85, **ASK 101**, ALLOW 854 —
/// and **74 of those 101 Asks classified `Low` + reversible**, i.e. were
/// auto-exec eligible. `Medium` + `!reversible` moves those 74 to
/// `Escalate` (a human decides) without force-gating the other 101 through
/// the outward gate.
pub fn classify_command(text: &str) -> RiskAssessment {
    let lower = text.to_ascii_lowercase();

    // 1a. Tier-1 outward publish/deploy signal — leaves the machine, hard to undo.
    // 1b. Tier-2: a deploy verb co-occurring with an outward target, both matched
    //     at word boundaries so bare verbs / trap words never fire.
    // Both tiers scan the masked text (bare file-path tokens outside backtick
    // spans and outside command position blanked — see
    // [`mask_file_path_tokens`]). Steps 2-4 still see the full text.
    if deploy_signal(&lower) {
        return RiskAssessment {
            risk: Risk::High,
            reversible: false,
        };
    }

    // 2/3. Reuse the binary detector so the two stay in lockstep — and read
    //    ALL THREE of its answers, not just `Deny`. Anything blastguard would
    //    DENY is irreversible data loss. An `Ask` is blastguard refusing to
    //    guess, which must not resolve to the auto-exec corner; see the
    //    "Why an `Ask` is not `Low` + reversible" note above.
    let verdict = detect::detect("Bash", Some(&json!({ "command": text })));
    if verdict.is_deny() {
        return RiskAssessment {
            risk: Risk::High,
            reversible: false,
        };
    }
    if verdict.is_ask() {
        return RiskAssessment {
            risk: Risk::Medium,
            reversible: false,
        };
    }

    // 4. History rewrite — recoverable via reflog, but risky.
    if HISTORY_SIGNALS.iter().any(|s| lower.contains(s)) {
        return RiskAssessment {
            risk: Risk::Medium,
            reversible: true,
        };
    }

    // 5. Default — ordinary, reversible work.
    RiskAssessment {
        risk: Risk::Low,
        reversible: true,
    }
}

/// Classify every command a task will actually run and merge the results
/// ([`RiskAssessment::merge`]: highest tier wins, reversible only if all are).
///
/// An EMPTY slice is a real observation — the caller declared that the task
/// runs no command — and answers `Low` + reversible. "The caller does not know
/// which commands the task runs" is a different answer and is not expressible
/// here: it is [`classify_change`]'s `commands: None`, which is undetermined.
pub fn classify_commands(commands: &[String]) -> RiskAssessment {
    commands.iter().fold(
        RiskAssessment {
            risk: Risk::Low,
            reversible: true,
        },
        |acc, c| acc.merge(classify_command(c)),
    )
}

/// Generalized classification: merge task-prose risk ([`classify`]), the risk
/// of the commands the task will actually run ([`classify_commands`]) and
/// diff-level semantic risk — configurable sensitive-path globs and
/// public/exported-symbol changes (see [`crate::diffrisk`]) — into ONE
/// [`RiskAssessment`]. All signals ride the same `requires_gate` escalation
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
/// `prose` is the task's free text (title + done-criteria + touched files);
/// it is read by [`classify`] only and is never parsed as a command.
/// `commands` are the commands the task will actually run, passed SEPARATELY
/// (backlog dc955812, user ruling 2026-10-20); each goes through the full
/// [`classify_command`] analysis. `paths` are the changed files (may overlap
/// with what's embedded in `prose`; kept separate so callers with a real
/// diff/file list don't need to serialize it into prose). `diff_text` is
/// optional unified-diff content (empty string if unavailable — the
/// public-symbol signal simply won't fire, matching backward-compatible/
/// additive behavior).
///
/// ## Undetermined is propagated, not folded into the other signals
///
/// Two inputs can leave the answer undetermined, and both return
/// [`Determination::Undetermined`] **even if another signal alone was High**:
///
/// * `commands: None` — the caller cannot say which commands the task runs.
///   Prose is no longer scanned as a command, so reporting the prose-only
///   assessment here would turn "we did not look at what it runs" into
///   "it runs nothing risky". `Some(&[])` (declared: no commands) is the
///   determined counterpart.
/// * the diff-level signal could not be determined (an unparseable configured
///   sensitive-path glob — see [`crate::diffrisk::SensitiveConfig::build`]).
///
/// Callers resolve that to their own restricted side (condukt force-gates /
/// escalates), which is always at least as restrictive as any partial
/// assessment would have been.
pub fn classify_change(
    prose: &str,
    commands: Option<&[String]>,
    paths: &[String],
    diff_text: &str,
    sensitive: &SensitiveConfig,
) -> Determination<RiskAssessment> {
    let Some(commands) = commands else {
        return Determination::undetermined(
            "the task does not declare the commands it will run (`commands` absent); \
             prose is not classified as a command, so what it runs is unknown",
        );
    };
    let action = classify(prose).merge(classify_commands(commands));
    diffrisk::classify_diff(paths, diff_text, sensitive).map(|diff| action.merge(diff))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unwrap a determination the test asserts must be a real observation.
    /// Fails the test on `Undetermined`, so a regression that silently turns a
    /// well-formed config undetermined is loud rather than a different value.
    #[track_caller]
    fn known<T>(d: Determination<T>) -> T {
        d.require()
            .expect("this test's config must classify to a Known value")
    }

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
        // Anything the binary detector denies is irreversible data loss — when
        // it is passed as a COMMAND (dc955812: prose is not parsed as one).
        for action in ["rm -rf build", "git reset --hard HEAD~1", "git clean -fdx"] {
            let a = classify_command(action);
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

    // ---- backlog 11be9484 (file-name half, user ruling 2026-10-16) ----

    #[test]
    fn deploy_needle_only_inside_a_touched_file_name_does_not_gate() {
        for text in [
            "check script exits 0 when the registry is absent scripts/check-plugin-rollout.py",
            "fix a typo scripts/rollout-plugins.sh",
            "tidy the helper deploy.sh",
            "edit crates/foo/src/deployment.rs and crates/foo/README.md",
            "update scripts/test_check_plugin_rollout.py, then commit",
        ] {
            let a = classify(text);
            assert!(
                !a.requires_gate(),
                "{text:?} must NOT be force-gated, got {a:?}"
            );
        }
    }

    #[test]
    fn real_commands_containing_paths_still_gate() {
        for text in [
            "git push origin main",
            "git push origin feature/x",
            "scripts/rollout-plugins.sh",
            "./scripts/rollout-plugins.sh --all",
            "run scripts/rollout-plugins.sh",
            "bash scripts/rollout-plugins.sh",
            "cd crates && scripts/rollout-plugins.sh",
            "build it; scripts/rollout-plugins.sh",
            "edit notes then run `scripts/rollout-plugins.sh`",
            "deploy crates/foo/src/lib.rs to prod",
            "aws s3 sync ./dist s3://prod-bucket/site",
            "rsync -av dist/ prod:/var/www/html",
            "scp build/app.tar user@prodserver:/var/www",
            "kubectl apply -f k8s/deploy.yaml",
            // detect still sees the original path argument.
            "rm -rf /some/path",
            "rm -rf build/out",
        ] {
            let a = classify_command(text);
            assert!(
                a.requires_gate(),
                "{text:?} MUST still be force-gated, got {a:?}"
            );
        }
    }

    #[test]
    fn backtick_spans_are_still_never_masked_for_deploy_needles() {
        // Backtick spans are never masked (11be9484 file-name half): a deploy
        // needle quoted in prose still gates through `classify`.
        for text in [
            "touch the `scripts/check-plugin-rollout.py` checker",
            "unbalanced `scripts/check-plugin-rollout.py checker",
        ] {
            assert!(classify(text).requires_gate(), "{text:?} must still gate");
        }
    }

    // ---- backlog dc955812 (user ruling 2026-10-20) ----
    // CONTRACT CHANGE: prose that only DESCRIBES a command no longer goes to
    // `detect` (the old `backtick_quoted_command_prose_gates_exactly_as_before`
    // pinned the opposite). The command itself, passed as a command, still
    // gates exactly as before — classify_command is the unchanged analysis.

    #[test]
    fn prose_quoting_a_command_is_not_parsed_as_that_command() {
        let prose = "block the retry when it re-runs `git config core.hooksPath /dev/null`";
        assert!(!classify(prose).requires_gate(), "prose gated: {prose:?}");
    }

    #[test]
    fn the_same_command_passed_as_a_command_still_gates() {
        let cmd = "git config core.hooksPath /dev/null";
        let a = classify_command(cmd);
        assert!(
            a.requires_gate(),
            "{cmd:?} must gate as a command, got {a:?}"
        );
        let a = classify_commands(&["cargo test".to_string(), cmd.to_string()]);
        assert!(
            a.requires_gate(),
            "one dangerous command among benign ones must gate the set, got {a:?}"
        );
    }

    #[test]
    fn declared_empty_command_list_is_low_and_reversible() {
        assert_eq!(
            classify_commands(&[]),
            RiskAssessment {
                risk: Risk::Low,
                reversible: true,
            }
        );
    }

    #[test]
    fn undeclared_commands_are_undetermined_even_for_benign_prose() {
        let cfg = SensitiveConfig::default();
        let d = classify_change("refactor the parser", None, &[], "", &cfg);
        assert!(
            matches!(d, Determination::Undetermined(_)),
            "undeclared commands must not classify (Low would be a guess)"
        );
    }

    #[test]
    fn classify_change_force_gates_a_dangerous_declared_command_under_benign_prose() {
        let cfg = SensitiveConfig::default();
        let cmds = ["rm -rf build".to_string()];
        let a = known(classify_change(
            "refactor the parser",
            Some(&cmds),
            &["src/parser.rs".to_string()],
            "",
            &cfg,
        ));
        assert!(a.requires_gate(), "declared rm -rf must gate, got {a:?}");
    }

    #[test]
    fn file_path_token_rule_is_narrow() {
        assert!(is_file_path_token("scripts/check-plugin-rollout.py"));
        assert!(is_file_path_token("deploy.sh"));
        assert!(is_file_path_token("rollout.md,"));
        assert!(is_file_path_token("crates/x/"));
        assert!(!is_file_path_token("rollout"));
        assert!(!is_file_path_token("v1.2.0"));
        assert!(!is_file_path_token("s3://bucket/key"));
        assert!(!is_file_path_token("prod:/var/www"));
        assert!(!is_file_path_token("user@host"));
        assert_eq!(
            mask_file_path_tokens("edit scripts/rollout.py now"),
            format!("edit {} now", " ".repeat("scripts/rollout.py".len()))
        );
        assert_eq!(
            mask_file_path_tokens("scripts/rollout.py now"),
            "scripts/rollout.py now"
        );
        assert_eq!(
            mask_file_path_tokens("see `a/rollout.py` b/rollout.py"),
            "see `a/rollout.py` b/rollout.py",
            "first token after a backtick span is command position"
        );
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

    /// What [`detect`] answers for `text` as a Bash command — the exact call
    /// `classify_command` makes internally, so a precondition assert here
    /// observes the same verdict `classify_command` sees.
    fn detect_verdict(text: &str) -> crate::model::Decision {
        detect::detect("Bash", Some(&json!({ "command": text })))
    }

    /// Commands `detect` answers `Ask` for TODAY (measured, not guessed): an
    /// unrecognised wrapper head in front of a destructive line, and an
    /// unrecognised head naming a protected gate/config path. Each test below
    /// asserts the `Ask` precondition first, so if the detector's coverage ever
    /// moves these to `Allow`/`Deny` the test fails loudly instead of quietly
    /// degrading into a test of some other input class.
    const DETECT_ASK_COMMANDS: &[&str] = &[
        "my-cleanup-wrapper rm -rf /some/path",
        "some-wrapper chmod 777 .githooks/pre-commit",
    ];

    #[test]
    fn detect_ask_is_never_classified_as_low_and_reversible() {
        // CONTRACT: `Decision::Ask` is a refusal to analyse, not a clean bill of
        // health. `{risk: Low, reversible: true}` is exactly the corner
        // `condukt::gate_exec::decide_gate_exec` auto-execs under an auto
        // policy, so mapping an Ask there lets a command blastguard explicitly
        // declined to understand run without a human. Undetermined must resolve
        // to the restricted side: at least Medium, and never reversible.
        for text in DETECT_ASK_COMMANDS {
            // Precondition: this really is an Ask today.
            let d = detect_verdict(text);
            assert!(
                d.is_ask(),
                "precondition: {text:?} must make detect answer Ask today, got {d:?}"
            );

            let a = classify_command(text);
            assert!(
                a.risk >= Risk::Medium,
                "{text:?}: detect refused to analyse it, so classify must not report Low risk; got {a:?}"
            );
            assert!(
                !a.reversible,
                "{text:?}: an unanalysable command must not be reported reversible; got {a:?}"
            );
            assert_ne!(
                a,
                RiskAssessment {
                    risk: Risk::Low,
                    reversible: true,
                },
                "{text:?}: Low+reversible is the auto-exec corner; an Ask must never land there"
            );
        }
    }

    #[test]
    fn detect_allow_control_still_classifies_low_and_reversible() {
        // CONTROL — must stay green before AND after the fix. Without it the
        // contract test above would also pass if someone escalated everything.
        for text in ["refactor the parser", "fix a typo in the README"] {
            let d = detect_verdict(text);
            assert!(
                matches!(d, crate::model::Decision::Allow),
                "precondition: {text:?} must be plainly allowed by detect, got {d:?}"
            );

            let a = classify_command(text);
            assert_eq!(a.risk, Risk::Low, "{text:?} must stay Low risk; got {a:?}");
            assert!(a.reversible, "{text:?} must stay reversible; got {a:?}");
            assert!(!a.requires_gate(), "{text:?} must not be force-gated");
        }
    }

    #[test]
    fn detect_deny_control_still_classifies_high_and_irreversible() {
        // CONTROL — the existing Deny precedence must be untouched by the fix.
        for text in ["rm -rf build", "git clean -fdx"] {
            let d = detect_verdict(text);
            assert!(
                d.is_deny(),
                "precondition: {text:?} must be denied by detect, got {d:?}"
            );

            let a = classify_command(text);
            assert_eq!(
                a.risk,
                Risk::High,
                "{text:?} must stay High risk; got {a:?}"
            );
            assert!(!a.reversible, "{text:?} must stay irreversible; got {a:?}");
            assert!(a.requires_gate(), "{text:?} must require the GATED gate");
        }
    }

    #[test]
    fn classify_change_baseline_matches_command_only_classification() {
        // No sensitive path, no public-symbol diff → identical to classify().
        let cfg = SensitiveConfig::default();
        let text = "refactor the parser";
        let a = known(classify_change(
            text,
            Some(&[]),
            &["src/parser.rs".to_string()],
            "",
            &cfg,
        ));
        assert_eq!(a, classify(text));
    }

    #[test]
    fn classify_change_sensitive_path_raises_risk_over_baseline() {
        let cfg = SensitiveConfig::default();
        let text = "add a login retry limit";
        let baseline = classify(text);
        let a = known(classify_change(
            text,
            Some(&[]),
            &["src/auth/login.rs".to_string()],
            "",
            &cfg,
        ));
        assert!(a.risk > baseline.risk, "sensitive path must raise risk");
    }

    #[test]
    fn classify_change_public_symbol_raises_risk_over_baseline() {
        let cfg = SensitiveConfig::default();
        let text = "add a helper function";
        let baseline = classify(text);
        let a = known(classify_change(
            text,
            Some(&[]),
            &["src/lib.rs".to_string()],
            "+pub fn new_helper() {}",
            &cfg,
        ));
        assert!(a.risk > baseline.risk, "public API change must raise risk");
    }

    #[test]
    fn classify_change_still_force_gates_deploy_text_regardless_of_diff_signals() {
        // The existing destructive/deploy precedence is untouched: merge only
        // ever raises risk, never masks the pre-existing High+irreversible verdict.
        let cfg = SensitiveConfig::default();
        let a = known(classify_change(
            "git push origin main",
            Some(&[]),
            &["README.md".to_string()],
            "",
            &cfg,
        ));
        assert_eq!(a.risk, Risk::High);
        assert!(!a.reversible);
        assert!(a.requires_gate());
    }
}
