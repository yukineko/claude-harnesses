# Agentic-scale レビュー再設計 — 実装項目

> このドキュメントは別セッションでの実装着手を目的とした自己完結ブリーフ。元の議論の会話ログには
> アクセスできない前提で読めるように書いてある。構成は「目的（フレームワーク項目）→現状→修正
> 方針」の順。過去のレビューで見つかった生の指摘リストは末尾の「アーカイブ」に格納し、現在
> アクションが必要な内容だけを本文（現状ステータス総覧・修正方針）に残してある。

## 背景（なぜこのドキュメントがあるか）

Agentic Coding によってコード生産量が現在の数倍〜1日1万行規模になり、複数開発者が同時に
Agentic Coding を行う前提に立つと、**人間によるコードレビューは universal な正しさのゲートとして
構造的に破綻する**、という議論の結論がある。根拠は以下：

1. **diff だけでは不十分** — 呼び出し元・不変条件・並行アクセスパターンなど非局所的な文脈まで
   遡らないと正しさは判定できないが、それを人力で全変更に対しやるコストは非現実的。
2. **レビュアーは容易に増やせない** — 増員は文脈の分割不可能性ゆえに非一貫性を注入する。
3. **責任分担も安全ではない** — 分割した境界を跨ぐ不整合は誰の担当でもなくなる
   （seam の所有者不在問題）。
4. **人間は疲れる** — 実証研究（Cisco/SmartBear）では 1 時間あたり 200〜400 行を超えると欠陥検出率が
   落ち、セッションが 60〜90 分を超えると質が落ちる。量を増やすほど実効検出率はむしろ下がる。
5. **「安心のための儀式」は無レビューより悪い** — 形骸化したレビューは「保証されている」という
   偽の安心感を生み、他の防御を薄くする（security theater 問題）。

この結論から「人間の判断が必要な箇所を最小化し、それ以外は機械的検証に完全に委譲する」という
再設計方針が導かれ、以下の 12 項目のフレームワークに整理された。無謬性は目標としない——
目標は「人間がどの時点で何を確認する必要があるかを明示し、それ以外はAIが可能な限り実行する」
ことである。

## 12 項目フレームワーク

1. **機械的 invariant 層** — 型・契約・PBT・spec drift 検知・capability 境界・静的解析で
   「diff + 前後文脈」を機械が保証する。
2. **リスクスコアリング/トリアージ** — 変更を自動採点し、閾値を超えたものだけ人間に回す。
3. **spec/plan 層への前倒し** — コード生成前の意図合意を主戦場にし、そこにも同じトリアージを適用
   （precedented な spec は自動承認、novel なものだけ人間へ）。
4. **事後統計サンプリングによる較正** — 自動承認された変更を無作為抽出で事後監査し、実際の欠陥率を
   測定して自動ゲートの閾値にフィードバックする。
5. **実行時/blast-radius 検証** — マージ前の完全証明を諦め、canary・feature flag・異常検知・
   自動 rollback で事後実証的に検証する。
6. **fleet 単位の相関エラー検知** — 同じ agent/パターンが複数の変更に同種のミスを横展開するリスクを、
   個別レビューではなく fleet 全体の再発パターンとして検知する。
7. **人間レビュー発火時は質問を絞る** — 「全部見て」ではなく「この invariant X との矛盾疑い」など
   具体的な理由付きで回す。
8. **AI による網羅的テスト生成・test-first** — AI は組み合わせ的なテストケース列挙が得意。
   テストコードは逐一人間が追わない。
9. **mutation testing 等によるテストの「歯」の検証** — カバレッジ数値やテスト量ではなく、
   意図的にバグを注入してテストが実際に検知できるかを機械的に測る（reward hacking / 空虚な
   テストの排除）。
10. **テスト生成者と実装者の分離** — 同じ agent が誤った実装と、それを自己確認する誤ったテストの
    両方を書いてしまう循環（reward hacking）を避ける。
11. **リスク階層化された e2e 検証** — ビジネスクリティカルな「コア」フローは毎回実際に visual/
    データ照合まで行い、それ以外は自動 visual diff やサンプリングに委ねる。
12. **意識的な守備範囲の絞り込み** — 「無謬性」ではなく「コアがビジネス上問題なく動く」ことを
    明示的な目標として設定する。

## 現状ステータス総覧（フレームワーク項目ごと・最新）

各項目の状態（COVERED / PARTIAL / IMPLEMENTED-BUT-DISCONNECTED / MISSING）と、対応する修正方針
セクションへのポインタ。**この表がこのリポジトリの「今どこまでできているか」の単一の正典。**
個別の実装詳細・根本原因・受け入れ基準は各項目が指す「修正方針」セクションを参照すること。

| # | 項目 | 状態 | 根拠 / 参照 |
|---|------|------|-------------|
| 1 | 機械的 invariant 層 | COVERED（ただし specguard 側に構造的欠陥あり） | `propguard`/`tdd`/`specguard`/`blastguard`。specguard の polarity gate 自体の欠陥は問題1参照。静的 call-graph 解析は `blastguard::callgraph`（決定論 caller 列挙）＋ `classify_diff_with_callers`（blast-radius シグナル）＋ condukt post-exec 配線（`diffrisk-callgraph` violation）で COVERED（backlog 4e710da1）。 |
| 2 | リスクスコアリング | PARTIAL / 一部 IMPLEMENTED-BUT-DISCONNECTED | `blastguard::classify`（破壊的コマンド）と`diffrisk::classify_diff`（公開シンボル/機微パス）は実装済みだが、後者は本番経路で実 diff を渡されたことがなく到達不能。問題3参照。 |
| 3 | spec 層の段階的トリアージ | PARTIAL | `specguard require_ratification` は 2 値ゲート。問題1の Phase 1（決定論的バックストップ）が段階化への足がかりになる。 |
| 4 | 事後サンプリング較正 | COVERED | `crates/benchkit/src/auditsample.rs` に実装済み：auto-gate（blastguard/propguard/specguard/mutategate）のみを通過した変更群から決定論的乱数サンプリングで抽出し、overwatch の violation stream と change_id/task_key で突合する「auto-approved 変更の無作為抽出監査」ループ。監査の見逃しは (a) 新invariant候補提案 と (b) 閾値調整提案（人間のratifyキュー行き・自動適用なし）の2経路にフィードバックされる。継続運用の原則の反復レビューは精神的に近いが対象が「gate 自身のコード」であり、item 4 本来の対象（ユーザーの変更）とは異なる点に注意——混同しないこと。 |
| 5 | 実行時/blast-radius 検証 | **IMPLEMENTED**（接続に課題あり） | `overwatch::canary`（`b66b3c2`）で実装済み。問題2参照。 |
| 6 | fleet 相関エラー検知 | **IMPLEMENTED**（接続に課題あり） | `overwatch::violation`（`bc50aef`）で実装済み。問題2参照。 |
| 7 | 絞り込みエスカレーション | PARTIAL | `condukt gate check` の `Escalate` は機能するが、item 5/6 の出力と統合されていない。問題3の4番（review queue）参照。 |
| 8 | AI 網羅的テスト生成 | COVERED | `tdd`（RED→GREEN 暗号学的証明）、`specguard testaudit`。変更なし。 |
| 9 | mutation testing | COVERED | `mutategate`（`harness-core` 限定で pilot 中、拡大方針は文書化済み）。変更なし。 |
| 10 | テスト著者と実装者の分離 | PARTIAL | `condukt state verifier-model` はあるが、`tdd` の RED→GREEN は同一 agent 逐次実行のまま。未着手（旧item C）。 |
| 11 | リスク階層化 e2e 検証 | PARTIAL | `trajectoryeval`/`specguard spec-audit` はあるが visual diff/perceptual hash サンプリングは無し。未着手（旧item F）。 |
| 12 | 守備範囲の絞り込み | COVERED | `propguard`/`mutategate` が意図的にスコープ限定。変更なし。 |

