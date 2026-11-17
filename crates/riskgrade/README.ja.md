# riskgrade

計画された action の段階的な risk / reversibility 判定（`classify`）と、diff-level の意味的
リスク信号（`diffrisk`）を持つ純粋ライブラリ（I/O なし）。condukt に静的リンクされる。
配布プラグインではない（`.claude-plugin/plugin.json` を持たない）。

2026-11-17 に blastguard から移設した（ユーザー裁定: blastguard はコマンド安全性の判定だけを持つ）。
ローカル破壊の判定は引き続き blastguard の検出器（`blastguard::detect::detect`）を再利用するので、
classifier の High/irreversible 集合が hook の deny 集合から乖離しない。仕様: `docs/specs/riskgrade.md`。

## 何をするか

condukt のスケジューラは段階的な
`classify::classify`（risk / reversibility 判定）を使って、上流の LLM が
誤ラベル付けした場合でも deploy・`git push`・release のような対外的で不可逆な
アクションを GATED ゲートへ強制的に通す。
タスク文中のファイル名の内側にだけ現れる deploy 語（例: `scripts/check-plugin-rollout.py`
の `rollout`）では強制ゲートしない。
ファイルパス token とは `/` を含むか、
`.py`/`.rs`/`.sh`/`.md`/`.toml` 等の既知拡張子で終わり、かつ `:` も `@` も含まない語のこと。
先頭にあるもの・runner 語（`run`, `bash` 等）や shell 区切りの直後にあるもの・`./` で
始まるものは従来どおり判定対象に残る。
backtick で引用されたものも判定対象に残る。
タスク散文はコマンドとして解析しない: `classify` は散文を deploy / history の needle だけで読むので、
コマンドを記述するだけの散文（例: backtick で `git config core.hooksPath` を引用する）は強制ゲートしない。
タスクが実際に実行するコマンドは別に渡され、従来どおりの `classify_command` で全解析されるので、
同じコマンドをコマンドとして渡せば gate される。
コマンド一覧が宣言されていない（`None`）`classify_change` は判定不能であり、Low にはならない。

コマンド分類とは別に、**diff の内容そのもの**から意味的リスクを見るモジュールと、
それが使う caller 列挙（harness-core 側）がある。いずれも純粋・決定論・I/O なしで、command 分類が拾えない「無害な
コマンドで危険な差分を書く」ケースを埋める:

- **`diffrisk`**（`crates/riskgrade/src/diffrisk.rs`）— 変更されたファイルパスがセンシティブ領域
  （auth/認証・payment/決済・PII 等、`DEFAULT_SENSITIVE_GLOBS` に列挙）に触れているか、
  および公開 API シグネチャ（`pub fn`/`pub struct` 等）を変更しているかを、diff の
  unified diff テキストから直接判定する（`classify_diff` / `changes_public_symbol`）。
  結果は command classification と同じ `riskgrade::classify::RiskAssessment` に載るので、
  呼び出し側は2つの軸を別々に扱わず1本の risk として扱える。
- **`harness_core::callgraph`** — 変更された symbol を diff から抽出し
  （`changed_symbol_names`）、その symbol を実際に参照している呼び出し箇所を
  ソースコーパス全体から字句レベルで列挙する（`enumerate_callers`）。パーサ・正規表現・
  外部 API 不使用の純粋な文字列トークンスキャンで、どんな入力に対しても panic しない
  （壊れた/中途半端なソースに対する fail-soft floor）。

**condukt 側の呼び出し経路**: `crates/condukt/src/diffrisk_record.rs` が
`riskgrade::diffrisk::{classify_diff, classify_diff_with_callers, SensitiveConfig}` と
`harness_core::callgraph::{changed_symbol_names, enumerate_callers}` を呼び出し、
コンパイル済みの caller コーパスと突き合わせて `(needs_review, high_risk)` 相当の
判定を1つの `ViolationSource::Blastguard` イベントとして `overwatch` へ記録する
（`crates/condukt/src/schedule.rs` / `gate_exec.rs` / `review_brief.rs` もこの経路を
消費し、diff の blast-radius をスケジューリング・レビューブリーフ生成に反映する）。
