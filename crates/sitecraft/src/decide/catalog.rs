//! The capability-area catalog: the areas a site may need decisions on, each
//! with representative questions and answer-conditional follow-ups.
//!
//! Selecting which areas matter for the user's purpose is the skill's (LLM's)
//! job; this module only holds the fixed catalog and the deterministic
//! question-activation rule. Area ids (fixed by the decide tests):
//! `auth`, `users-roles`, `admin`, `storage`, `data-analytics`,
//! `web-analytics`, `processing`, `workflow`, `notification`, `ai`,
//! `formats`, `exposure`.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::Determination;

/// Pseudo question id every area has: whether the area is relevant to the
/// purpose. Answers are [`RELEVANT`] / [`IRRELEVANT`]. It is asked (by
/// `decide next`) before any other question of the area; an `irrelevant`
/// answer settles the area with no further questions.
pub const RELEVANCE_QUESTION: &str = "relevance";
/// Answer to [`RELEVANCE_QUESTION`]: the area matters for this site.
pub const RELEVANT: &str = "relevant";
/// Answer to [`RELEVANCE_QUESTION`]: the area is out of scope (recorded, not skipped).
pub const IRRELEVANT: &str = "irrelevant";

/// Answers recorded for one area, keyed by question id.
pub type Answers = BTreeMap<String, String>;

/// One catalog area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Area {
    /// Stable id (e.g. `auth`).
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// All questions of the area, representative and conditional.
    pub questions: Vec<Question>,
}

/// One question in an area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Question {
    /// Id, unique within its area.
    pub id: String,
    /// The question text shown to the user.
    pub text: String,
    /// Allowed answers. Empty means free text.
    pub choices: Vec<String>,
    /// `None` for a representative question (always asked once the area is
    /// relevant); `Some` for a follow-up asked only when the condition holds.
    pub when: Option<Condition>,
}

/// Activation condition of a follow-up question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Condition {
    /// The question (same area) whose answer gates this one.
    pub question_id: String,
    /// The follow-up is active iff that question's recorded answer is one of these.
    pub answer_in: Vec<String>,
}

/// Build a question. `choices` empty = free text.
fn q(id: &str, text: &str, choices: &[&str]) -> Question {
    Question {
        id: id.to_string(),
        text: text.to_string(),
        choices: choices.iter().map(|c| (*c).to_string()).collect(),
        when: None,
    }
}

/// Build a follow-up asked only when `gate`'s answer is one of `on`.
fn follow(id: &str, text: &str, choices: &[&str], gate: &str, on: &[&str]) -> Question {
    Question {
        when: Some(Condition {
            question_id: gate.to_string(),
            answer_in: on.iter().map(|a| (*a).to_string()).collect(),
        }),
        ..q(id, text, choices)
    }
}

fn area(id: &str, title: &str, questions: Vec<Question>) -> Area {
    Area {
        id: id.to_string(),
        title: title.to_string(),
        questions,
    }
}