## 実装時の注意（リポジトリ規約）

- 各クレートを 1 行でも変更したら、`Cargo.toml` / `plugin.json` / `marketplace.json` の
  version を必ず 3 ファイル同時に micro 以上 bump する（`python3 scripts/check-plugin-versions.py`
  と `python3 scripts/check-version-bumped.py` で機械チェック）。
- 稼働ハーネスへの反映は手動 `cp` 禁止。`scripts/rollout-plugins.sh` を使う。
- 新規/変更 crate は `cargo fmt` + `cargo clippy -p <crate> --all-targets` を green にすること。
- `docs/GLOSSARY.md` に新規クレートの一言説明を追記すること。

## 修正方針（現在の実装対象）

以下は現時点で「まだ実装されていない、または実装が不完全と判明している」項目の修正方針。
いずれも実装は別セッションが行う前提で、症状・根本原因・修正方針・受け入れ基準の形式で書く。

### 問題1（最優先・技術的に解明済み）: specguard polarity gate が構造的にwhack-a-mole状態

**症状**: `crates/specguard/src/similarity.rs`の`polarity_signature`/`polarity_preserved`は、
グレーデッド・ラティフィケーション・ゲートの中核となる「意味論的な逆転（allow↔deny等）を検知して
人間に回す」安全装置である。ところが3回連続の独立レビュー（原型バグ発見→`deed6d4`でfix→再レビュー
で2バリアント発見、いずれも未fix）で、fixするたびに別のバイパスパターンが見つかっている:

1. 原型（review finding 1）: 2クローズ間でallow/denyの**動詞そのもの**を交換 → フラットな
   multiset比較では検知不能 → `deed6d4`で`POLARITY_AXES`によるper-axis順序トラッキングに変更し
   fix。
2. re-review finding 1(a): 動詞はクローズ内の元位置に残したまま**目的語フレーズだけ**を交換
   （`allow the whitespace change` / `deny the substantive rewrite` → 目的語だけ入れ替え）→
   axis内のbucket出現順は不変なので依然検知不能。再現テスト:
   `crates/specguard/src/similarity.rs::zzz_adversarial_probe_object_phrase_swap_still_bypasses`
   （`#[ignore]`、現在実際にFAILし再現済み）。
3. re-review finding 1(b): 各axisで出現数が1回しかないトークンを**異なるaxis間で**交換
   （modal軸の`require`とauthz軸の`forbid`を入れ替え）→ 各axisのシーケンスは単一要素のままなので
   不変 → 依然検知不能。再現テスト:
   `...::zzz_adversarial_probe_cross_axis_single_occurrence_swap_still_bypasses`（同上）。
   構築時の注意点として、`sign`が`POLARITY_TOKENS`の`approve`バケット（authz軸）に既に
   マップされているため文中に迂闊に"sign off"を使うと意図せず追加のauthzトークンが混入し
   axisシーケンスが変化して再現できなくなる（最初の試行でこれにより誤ってテストが"ok"に
   なった）。同様に`must`のような他のmodal語彙を padding 文に混入させると axis の出現順が
   実際に変化してしまい、これも誤って"ok"になる（同一axisに3出現かつ順序が変わるケースは
   既存の順序トラッキングで正しく検知されてしまうため）。今後同ファイルを触る際、
   `POLARITY_TOKENS`の全リストを確認せずにテスト文を書くと同じ失敗を繰り返す。

**根本原因（3回の検証で一貫して特定された共通点）**: `polarity_signature`は「どのaxisに、どういう
順序でどのbucketが出現するか」しか追跡しておらず、**そのpolarityトークンがテキスト中のどのクローズ
／目的語を修飾しているか**への束縛が一切ない。個別バリアントを塞ぐたびに「束縛していない別の次元」
が新たなバイパス経路になる——これはパッチの継ぎ足しでは収束しない構造的な設計限界であり、次に
「fix」する前に一段深いデータ構造への変更が必要と判断する。

#### 実装方針（確定・優先順位を明示） — 設計原則: 無謬性を目指さない

前セクションで検討した「ローカル文脈への束縛によるコア修正」は、依然としてヒューリスティックの
一種であり、将来また別のバリアントが見つかる可能性を否定できない（3回連続でそれが実際に起きた
という実績がある）。**したがって、このゲートの安全性を「ヒューリスティックが完璧であること」に
依存させる設計は採用しない。** 代わりに、以下の設計原則で実装する:

> ヒューリスティックにレビューの精度向上（＝人間に回す頻度を下げること）は期待してよいが、
> ヒューリスティックに安全性の担保（＝危険な編集を見逃さないこと）を依存させてはならない。
> 安全性は、粗くても回避しにくい決定論的なルールが担保し、そのルールが「どの時点で人間が
> 何を確認するか」の境界線を明示する。ヒューリスティックはその境界線の内側で人間の負荷を
> 減らすための最適化に徹する。

