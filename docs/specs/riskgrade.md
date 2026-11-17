> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft（2026-11-17 に docs/specs/blastguard.md の
> classify / diffrisk 部分を移設したもの。本文は移設元のまま）。人間レビューまで正典として扱わない。

# riskgrade 仕様

## 概要

`riskgrade` は計画された action の **graded risk / reversibility** を判定する純粋ライブラリ
（`classify`）と、diff 内容から意味的リスクを見る **diff-level リスク信号**（`diffrisk`）を持つ
lib crate である。2026-11-17 のユーザー裁定（blastguard はコマンド安全性の判定だけを持つ）で
blastguard から移設した。配布プラグインではなく（`.claude-plugin/plugin.json` を持たない）、
condukt に静的リンクされる。tier-2「ローカル破壊」判定は blastguard の `detect::detect` に委譲する。

## 不変条件

- **判定は純粋・決定論** — `classify` / `diffrisk` は I/O を持たず、`(入力) -> 出力` の純関数。
- **detect と classify の lockstep** — `classify::classify` の tier-2「ローカル破壊」判定は
  blastguard の `detect::detect("Bash", …)` を再利用するため、binary の deny 集合と classifier の High/irreversible
  集合が乖離しない（同一検出器を共有）。

## module 責務

- **`classify`** — planned action の graded risk/reversibility 分類器（`RiskAssessment{risk, reversible}`,
  `Risk::{Low,Medium,High}`, `requires_gate()` = High かつ not reversible）。precedence: (1) outward
  deploy/publish/push（tier-1 `DEPLOY_SIGNALS` 部分文字列 + tier-2 `DEPLOY_VERBS`×`OUTWARD_TARGETS` を
  `has_word` 単語境界で照合）→ High/irreversible、(2) ローカル破壊（`detect` 委譲）→ High/irreversible、
  (3) history rewrite（`HISTORY_SIGNALS`: rebase/`--amend`/filter-branch）→ Medium/reversible、
  (4) その他 → Low/reversible。`has_word` が `ownership`⊃`ship` 等の偽陽性を防ぐ。condukt が
  GATED ゲート強制に利用する。
- **`diffrisk`** — 下の「一般化リスクスコアリング」節。

## 一般化リスクスコアリング (diff-level 意味的リスク)

> **REVIEW-NEEDED**: コードから逆算 (2026-07-09 セッション)。人間レビュー前は正典としない。

### 概要

`diffrisk` module は `classify` の command-text 分類（tier-1/2 deploy 語彙、`detect` 委譲のローカル
破壊、history rewrite）に加えて、**diff-level の意味的リスク**を同じ `RiskAssessment` 軸へ合流させる。
判定材料は2つ: (1) 変更パスが機微パス glob（auth/authn/authz/login/session/oauth/credential/secret、
payment/billing/checkout/stripe、pii/personal_data/gdpr の既定 `DEFAULT_SENSITIVE_GLOBS`。
`SensitiveConfig::with_extra_globs`/`from_globs` で呼び出し側が拡張・置換可能）に一致するか、
(2) unified diff テキストに Rust の公開シンボル追加/変更（`pub fn`/`pub struct`/`pub enum`/
`pub trait`/`pub const`/`pub static`/`pub type`/`pub mod`、`pub(crate)` 修飾も同様に扱う）を示す
`+`/`-` 行があるか。`classify::classify_change(text, paths, diff_text, sensitive)` が command 分類と
diff 分類を `RiskAssessment::merge` で統合し、condukt の `gate check` / force-gate エスカレーションは
1つの `requires_gate()` 軸だけを見ればよい（並行パス無し）。

### 不変条件

- **機微パス glob は設定可能** — 既定は `DEFAULT_SENSITIVE_GLOBS`（auth/payment/PII 系）。
  `SensitiveConfig::with_extra_globs` は既定に追加、`SensitiveConfig::from_globs` は既定を使わず
  呼び出し側リストのみを採用する（プロジェクト固有の機微パスを完全に所有したい場合向け）。
- **純関数** — `diffrisk` module は I/O を持たない。`classify_diff`/`changes_public_symbol`/
  `SensitiveConfig::any_sensitive` は `(入力) -> 出力` の決定論的写像（`SensitiveConfig::build` の
  `GlobSet` 構築もメモ化なしの純計算）。
- **既存の破壊コマンド分類は不変で追加的** — `classify::classify`（tier-1/2 deploy、`detect` 委譲の
  ローカル破壊、history rewrite、既定 Low）のロジック・precedence は一切変更しない。
  `classify_change` は `classify(text)` と `diffrisk::classify_diff(...)` を `merge` するだけで、
  diff signal が既存の deploy/破壊判定を弱める方向には作用しない
  （`classify_change_still_force_gates_deploy_text_regardless_of_diff_signals` テストが固定）。
- **公開シンボル変更検知は diff header `+++`/`---` を誤検知しない** — `changes_public_symbol` は
  行頭の `+`/`-` マーカーの直後が更に `+`/`-` で始まる場合（unified diff のファイルヘッダ行）を
  除外してから `pub …` プレフィックスを照合する。プレーンな context 行（マーカー無し）も対象外
  （`diff_file_headers_are_not_mistaken_for_pub_markers` テストが固定）。
- **merge は max risk かつ both-reversible のみ reversible** — `RiskAssessment::merge` は
  `risk` を2つの `Ord` 実装 (`Low < Medium < High`) の max、`reversible` は両方が true の場合のみ
  true（どちらかが irreversible なら結果も irreversible。希釈させない）。

### 振る舞い

機微パス glob 該当（auth/payment/PII）と公開シンボル変更を1つの `RiskAssessment{risk, reversible}`
へ合流させ、既存の `requires_gate()` エスカレーションに相乗りする:

- 機微パスのみ該当、または公開シンボル変更のみ検出 → `Risk::Medium`、reversible
  （人間はコミットを revert できるので undoability ではなく *review 姿勢*の格上げ）。
- 両方該当 → `Risk::High`、reversible（複合的な review-worthiness だが、diff signal 単体は
  irreversible 化しない — irreversible 化するのは command-text 側の deploy/ローカル破壊判定のみ）。
- どちらも該当しない → `Risk::Low`（`classify_change` は `classify(text)` 単体と同一結果になり、
  baseline 後方互換）。
- deploy/push テキスト（例: `git push origin main`）は diff signal の有無に関わらず、
  従来どおり `classify` 側の tier-1/2 判定で `High`/irreversible/`requires_gate()` に force-gate
  される（`merge` は risk を上げる方向にのみ作用するため、diff 側が Low でも既存の force-gate は
  マスクされない）。