/// The fixed catalog content (asking order).
fn catalog() -> Vec<Area> {
    vec![
        area(
            "auth",
            "認証",
            vec![
                q(
                    "needs-login",
                    "利用者がログインする必要がありますか？（yes: アカウントでログインする / no: 誰でもログインなしで使う）",
                    &["yes", "no"],
                ),
                follow(
                    "auth-method",
                    "ログイン方式はどれにしますか？（email-password: メール+パスワード / magic-link: メールのリンク / oauth-social: Google・GitHub 等の外部アカウント / sso: 社内 SSO（SAML・OIDC）/ passkey: パスキー）",
                    &["email-password", "magic-link", "oauth-social", "sso", "passkey"],
                    "needs-login",
                    &["yes"],
                ),
                follow(
                    "mfa",
                    "二要素認証（MFA）は必要ですか？（required: 全員必須 / optional: 任意で有効化 / none: 使わない）",
                    &["required", "optional", "none"],
                    "needs-login",
                    &["yes"],
                ),
            ],
        ),
        area(
            "users-roles",
            "利用者と権限",
            vec![
                q(
                    "user-model",
                    "利用者の構成はどれですか？（single-user: 自分ひとり / same-rights: 複数人だが権限は全員同じ / multi-role: 役割ごとに権限が違う）",
                    &["single-user", "same-rights", "multi-role"],
                ),
                follow(
                    "role-set",
                    "役割の分け方はどれに近いですか？（admin-member: 管理者と一般 / admin-editor-viewer: 管理者・編集者・閲覧者 / custom: 独自の役割を定義）",
                    &["admin-member", "admin-editor-viewer", "custom"],
                    "user-model",
                    &["multi-role"],
                ),
                follow(
                    "signup",
                    "利用者はどうやって登録しますか？（self-signup: 自分で登録 / invite-only: 招待制 / admin-created: 管理者が作成）",
                    &["self-signup", "invite-only", "admin-created"],
                    "user-model",
                    &["same-rights", "multi-role"],
                ),
            ],
        ),
        area(
            "admin",
            "管理（管理画面・データ管理）",
            vec![
                q(
                    "admin-ui",
                    "サイト内に管理画面が必要ですか？（yes: 管理画面を作る / no: 管理はコード・DB を直接編集する）",
                    &["yes", "no"],
                ),
                follow(
                    "admin-scope",
                    "管理画面で何を管理しますか？（content: 記事・ページ等のコンテンツ / content-and-users: コンテンツと利用者 / all-data: 全データ）",
                    &["content", "content-and-users", "all-data"],
                    "admin-ui",
                    &["yes"],
                ),
                q(
                    "bulk-data",
                    "データの一括取り込み・書き出しは必要ですか？（none: 不要 / csv: CSV で入出力 / api: API で連携）",
                    &["none", "csv", "api"],
                ),
            ],
        ),
        area(
            "storage",
            "データ保存",
            vec![
                q(
                    "data-kind",
                    "保存するデータはどれですか？（structured: 表形式のデータ / files: 画像・動画などのファイル / both: 両方 / browser-only: ブラウザ内（localStorage 等）だけ）",
                    &["structured", "files", "both", "browser-only"],
                ),
                follow(
                    "database",
                    "表形式データの保存先はどれにしますか？（sqlite: SQLite / postgres: PostgreSQL / baas: Supabase・Firebase 等のサービス / key-value: KV ストア）",
                    &["sqlite", "postgres", "baas", "key-value"],
                    "data-kind",
                    &["structured", "both"],
                ),
                follow(
                    "file-storage",
                    "ファイルの保存先はどれにしますか？（local-disk: サーバーのディスク / object-storage: S3 互換ストレージ / cdn-service: 画像配信サービス）",
                    &["local-disk", "object-storage", "cdn-service"],
                    "data-kind",
                    &["files", "both"],
                ),
                q(
                    "backup",
                    "データのバックアップはどうしますか？（none: 不要（消えても作り直せる）/ daily: 毎日 / managed: 保存先サービスの機能に任せる）",
                    &["none", "daily", "managed"],
                ),
            ],
        ),
        area(
            "data-analytics",
            "データ・ログ解析",
            vec![
                q(
                    "analysis-target",
                    "何を解析しますか？（app-logs: アプリのログ / business-data: 業務・利用データ / both: 両方）",
                    &["app-logs", "business-data", "both"],
                ),
                q(
                    "analysis-output",
                    "解析結果はどう見せますか？（dashboard: ダッシュボード / report: 定期レポート / export: データ書き出しのみ）",
                    &["dashboard", "report", "export"],
                ),
                follow(
                    "report-frequency",
                    "レポートの頻度はどれですか？（daily: 毎日 / weekly: 毎週 / monthly: 毎月）",
                    &["daily", "weekly", "monthly"],
                    "analysis-output",
                    &["report"],
                ),
            ],
        ),
        area(
            "web-analytics",
            "アクセス解析",
            vec![
                q(
                    "analytics-tool",
                    "アクセス解析の方法はどれにしますか？（privacy-friendly: Cookie を使わない解析（Plausible 等）/ google-analytics: Google Analytics / self-hosted: 自前ホストの解析（Matomo 等）/ server-logs: サーバーログのみ）",
                    &["privacy-friendly", "google-analytics", "self-hosted", "server-logs"],
                ),
                follow(
                    "consent-banner",
                    "Cookie 同意バナーを表示しますか？（yes: 表示し同意後に計測 / no: 表示しない）",
                    &["yes", "no"],
                    "analytics-tool",
                    &["google-analytics", "self-hosted"],
                ),
            ],
        ),
        area(
            "processing",
            "処理（バッチ・重い処理・変換・生成）",
            vec![
                q(
                    "processing-kind",
                    "どんな処理が必要ですか？（batch: まとめて定期処理 / heavy-compute: 重い計算 / conversion: ファイル・データの変換 / generation: 画像・文書などの生成）",
                    &["batch", "heavy-compute", "conversion", "generation"],
                ),
                q(
                    "execution",
                    "処理はどこで実行しますか？（in-request: リクエスト中に同期実行 / background: バックグラウンドのジョブキュー / browser: 利用者のブラウザ内 / external: 外部サービス）",
                    &["in-request", "background", "browser", "external"],
                ),
                follow(
                    "progress",
                    "処理の進み具合を利用者に見せますか？（yes: 進捗・完了を表示 / no: 結果だけ）",
                    &["yes", "no"],
                    "execution",
                    &["background", "external"],
                ),
            ],
        ),
        area(
            "workflow",
            "workflow（自動化・承認フロー・スケジュール実行）",
            vec![
                q(
                    "workflow-kind",
                    "どの種類のワークフローですか？（scheduled: 決まった時刻に実行 / approval: 承認フロー / event: 出来事をきっかけに自動実行）",
                    &["scheduled", "approval", "event"],
                ),
                follow(
                    "schedule",
                    "実行の頻度はどれですか？（hourly: 毎時 / daily: 毎日 / weekly: 毎週 / custom: 独自の cron 指定）",
                    &["hourly", "daily", "weekly", "custom"],
                    "workflow-kind",
                    &["scheduled"],
                ),
                follow(
                    "approval-steps",
                    "承認は何段階ですか？（single: 1 段階 / multi: 複数段階）",
                    &["single", "multi"],
                    "workflow-kind",
                    &["approval"],
                ),
                q(
                    "on-failure",
                    "自動処理が失敗したらどうしますか？（retry: 自動で再試行 / notify: 担当者に知らせる / both: 再試行して駄目なら知らせる）",
                    &["retry", "notify", "both"],
                ),
            ],
        ),
        area(
            "notification",
            "通知",
            vec![
                q(
                    "channel",
                    "通知の手段はどれにしますか？（email: メール / web-push: ブラウザのプッシュ通知 / in-app: サイト内の通知 / chat: Slack・Discord 等）",
                    &["email", "web-push", "in-app", "chat"],
                ),
                follow(
                    "email-sender",
                    "メールの送信方法はどれにしますか？（smtp: 既存の SMTP サーバー / service: 送信サービス（SendGrid・Resend 等））",
                    &["smtp", "service"],
                    "channel",
                    &["email"],
                ),
                q(
                    "opt-out",
                    "利用者が通知を止められるようにしますか？（yes: 通知設定を用意 / no: 必須の通知のみ送る）",
                    &["yes", "no"],
                ),
            ],
        ),
        area(
            "ai",
            "AI 駆動",
            vec![
                q(
                    "ai-use",
                    "AI を何に使いますか？（chat: 対話アシスタント / generation: 文章・画像の生成 / extraction: 分類・情報抽出 / search: 検索・RAG）",
                    &["chat", "generation", "extraction", "search"],
                ),
                q(
                    "ai-provider",
                    "AI モデルはどこで動かしますか？（hosted-api: クラウドの API / self-hosted: 自前でモデルを動かす / in-browser: ブラウザ内で動かす）",
                    &["hosted-api", "self-hosted", "in-browser"],
                ),
                follow(
                    "user-data-to-ai",
                    "利用者のデータを外部の AI API に送ってよいですか？（allowed: 送ってよい / anonymized: 匿名化してのみ / not-allowed: 送らない）",
                    &["allowed", "anonymized", "not-allowed"],
                    "ai-provider",
                    &["hosted-api"],
                ),
            ],
        ),
        area(
            "formats",
            "扱う形式",
            vec![
                q(
                    "primary-formats",
                    "サイトが主に扱うファイル形式は何ですか？（例: VRM, PNG, CSV。自由記述）",
                    &[],
                ),
                q(
                    "other-formats",
                    "主要な形式以外（例: VRM 以外の 3D 形式）が来たらどうしますか？（reject: 受け付けない / convert: 主要な形式に変換する / accept-as-is: そのまま受け付ける）",
                    &["reject", "convert", "accept-as-is"],
                ),
                follow(
                    "conversion-failure",
                    "変換に失敗したらどうしますか？（reject-with-error: エラーを表示して受け付けない / keep-original: 元のまま保存する）",
                    &["reject-with-error", "keep-original"],
                    "other-formats",
                    &["convert"],
                ),
                q(
                    "max-file-size",
                    "1 ファイルの最大サイズはどれくらいですか？（10mb / 100mb / 1gb / unlimited: 制限なし）",
                    &["10mb", "100mb", "1gb", "unlimited"],
                ),
            ],
        ),
        area(
            "exposure",
            "公開",
            vec![
                q(
                    "audience",
                    "誰に公開しますか？（public: インターネットの誰でも / members-only: ログインした利用者のみ / internal: 社内・自分だけ）",
                    &["public", "members-only", "internal"],
                ),
                q(
                    "hosting",
                    "どこで公開しますか？（static: 静的ホスティング / server: 常時動くサーバー / serverless: サーバーレス / local-only: 手元でのみ動かす）",
                    &["static", "server", "serverless", "local-only"],
                ),
                follow(
                    "seo",
                    "検索エンジンへの最適化（SEO）は必要ですか？（yes: 必要 / no: 不要（検索に載せない））",
                    &["yes", "no"],
                    "audience",
                    &["public"],
                ),
                follow(
                    "custom-domain",
                    "独自ドメインを使いますか？（yes: 使う / no: ホスティングの既定ドメインでよい）",
                    &["yes", "no"],
                    "hosting",
                    &["static", "server", "serverless"],
                ),
            ],
        ),
    ]
}