この原則に従い、**Phase 1（バックストップ）を先に実装し、それだけで単独でマージ可能な状態に
する。Phase 2（局所文脈フィンガープリント）はPhase 1の上に乗せる精度改善であり、実装しなくても
安全性上の欠陥にはならない任意の後続作業とする。** さらにPhase 1・2はいずれも「一度実装したら
完成」という静的な成果物ではなく、後述の「継続運用の原則」（旧称 Phase 3）を伴って初めて信頼
できるという設計に改めた。決定性のあるPhase 1・2は「既に知っている見落としを二度と繰り返さない」
ためのものであり、「まだ知らない見落とし」を発見する仕事は継続運用の原則が担う——両者は代替関係
ではなく分業関係にある。

##### Phase 1（必須・最優先）: 決定論的バックストップ — 安全性を担保する本体

`crates/specguard/src/similarity.rs`の`triage`（または`polarity_preserved`の直前）に、
類似度やaxis比較の結果を一切参照しない、単純な出現数カウントのハードルールを追加する:

- **ルール**: `ratified`（precedent）と`candidate`のいずれかで、authz軸
  （`allow`/`deny`/`forbid`/`approve`の各bucket合計）またはroute軸（`human`/`auto`の各bucket
  合計）に属するpolarityトークンの出現回数が**2以上**の場合、`polarity_preserved`の結果に
  かかわらず常に`Verdict::Novel`を返す（＝必ず人間に回す）。
- 実装場所の目安: `polarity_signature`が返す`BTreeMap<&'static str, Vec<&'static str>>`から
  `sig.get("authz").map(|v| v.len()).unwrap_or(0) + sig.get("route").map(|v| v.len()).unwrap_or(0)`
  を計算し、2以上なら`triage`が`polarity_preserved`の判定を待たずに`Novel`を返す新しい早期
  リターンを追加する形が自然（`polarity_signature`は既にprivateなヘルパーとして存在するので
  そのまま再利用できる）。
- **これはヒューリスティックではなく決定論的カウント**なので、バイパスの「新バリアント」が
  将来見つかる余地がない。回避する唯一の方法は「authz/route軸のトークンを1テンプレート中に
  1回以下に抑える」ことだが、それ自体が複数の認可判断を1度の編集で混在させないという健全な
  制約であり、抜け道として機能しない。
- **既存の望ましい動作を壊さないための確認**: 単一のauthz/routeクローズしか持たない既存の
  benignなratify編集（`benign_reword_with_unchanged_polarity_still_auto_ratifies`、
  `synonym_swap_within_same_bucket_preserves_polarity`等）はauthz/route軸の出現数が1以下
  なのでこのルールの影響を受けない。影響を受けるのは「1テンプレート中に2つ以上のauthz/route
  判断を書く」template（＝review findingが実際に使った"2クローズ"構成そのもの）のみで、これは
  意図した挙動。
- **このルールが閉じる範囲（重要・過大に見積もらないこと）**: このバックストップは
  **同一axis内の出現数が2以上になる**バリアントのみを検知する。具体的には、原型のfinding 1
  （2クローズ間でallow/denyの動詞を交換 — authz軸に2出現）と、re-review finding 1(a)
  （目的語スワップ — 同じくauthz軸に2出現）はこのルールだけで確実に`Novel`へ倒れる。
  **一方でre-review finding 1(b)（`require`と`forbid`のような異axis・単発出現同士の交換）は
  このルールでは検知できない** —— 交換後もmodal軸・authz軸それぞれの出現数は1のまま変わら
  ないため、単純カウントの土俵に乗らない。異axisのカウントを合算する拡張（例:
  「authz+route+modalの合計が2以上なら常にNovel」）も検討したが、これは`"the policy must
  forbid untested changes"`のような、ごく普通の1クローズ・良性フレーズ（modal+authzが自然に
  同居する）まで誤検知してしまうため採用しない——false positiveのコストが高すぎる。
  finding 1(b)はPhase 1では**未解決のまま**とし、Phase 2で閉じる（下記参照）。
- **受け入れ基準（Phase 1完了の定義）**: 既存の
  `zzz_adversarial_probe_two_clause_pole_swap_routes_to_human`（既にgreen。理由をこのルール
  でも再確認する）と、re-review finding 1(a)の再現テスト
  `zzz_adversarial_probe_object_phrase_swap_still_bypasses`の`#[ignore]`を外した状態で
  **green**になること。finding 1(b)の再現テスト
  `zzz_adversarial_probe_cross_axis_single_occurrence_swap_still_bypasses`はPhase 1の時点では
  **`#[ignore]`を外さない**（Phase 1だけでは閉じないことが分かっているため）。

##### Phase 2（finding 1(b)の解決に必須。それ以外は精度改善）: ローカル文脈フィンガープリント

Phase 1だけでfinding 1・1(a)の安全性は担保されるが、finding 1(b)（異axis・単発出現の交換）は
純粋なカウントベースの決定論的ルールでは、良性フレーズを誤検知するコストなしに閉じることが
**原理的にできない**（上記の理由）。finding 1(b)を閉じるには、polarityトークンをその局所文脈
（どのクローズ・どの目的語を修飾しているか）に束縛する以下の仕組みが必須になる。加えて、
authz/route軸のトークンが2回以上出現する**正当な**benign編集がPhase 1で一律Novelに倒れて
しまう副作用（人間負荷の増加）を軽減する効果もある:

- `polarity_signature`の型を `axis -> Vec<bucket>` から
  `axis -> Vec<(bucket, local_context_fingerprint)>` に変更する。`local_context_fingerprint`は
  該当polarityトークンの出現位置を中心としたローカルウィンドウ（前後K語、または`and`/`but`/
  `because`/`;`等の決定論的な接続詞リストで区切ったクローズ相当の範囲）から、polarityトークン
  自体をマスクした上で計算するshingle集合（既存の`shingles`/`jaccard`を再利用）。
- `polarity_preserved`は、まずaxis毎のbucket個数一致を確認し、次に同一axis・同一bucketの出現
  同士をローカルウィンドウのJaccard類似度でgreedyに最良マッチングする（決定論性のため類似度
  降順→出現順でtie-break）。マッチ類似度が閾値（初期値目安0.5、要キャリブレーション）を下回る、
  またはマッチしない出現が残る場合は`Novel`。
- **Phase 2はPhase 1のバックストップを置き換えない。** Phase 2導入後も、Phase 1のハードルールは
  そのまま残す（多重防御）。finding 1・1(a)については、Phase 2の判定結果によらずPhase 1が
  常に最終防御として機能する。finding 1(b)については、**Phase 2そのものが安全性を担保する
  唯一の機構**であり、その意味でfinding 1(b)に限りPhase 2は「精度改善」ではなく「安全性の
  実装」である——Phase 1/2の優先順位を切り替えた設計原則（バックストップ＝安全性,
  ヒューリスティック＝精度改善）を厳密に貫くなら、finding 1(b)級の脆弱性は本来Phase 1側で
  閉じたいところだが、上記の通りコストに見合う決定論的ルールが存在しないため、本findingに
  限っては例外としてPhase 2に安全性の責務を負わせる、という判断であることを実装者は明示的に
  認識しておくこと。
- **キャリブレーションの難所（finding 1・1(a)に限れば実害が小さい）**: Phase 2の類似度閾値を
  下げすぎてfinding 1・1(a)相当のバイパスが再びPrecedentedとして通ってしまっても、Phase 1の
  バックストップが最終的にNovelへ倒すので実害はPhase 1が吸収する。**しかしfinding 1(b)は
  Phase 1の対象外なので、Phase 2の閾値ミスがそのままfinding 1(b)の再発に直結する** ——
  finding 1(b)のキャリブレーションだけは通常より慎重に行うこと（閾値を保守的寄り＝高めに
  設定し、多少人間負荷が増えてもバイパスを優先して塞ぐ）。
- **受け入れ基準（Phase 2完了の定義）**: (1) Phase 1のテストが引き続き全てgreenのまま
  であること。(2) finding 1(b)の再現テスト
  `zzz_adversarial_probe_cross_axis_single_occurrence_swap_still_bypasses`の`#[ignore]`を
  外した状態でgreenになること（これがPhase 2完了の必須条件）。(3) Phase 1のみでは過剰に
  Novelへ倒れていた既知のbenignケース（新規に追加してよい）がPrecedentedに戻ること
  （任意・精度改善の確認）。加えて、3回連続で手動列挙のたびに新バリアントが見つかったという
  実績を踏まえ、**`proptest`（`crates/specguard/proptest-regressions/`は既存で依存関係も
  導入済み）でランダムなクローズ/目的語の並べ替え・軸またぎのトークン交換をfuzzし、
  `similarity(candidate, ratified) >= threshold && polarity_preserved(candidate, ratified) ==
  true`かつ実際には意味が反転しているケースが生成されないことを性質として検証するテストを
  最低1本追加する**（手動列挙の限界を機械的fuzzingで補う）。

##### この2フェーズ構成が体現する設計思想

Phase 1は「この編集は人間が確認すべきか」を無謬性なしに、決定論的かつ説明可能な基準
（同一axis内のトークン出現数）だけで先に確定させる——これが「人間がどの時点で何を確認する
必要があるか」を明示するということの具体的な実装である。この基準で閉じられる範囲
（finding 1・1(a)、いずれも「1つのaxisに複数の意思決定が混在する」という同じ形をした
バイパス）については、Phase 2の実装状況や精度に関係なく安全性が保証される。

一方でfinding 1(b)（異axis・単発出現の交換）は、決定論的カウントだけでは良性フレーズとの
判別がつかず、Phase 2のローカル文脈束縛なしには原理的に閉じられないことが判明した——これは
「粗いルールで全てを塞ぐ」という当初の期待が万能ではなく、**一部の脆弱性クラスは局所文脈の
判定というヒューリスティックそのものに安全性を委ねざるを得ない**という限界を示している。
この限界は隠さず、finding 1(b)がPhase 2完了までの間は既知の未解決リスクとして残ることを
このドキュメントおよびコード中のコメントで明示すること。それでもなお、finding 1・1(a)という
最も直接的で実際に実演されたバイパス（3回のレビューで一貫して悪用可能と証明された経路）が
Phase 1だけで無謬性なしに閉じられる、という点が今回の設計変更の主要な成果である。

### 問題1b: specguard ratify lock の書き込み失敗が無音化する

**症状**: `crates/specguard/src/ratify.rs`の`write_lock`は`harness_core::store::save_bytes`
（atomic tmp+rename方式だが、内部エラーをfail-softに`let _ = ...`で握り潰す設計）を呼び、
成功判定を`path.exists()`のみで行っている。既存ロックを**再ratify**する（＝一般的なケース）際に
`save_bytes`が権限エラー・ディスク満杯等で書き込みに失敗しても、古いファイルがそのまま残っている
ため`path.exists()`はtrueを返し続け、`write_lock`は誤って成功を報告する。検証テスト
`write_lock_reports_error_when_write_silently_fails`（`crates/specguard/src/ratify.rs`、
`#[ignore]`）がロックパスをあらかじめディレクトリで占有しておく（＝`rename`が必ず失敗する）
ことでこれを決定論的かつクロスプラットフォームに再現しており、現在実際にFAILすることを確認済み。

**根本原因**: `save_bytes`のfail-soft設計（hookパスでエラーがターン全体をブロックするより、
ログ欠落の方がまし、という判断）は多くの呼び出し元では正しいが、**ratifyのlockファイルは
「この変更を安全とみなした」という監査証跡そのもの**であり、書き込みが黙って失敗すると監査証跡
が実は存在しないのに存在するとみなされる——fail-softの前提（「多少のログ欠落は許容できる」）が
成り立たない数少ない呼び出し元。

**修正方針**: `write_lock`専用に、書き込み結果を実際に検証するchecked writeへ切り替える。
選択肢は2つ: (a) `harness_core::store`に`save_bytes`とは別に`Result<()>`を伝播する
`save_bytes_checked`（内部の`rename`エラーをそのまま`?`で返す）を追加する、(b) `write_lock`
側で`save_bytes`呼び出し後に書き込んだ内容のハッシュ（`harness_core::hash::fnv1a64`、
`ratify.rs`は既にimport済み）を実ファイルから再読み込みして期待値と比較し、不一致なら`Err`を
返す事後検証を追加する。(a)は汎用性が高いが`store`モジュールの契約変更になり影響範囲の確認が
必要、(b)は`ratify.rs`内で閉じるため最小侵襲。どちらを採るかは実装セッションでの判断とするが、
**`path.exists()`だけの成功判定は今後この用途では使わない**ことが必須要件。

**受け入れ基準**: `write_lock_reports_error_when_write_silently_fails`の`#[ignore]`を外した
状態でgreenになること。

### 問題1c: stuckguard near-repeat が window境界を超えると失速する