/// The full catalog, in asking order.
///
/// Contract: `Known` with exactly the 12 areas listed in the module docs,
/// each with at least one representative question (`when == None`). Never
/// `Known(vec![])`: an empty or representative-less catalog is
/// `Undetermined`.
pub fn areas() -> Determination<Vec<Area>> {
    let all = catalog();
    if all.is_empty() {
        return Determination::undetermined("sitecraft: the capability-area catalog is empty");
    }
    if let Some(a) = all
        .iter()
        .find(|a| !a.questions.iter().any(|q| q.when.is_none()))
    {
        return Determination::undetermined(format!(
            "sitecraft: catalog area `{}` has no representative question",
            a.id
        ));
    }
    Determination::known(all)
}

/// The catalog area `area_id`. Unknown id (including the domain pseudo
/// area) or an unavailable catalog → `Undetermined`.
pub fn area_by_id(area_id: &str) -> Determination<Area> {
    match areas() {
        Determination::Known(all) => match all.into_iter().find(|a| a.id == area_id) {
            Some(a) => Determination::known(a),
            None => {
                Determination::undetermined(format!("sitecraft: `{area_id}` is not a catalog area"))
            }
        },
        Determination::Undetermined(why) => Determination::Undetermined(why),
    }
}

/// Is `q` active given the area's `answers`?
#[must_use]
pub fn is_active(q: &Question, answers: &Answers) -> bool {
    match &q.when {
        None => true,
        Some(c) => answers
            .get(&c.question_id)
            .is_some_and(|a| c.answer_in.contains(a)),
    }
}

/// The questions of `area_id` that are active given `answers` (the area's
/// recorded answers), in asking order: every representative question, plus
/// each follow-up whose [`Condition`] holds. Does not include
/// [`RELEVANCE_QUESTION`].
///
/// Contract: unknown `area_id` → `Undetermined`; otherwise `Known` (never
/// empty: every area has a representative question).
pub fn active_questions(area_id: &str, answers: &Answers) -> Determination<Vec<Question>> {
    area_by_id(area_id).map(|a| {
        a.questions
            .into_iter()
            .filter(|q| is_active(q, answers))
            .collect()
    })
}