**症状**: `crates/stuckguard/src/detect.rs`の`Trip::key`は`same.first()`（現在のwindow内で
最古の一致イベント）に由来する。`SessionState::push`（`crates/stuckguard/src/state.rs:53-65`）
はwindow（既定`window=12`）が埋まると最古のイベントを追い出すため、near-repeat系列がwindow長を
超えて継続する場合（＝stuckguardが本来検知したい「長時間のスタックループ」そのもの）、pushの
たびにアンカーが入れ替わり`same.first()`が別のsignatureのイベントへローリングし続け、nudge
countが繰り返し1にリセットされる。到達可能な最大nudge countは`window - repeat_threshold + 1`
（既定値で10）で頭打ちになり、`escalate_after`に到達できない。検証テスト
`near_repeat_escalates_even_past_window_boundary`（`crates/stuckguard/src/detect.rs`、
`#[ignore]`）が`escalate_after=11`・30イベントのループで現在実際にFAILすることを確認済み。

**根本原因**: エスカレーションのカウンタが「window内に今どのイベントが残っているか」という
window境界に強く依存した実装になっており、windowという実装都合の概念と、「同じ問題が何回連続で
起きているか」という意味論的なカウントが分離されていない。

**修正方針**: signatureごとのエスカレーションカウンタを、window境界とは独立に永続化する。
具体的には`SessionState`に（signatureをキーとした）「連続一致streakの開始時刻」と「streak内の
累積出現数」を保持するフィールドを追加し、新規イベントが直前のsignatureと一致する限りwindowから
の追い出しに関係なくカウントを増やし続け、一致しないイベントが来て初めてstreakをリセットする。
windowは「similarity判定に使う直近イベントの比較対象範囲」としての役割に限定し、「エスカレー
ション到達済みかどうか」の判定からは切り離す。

**受け入れ基準**: `near_repeat_escalates_even_past_window_boundary`の`#[ignore]`を外した状態
でgreenになること。既存の（window境界に達しない短いループでの）near-repeatテストも引き続き
greenであること。

### 継続運用の原則: 決定性はテストに固定化し、非決定性は発見エンジンとして分離する

（旧称 Phase 3。問題1のPhase 1/2に限らず、問題1b/1c/2/3すべてに横断して適用される運用原則
なので独立したセクションに格上げした。）

決定論的な修正はいずれも「静的に実装して完成」という成果物ではない。今回のspecguard polarity
ゲート自体の3回連続whack-a-moleが示した通り、**決定論的なロジックは「まだ知らない見落とし」を
自力では発見できない**——finding 1(a)・1(b)はいずれも、決定論的ロジックを机上でこねくり回す
ことではなく、非決定的な多視点AIレビュー（finder→verifier→再レビューの反復）が探索的に見つけた
ものであり、しかもこの反復は**単一箇所（specguardのpolarity gateだけ）ではなく、リポジトリ内の
複数のgate/security-relevantな箇所を横断的に見て回ることで見落し確率を下げる**、という性質の
ものだった（実際、今回の2周の`/code-review`はspecguard/propguard/blastguard/tdd/trajectoryeval/
rollout-plugins.shと複数crateを横断していた）。この経験から、個別の修正を実装して終わりに
せず、以下を恒常的な仕組みとして追加する:

- **原則**: 「決定性はテストに置き換えて評価する」。問題1のPhase 1バックストップ・Phase 2の局所
  文脈ヒューリスティック、問題1b/1cの修正は、いずれも実装した瞬間からその正しさをユニット
  テスト・proptestで固定化する（既にそうしている）。決定論的コードの役割は「一度発見された
  見落としを二度と繰り返さないこと」に限定し、それ以上の期待（＝まだ見つかっていない見落とし
  も防げるはず、という期待）を決定論的コードに背負わせない。
- **非決定性（AIレビュー）の役割**: 「まだテストに固定化されていない見落とし」を継続的に探す
  発見エンジンとして、決定論的コードとは明確に別の仕組みに切り出す。具体的には、以下を満たす
  継続的な敵対的レビューの仕組み（新規skill/workflow、あるいは既存の`mutategate`
  ——構文変異でテストの網羅性を検査する仕組み——の設計を流用した新crateとして実装する案が
  現実的）を追加する:
  1. **単発ではなく反復実行**: 1回の`/code-review`で「完了」とみなさず、一定の周期（例:
     gate関連ファイル——`crates/specguard/src/similarity.rs`/`ratify.rs`、
     `crates/blastguard/src/diffrisk.rs`、`crates/tdd/src/proof.rs`、`crates/condukt`の
     gate統合ロジック等——に変更が入るたび、または定期cron）で繰り返し起動する。
  2. **単一箇所ではなく複数箇所を横断**: 1回の起動で1つのgateだけを見るのではなく、
     リポジトリ内の「security/gate-relevant」なcrateの集合をローテーションまたは並行に対象と
     する（既存の`docs/GLOSSARY.md`のgate関連crate一覧を対象リストの初期値として使える）。
  3. **非決定性を許容し、偽陽性は再検証で吸収する**: 各ラウンドで複数の独立したAIエージェントに
     「まだテストが捕捉していないバイパス／見落としを探せ」と指示し（今回の finder 役に相当）、
     見つかった候補は別の独立したAIエージェントで再検証する（今回の verifier 役に相当、
     REFUTEDは捨てCONFIRMED/PLAUSIBLEのみ残す）。1ラウンドの結果を鵜呑みにせず、複数ラウンド
     ・複数エージェントを跨いだ収束（同じ問題が繰り返し浮上するか、それとも一度きりで消えるか）
     で確度を判断する——今回の2周の`/code-review`が実際にこの収束パターンを示した
     （1周目19件→2周目は主に1周目の修正の不備7件、新規の完全に独立した発見は少数）。
  4. **発見は必ずテストへ還元してループを閉じる（決定性への固定化）**: このラウンドでCONFIRMED
     と判定された発見は、放置せず**必ず**その場で再現テスト（今回`#[ignore]`付きで追加した
     4本と同じパターン）としてコード化する。これにより次回以降のラウンドは同じ見落としを
     二度と探す必要がなくなり、AIレビューの探索範囲は常に「まだテスト化されていない未知の
     領域」に絞られていく——ラウンドを重ねるほど新規発見数が減っていくことが期待される
     （減らなければ、そのgateの決定論的コアの設計自体を見直すシグナルとして扱う。今回の
     specguard polarityゲートはまさにこのシグナルが3回連続で出た結果、Phase 1/2の構造変更
     という判断に至った）。
  5. **メトリクスとして記録する**: ラウンドごとの「新規発見数」「再発見数（過去に見つかったが
     まだテスト化されていなかったもの）」「再検証で生き残った率」を記録し、時系列で新規発見数が
     減少傾向にあるかを追跡する。減少しない・増加するcrateがあれば、そこは決定論的コアの構造的
     欠陥を疑う優先対象として扱う——これはフレームワークitem 4（事後サンプリング較正ループ）の
     具体化そのものであり、item 6（fleet相関エラー検知）とも接続する（同一パターンが複数の
     独立したラウンドで繰り返し検出される＝相関エラー）。
  6. **収束性について（確定・追加の実証は要さない）**: 「ラウンドを重ねるほど新規発見数が減る」という
     上記の期待は、実証待ちの仮説ではなく**発見→テスト化というループ構造自体から論理的に導かれる
     帰結**として扱ってよい。CONFIRMEDと判定された発見は原則4により必ずその場でテスト化される
     ため、「まだテストに固定化されていない未知の領域」の集合はラウンドを経るごとに単調非増加に
     なる——これは構成上保証される性質であり、今回の2周の`/code-review`の実績（1周目19件→2周目
     7件）はこの帰結と整合する一データ点ではあっても、収束性そのものの証明ではないし証明を必要と
     しない。したがって設計上残る自由度は「収束するかどうか」ではなく**収束の速度・カバレッジ**
     （1ラウンドあたり何crateを対象にするか、周期をどう設定するか）だけであり、これは上記の
     メトリクス（新規発見数の時系列）を見ながら運用開始後にチューニングすればよいパラメータで
     あって、実装着手を妨げる未解決の前提条件ではない。
- **起動方式は実装セッションでの設計判断が必要（未確定・要検討）**: 選択肢は主に2つ。
  (a) このリポジトリ自身がClaude Code環境で使えるscheduled-task/cron機能
  （`backlog`のSessionStart連携等、既存のsubscription-native設計思想に沿う）でユーザー環境の
  Claude Codeセッションとして定期実行する案、(b) `.github/workflows/mutation.yml`と同様に
  CIでheadless実行する案（ただしAPIキー課金が発生し、このリポジトリの「subscription-native、
  APIキー不要」という既存方針と衝突する可能性があるため要合意）。どちらを選ぶか、あるいは
  両方を用途で使い分けるかは実装セッションでユーザーに確認すること。問題3の4番（review queue）
  が実現すれば、どちらの起動方式を選んでも人間が見る場所は1つに集約されるため、この論点の
  重要度自体は下がる。
- **人間の役割（変わらない）**: この仕組み全体の目的は人間を仕組みから排除することではない。
  この規模・頻度の探索的レビューを人間が直接行うことは理論的に不可能ではないが、量的に
  現実的でない、というのが導入の理由である。人間は、Phase 1/2が機械的に確定させた境界線
  （＝常にNovelになる構成）でのratify判断と、複数ラウンドを経てCONFIRMEDと確信した
  発見への対応、という**絞り込まれた確認点**に専念できるようにする。これが「HITLを排除せず、
  高生成量下でもレビューによって開発を維持する」という目的の具体的な実装形である。

### 問題2: item 5/6は「設計未着手」ではなく、既に実装済み——ただし接続が欠けている

**このセクションは過去の版で「設計未着手」と記述していたが、それは事実誤認だった。** 実装
セッションが着手する前に必ず`crates/overwatch/src/canary.rs`・`crates/overwatch/src/violation.rs`
を先に読むこと。以下の2機能はすでにコード・テスト・CLI・シェル統合まで揃っている:

- **item 5（runtime/blast-radius検証）は`overwatch::canary`として実装済み**
  （commit `b66b3c2` "feat(overwatch): opt-in canary staged rollout core + rollout-plugins.sh
  --canary (Item G)"）。`Stage`/`StagePlan`によるステージ分割、`HealthGatePolicy`/`GateDecision`
  による健全性判定（`decide_from_count`・`evaluate_health_gate`・`evaluate_health_gate_systemic`）、
  `compute_rollback_plan`によるロールバック計画の3つが純粋関数として揃い、`canary_cli.rs`が
  CLI化し、`scripts/rollout-plugins.sh --canary`（448〜636行目付近）が実際にステージごとに
  「適用→`overwatch canary-gate`で健全性確認→閾値超過ならその場で自動ロールバック」を実行する。
  dry-runだけでなく実運用パス（563行目`gate_out="$("$ow" canary-gate --threshold
  "$canary_threshold")"`）も`overwatch`のviolationレジストリを実際に読む——スタブではない。
- **item 6（fleet相関エラー検知）は`overwatch::violation`として実装済み**
  （commit `bc50aef` "feat(overwatch): add fleet-level correlated gate-violation detection"、
  および`01d036a`の discriminator 厳格化）。`ViolationSource`（blastguard/propguard/specguard/
  mutategate）ごとの`RawViolation`から`normalize_signature`で共通シグネチャ空間に正規化し、
  `RecurrencePolicy`（既定: 24h窓で3回以上）で`detect_recurrence`/`systemic_issues`が
  「同一シグネチャが複数タスク・複数セッションに跨って再発したか」を判定する。specguard/
  propguard/blastguard/mutategateの各バイナリが実際に`append_violation`を呼んでおり
  （`crates/{specguard,propguard,blastguard,mutategate}/src/main.rs`）、`violation_cli::
  print_recurrence`/`print_escalations`がCLIから問い合わせ可能。

**したがって「5と6はつくる」という指示に対する正確な回答は「作る」ではなく「繋ぐ」である。**
実際に手を動かして確認した結果、次の4つの具体的な接続漏れが残っている。これらは新機能の設計
ではなく、既存の決定論的コンポーネント同士を結線する作業なので、問題1より実装コストは低い:

1. **canary health gateがitem 6のsystemic判定を使っていない**: `rollout-plugins.sh`の実運用パス
   （563行目）は`canary-gate --threshold N`のみを呼び、`--systemic`を渡していない。つまり
   ロールバック判定は生の違反件数（`evaluate_health_gate`）のみで行われ、`evaluate_health_gate_
   systemic`（item 6のfleet相関ロジック）は素通りされている。無関係な単発ノイズが閾値を超えれば
   誤ってロールバックし、逆に低頻度だが複数セッションに跨る真の相関パターンは件数閾値に達しない
   限り見逃される。**修正方針**: `canary_cli::gate`に「raw spike **OR** systemic recurrence」の
   いずれかでロールバックする結合判定を追加し（`evaluate_health_gate`と`evaluate_health_gate_
   systemic`を両方評価してORするだけの薄いラッパー、既存の2関数は変更不要）、
   `rollout-plugins.sh`の実運用パスをこの結合判定を使うよう既定で切り替える。
2. **health gateの計測窓がステージ適用時刻に紐付いていない**: `evaluate_health_gate`系は
   「`now`から遡って`window_secs`以内」のイベントを数えるだけで、「このステージを適用した時刻
   以降」という起点を持たない。ステージ適用の直前に無関係な違反が既に発生していた場合、それが
   今回のcanary版由来であるかのように誤って集計されうる。**修正方針**: `canary.rs`の関数群に
   `since: Option<i64>`引数を追加し（`ts < since`のイベントを除外するだけの純粋な拡張、既存の
   テストは壊れない）、`rollout-plugins.sh`がステージ適用直前のタイムスタンプを記録して
   `--since`として渡すようにする。
3. **`--canary`はopt-inのままで、既定の運用では素通りされる**: このリポジトリ自身のCLAUDE.md
   が例示する標準コマンド（`scripts/rollout-plugins.sh --plugin <name>`）はcanaryを使わない。
   item 5の仕組みは実装されているが、人間が`--canary`を明示的に付け忘れれば発動しない。
   **修正方針**: 対象plugin集合が「gate関連crate」（`docs/GLOSSARY.md`のgate系crate一覧、
   継続運用の原則が周回対象とする集合と同じにできる）と重なる場合は`--canary`省略をエラーにし、
   意図的に外すときだけ`--no-canary`で明示的に上書きさせる（安全側デフォルト）。CLAUDE.mdの
   例示コマンドも合わせて更新する。
4. **item 6のsystemic検知に「起動して知らせる」経路がない**: `overwatch violations --systemic`
   （実体は`print_escalations`）は問い合わせ待ち（pull）であり、誰かが手動で叩かない限り
   SYSTEMIC判定は誰の目にも触れない。これはitem 7（絞り込まれたエスカレーション）との接続が
   欠けているということであり、次の問題3でまとめて扱う。

### 問題3: 変更流量を裁くパイプライン（item 2/3/7）が実際には未接続

問題1・2で確認した通り、決定論的な部品（specguardのpolarity gate、blastguardのrisk分類、
overwatchのcanary/systemic検知）は個々には存在する。しかし**1日1万行という流量を実際に
「どれだけ人間に回すか」を決めるパイプライン全体としては繋がっていない**。

**現状の断絶点（コードを実際に確認して特定したもの）**:

- `blastguard::diffrisk::classify_diff`（公開シンボル差分・機微パスglobを検知するriskシグナル）
  は実装・テスト済みだが、実際の呼び出し元は`crates/condukt/src/gate_exec.rs`の
  `gather_assessment`と`crates/condukt/src/schedule.rs`の`schedule`の2箇所のみで、**どちらも
  `diff_text`に空文字列`""`を固定で渡している**（`gate_exec.rs`76〜88行目、`schedule.rs`
  255〜285行目、両方のdoc commentが明記する通り「タスク実行前でdiffがまだ存在しない」ことが
  理由——これはバグではなく、ゲートが実行を許可するかどうかを**実行前**に決める必要があるという
  構造上の制約から来ている）。結果として、「公開APIが変わるほど大きい変更か」という最も重要な
  risk シグナルの一つが、テスト以外のどの経路からも実diffを渡されたことがない。
- item 3（spec層の段階的triage）はspecguardのpolarity gate/ratifyフローとしてかなりの程度
  実装されている（問題1参照）。item 7（絞り込まれたエスカレーション）は
  conduktのgate checkが`Escalate`判定として持っている。だが両者と、問題2で見たoverwatchの
  systemic検知・canaryのrollback通知は、**互いに独立した出力先を持ち、1箇所に集約されていない**
  ——人間が確認すべき場所が最大3箇所（condukt gateのEscalate出力、`overwatch violations
  --systemic`の手動実行結果、canary rollbackのシェル出力）に分散している。

**修正方針（4段階、依存順）**:

1. **conduktにpost-execution diff-risk再分類を追加する**（item2/3の未接続を閉じる）:
   gated/auto-execされたタスクが完了した時点——`checkpoint.rs`が既にdurable run checkpointを
   書いている直後のタイミングに新しいステップとして追加する——で、実際に`git diff`をタスクの
   worktree・`touched_files`に対して取得し、`blastguard::diffrisk::classify_diff`に実diffを
   渡して再分類する。これは実行を止める事前ゲートではなく（既に実行済みのため止めようがない）、
   **事後のrisk再評価**として位置づける: 事前予測（sensitive-path globのみ）より実際のdiffが
   high-riskだった場合（例: 公開シンボルの変更を検知）、`overwatch::violation`へ1件記録する
   （`ViolationSource::Blastguard`に新しいdiscriminator、例えば`"diffrisk-public-symbol"`を
   追加する形で、既存のスキーマ変更なしに乗せられる）。こうして初めて、事前予測が外れた
   ケースが自然にitem 6のfleet相関検知の対象に入る。受け入れ基準: (a) `classify_diff`が実diff
   を渡されて呼ばれる統合テストを新規に追加し、現状の空文字列呼び出し2箇所とは別の実行パスで
   あることを明示する、(b) 高リスク判定時に`overwatch`へ1件記録されることを確認する。
2. **canary health gateとitem 6のsystemic判定を結合する**（問題2の1・2番で述べた修正そのもの）。
3. **gate関連crateのロールアウトで`--canary`を既定必須にする**（問題2の3番で述べた修正
   そのもの）。
4. **単一の人間向けエスカレーション面（review queue）を作る**——「3のレビューの戦略」
   への直接の回答: 新しいCLIサブコマンド（例: `overwatch review-queue`、既存の`overwatch`に
   追加するのが自然——violationレジストリを既に持っているため）を用意し、次の3系統の出力を
   1つのリストにマージして表示する。
   - (a) `overwatch violations --systemic`（item 6、既存のまま利用）
   - (b) 直近のcanary rollbackイベント（item 5、新設: rollbackが発生したら`canary_cli::gate`が
     `overwatch`へも1件記録するよう拡張する——ステップ1で追加するdiffrisk記録と同じ経路を使う）
   - (c) 継続運用の原則によるAIレビューラウンドでCONFIRMEDと判定されテスト化された発見の一覧
     （記録先は未定義——継続運用の原則の実装時に同じ`overwatch::violation`スキーマへ
     `ViolationSource`の新バリアントを追加するか、別の記録先を設けるかは実装セッションでの
     判断に委ねる）
   人間はこの1コマンドだけを定期的に（あるいは継続運用の原則で既に触れたcron/scheduled-task
   経由で自動的に）確認すればよい状態を目指す。

**優先度**: 問題1（specguard polarityゲートの構造修正）を最優先とすることは変わらない——既存の
セキュリティゲートが実際に機能しているかという信頼性の欠陥だからである。問題1b/1cは同じ
ラウンドで見つかった具体的な退行バグであり、問題1の直後に着手する。問題2の4項目・問題3の
4段階はいずれも「既存の決定論的部品を正しく繋ぐ」作業であり、新規のアルゴリズム設計を要しない
分、問題1より実装リスクは低いが、複数crate（specguard/blastguard/condukt/overwatch）を跨ぐため
変更範囲は問題1より広い。着手順の推奨: 問題1 → 問題1b・1c → 問題3の1（diff-risk再接続） →
問題2の1・2（＝問題3の2・3、canary×systemic結合） → 問題2の3（`--canary`既定化） →
問題3の4（review queue集約、他の全ステップの出力を消費するため最後）。

## 既知の軽微な未着手課題（低優先）

いずれも安全性に関わらない重複/デッドコードの指摘。修正は上記の問題1〜3より優先度が低い。

- **tdd**: `green()`の新規チェック（`strict_separation`前の`has_red`チェック）が
  `judge_green()`内の既存チェックと重複し、後者がデッドコード化している
  （`crates/tdd/src/proof.rs:52-55,274`）。
- **propguard**: `emit_overwatch_violations`が`overwatch::store::append_violation`の書き込み
  シーケンスをprivateにインライン複製している（`crates/propguard/src/main.rs:284`）。
  省略された`signature_is_bucketable`ガードは上流の`normalize_signature`が既に保証しているため
  安全（正しさではなく保守性の問題）。
- **stuckguard**: near-repeatキー導出の`.unwrap_or(cur.sig.as_str())`フォールバック節が
  到達不能なデッドコード（`crates/stuckguard/src/detect.rs:120`）。挙動リスクはないが、
  将来の読み手が実在するエッジケースの処理と誤読するおそれがある。
- **harness-core**: `jaccard()`が非`pub`のため`stuckguard`/`specguard`がそれぞれ独自実装して
  いる。共有化するなら`harness-core`側を`pub`にする必要がある（低優先の重複解消）。

## アーカイブ: 実施済みレビューの記録

以降は過去のレビューラウンドの実施記録。**現在アクションが必要な内容はすべて上記の
「修正方針」セクションに反映済みであり、このアーカイブは経緯の参照用。**

### 実装レビュー結果（2026-07-09、コミット範囲 `9c70015..HEAD`）

別セッションによる実装がmainにマージされた後、`/code-review`（high effort、8観点 finder × 1票
verify、recall優先）でレビューを実施。20件の候補のうち18件がCONFIRMED、1件PLAUSIBLE、1件REFUTED。
**2026-07-10、コミット`deed6d4..114f1ae`で17件（1〜17）を全て修正済み。** REFUTED1件
（harness-core `jaccard`共有化、非pub関数なので「既存流用を怠った」という主張自体が不成立）は
上記「既知の軽微な未着手課題」に集約済み。触った6クレート（specguard 0.2.18 / propguard 0.1.4 /
stuckguard 0.1.9 / blastguard 0.1.7 / tdd 0.1.8 / trajectoryeval 0.1.6）はversionを3ファイル
lockstep bump。全crateでfmt/clippy/test green、version整合ゲートexit 0を確認済み。
finding 8（blastguard item Dの本番未到達）は当時「condukt本体への配線をスコープ外」とし
コメント＋単体テストで現実的な到達点を固定したのみで、その後の再レビュー（finding 4として）で
未修正のまま残っていることが判明——現在は問題3が正式な修正方針。

CLAUDE.md規約監査: バージョン整合性（3ファイルlockstep）は39プラグイン全て整合、違反なし。
item B/C/D/E（fleet相関エラー検知/テスト-実装分離/リスクスコアリング一般化/spec段階トリアージ）
は当時、doc が指示した既存機構へ正しく接続されており disconnected な並行実装にはなっていない
ことを確認済み（唯一の例外が上記finding 8）。

### 再レビュー結果（2026-07-10、コミット範囲 `66eb9fb..HEAD` — 上記1-17番修正コミットの検証）

上記の修正コミット（deed6d4, 5c4fa53, ac6b256, 197f466, a364c07, aee08ba, 114f1ae, db37289）を
1件ずつ検証。実際に`cargo test`/シェルスクリプトを実行して再現・反証した。8/8 finder + 6/6
verifier完了、**7件がCONFIRMED（PLAUSIBLE/REFUTEDなし）**。大半（12/17項目）は正しく修正された
ことを確認したが、4件は「修正が不完全」または「修正自体が新たな退行を生んだ」ことが判明した:

- finding 1（specguard polarityスワップ・バイパス未クローズ）→ **問題1**
- finding 2（specguard atomic-write修正が新規退行、上書き失敗の無音化）→ **問題1b**
- finding 3（stuckguard near-repeatエスカレーションのwindow境界回帰）→ **問題1c**
- finding 4（blastguard item D、実際には未修正）→ **問題3**
- finding 5-7（tdd/propguard/stuckguardの軽微な重複・デッドコード）→ **既知の軽微な未着手課題**

### 再レビュー最優先3件の検証テスト（2026-07-10、コミット `38f613c`）

上記findingのうち安全性に関わる3件（finding 1 の1(a)/1(b)バリアント、finding 2、finding 3）に
ついて、実際にバグを再現する回帰テストを追加・commit済み。いずれも`#[ignore = "known bug: ...
参照"]`付き（CIの通常`cargo test`はスキップ、`-- --ignored`で明示実行）。`cargo test --
--ignored <test名>`で実行すると**現時点で実際にFAILする**ことを確認済み（つまりバグの実在を
機械的に証明している）。将来の修正時は、修正後にこれらのテストが緑になったことを確認したうえで
`#[ignore]`属性を外し、恒久的な回帰テストへ昇格させること（各テスト名・受け入れ基準は問題1/
1b/1cの該当箇所を参照）。

finding 4（blastguard item D）はblastguard単体のユニットテストでは再現できず condukt側の統合
テストが必要なため当時は見送り（問題3のステップ1で対応）。finding 5-7は挙動バグではなく重複/
デッドコードの指摘のためテストでの「検証」に馴染まない（コードレビューで直接確認済み）。

副作用として実施した規約対応: テスト追加はcrateソースへの変更にあたるため、CLAUDE.mdの
バージョンlockstepルールに従い`specguard`(0.2.18→0.2.19)と`stuckguard`(0.1.9→0.1.10)を
3ファイル同時にmicro bumpし、`check-plugin-versions.py`/`check-version-bumped.py`の両ゲートが
exit 0であることを確認済み。`cargo fmt`/`cargo clippy --all-targets`も両crateでクリーン。
