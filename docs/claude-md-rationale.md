# CLAUDE.md の根拠・実測・経緯

`CLAUDE.md` は main session と全 subagent に毎回注入されるため、規範（ルール・手順・定義）だけを残し、
その根拠となる実測・経緯・事例・過去コードの引用をここへ移した（2026-11-01 のユーザー裁定:
「規範は変えずに根拠を移す」）。**規範の正典は `CLAUDE.md` であり、この文書は規範を追加も変更もしない。**
見出しの番号は `CLAUDE.md` の「最上位の方針」の節番号と一致させてある（`CLAUDE.md §3` の根拠は本書 §3）。

## 1. fail は罰ではない。検出であり、価値がある

### 「ターンを壊すな」の経緯

このリポジトリには「never break the turn ＝ ターンを壊すな」という**不変条件**があり、
判定を持つコードにも適用されていた。結果として Stop ゲートの panic barrier は panic を「許可」に
写しており、**panic やエラーを「許可」に写す実装がこの一文で正当化されていた** — 当時のコードは
`docs/article-llm-fail-open.md:48` に「swallow it and exit 0 (allow the stop; a hook must never break the user's turn)」として引用が残る。

### 移行済みの実体

**移行済みの実体**: 7 ゲート（donegate / reviewgate / propguard / tdd、および budgetguard / autoflow /
ctxrot の Stop hook 各 `main.rs`）が通る panic barrier は fail-closed へ移行した（前者4つは `d6db4670`、
後者3つ=budgetguard/autoflow/ctxrotは判定〈`{"decision":"block"}` を実際に返す〉を持ちながら
`harness_core::hook::run_hook`〈panic握り潰し→常時exit0〉経由のまま取り残されていたのを本節の是正として
`harness_core::gate::run::run_guarded` へ移行）。gate 本体が panic した＝判定不能は block に
解決する: `crates/harness-core/src/gate/run.rs:186`「**fail closed**: block the stop and surface the crash」。
同一ゲート・同一セッションの連続 2 回目の panic だけが bounded に allow へ落ちる。
その allow には `stop_hook_active` に加え、直前の stop もそのゲートの panic だったことを示す
per-gate・per-session marker が要り、別ゲートの block による re-entry 中の初回 panic や、
panic しなかった run を挟んだ後の panic は block する。
marker を読めない・書けない（またはセッション id が無い）ときの re-entry は `systemMessage` 付きの明示的な allow になり、
chain 最初の stop は常に block する。

## 2. 判断は予測にすぎない。fail はテストで決着させる

### 「何も検証していないテストは常に通る」の実例

実例・**修正済み**: `crates/condukt/src/verify.rs` の `checks_verdict` はかつて空スライスに
`true` を返し、`assert!(checks_verdict(&[]))` がそれを*正しいものとして固定*していた（空集合 fail-open が
仕様として書き下された形で、3. の「空集合を返さない」に真っ向から反していた）。commit `d30d9b00`
（2026-07-21 13:51、backlog `100af807`）で三値化し、今は空スライスに `ChecksVerdict::NoChecksDeclared`
を返す（`Passed` ではない）。

## 3. 判定不能（cannot determine）は必ず制限側に解決する

### なぜ「わからない」を「大丈夫」に写してはならないか

「わからない」は「大丈夫」ではない。この 2 つを同じ出力に写した時点で、そのゲートは
「検査した」と「検査できなかった」を下流から区別不能にしている。

### blastguard の三値設計（最もよく説明されている箇所）

この不変条件が最もよく説明されているのは `crates/blastguard/src/model.rs:5`（「Three answers, not two.」）で、
**二値型そのものが原因**だと名指ししている — 二値の `Allow`/`Deny` は、blastguard が解析できない構文を
すべて `Allow` に強制し、「これは理解できない」を「これは問題ない」として記録させていた。
`Decision::Ask` はその欠けた第三の答えで、`crates/blastguard/src/model.rs:9`（「it is NOT a verdict about the command, it is a refusal」）
のとおり**コマンドについての判定ではなく、判定を推測することの拒否**である。

### 共有三値型への収斂（測定値）

`grep -rln 'harness_core::verdict::' crates/*/src/`（harness-core 自身を除く）は21ファイルにヒットする
（測定日 2026-07-22、測定点 `6d4312c5`）— blastguard/budgetguard/condukt/ctxrot/donegate/gauge/
harness-status/propguard/reviewgate/session-insights 等、各 crate が個別再発明するのではなく
この共有型へ収斂している。

### 測定値ブロック（fail-open 修正の件数）

測定点の無い数字は、次の著者が転記した瞬間に腐る。

```
git log 94364b09 -i --grep='fail.open\|fail.closed' --oneline | wc -l
```
→ **60 件**（測定日 2026-07-21、測定点 `94364b09`）。うち **32 件が 2026-07-21 の 1 日**に
landed している（同じ集合を `--format='%ad' --date=short` で日別集計）。

同じ手順を `silently` で回すと 106 件になるが、これは無関係な commit と `feat` を含む
**広いパターン**であり、「このクラスの修正」の根拠には使えない。
広いパターンの値を狭い主張の根拠に転記すること — それがこの節が防ごうとしている記録の腐敗そのもの
（規範は `CLAUDE.md §3` の測定値ブロックに残る）。

この段落の前版は「45 件 / 126 件 / 14 件」と書いていたが、**どれも同じ方法で再現できなかった**
（実測 60 / 106 / 32）。

60 件のうち少なくとも 3 件（`3b1eb24` / `c066fc8` / `05df9b2`）は「安全な degrade だ」と
*明示的に正当化するコメント*を伴っていた（`231e20e` はコメント無しだったので「全件」ではない）。
**バグというより、判断が ok 側に倒れた結果**である。だからこの規範を明文で置く。

## 6. 実装者は「大丈夫だろう」に倒れる — それを**前提**に設計する

### 反証の立証責任（実測）

実測: 2026-07-21 の監査で `specguard/forge/gather.rs` の実在する fail-open が、検証者が消費経路を 1 本しか辿らなかった
ために誤って棄却された。件数閾値を満たす部分的な束が clean として下流へ渡る経路を見落としていた。

### 批判の非対称性（実測）

実測: 2026-07-21、敵対的検証者の 4 件の
`REFUTED` を無検証で採用して backlog を降ろしたが、うち 1 件は実在する欠陥だった。
残り 3 件は今も未検証のまま。

### Ask が安い理由

- **質問を避けて自分の判断で埋める行為が、まさに「そういうことにしておく」**であり、
  123 コミット分の fail-open を生んだ当のものである。
- Ask のコストは人間の数秒。黙って判断で埋めたコストは、**誰も気づかないまま残る fail-open**。
  非対称であり、ask 側が常に安い。

## 7. ワークフローの制御は自分で保有する — ゲートを外部サービスに預けない

### 外部に預けると何を握られるか（実測）

外部に預けると、**権限・可用性・可視性の 3 つを同時に相手に握られる**。これは推測ではなく実測である
（2026-07-21、fail-open ゲート 4 本を GitHub の required status check として登録した結果）:

- **権限**: 解除に必要な Administration:write が手持ちの fine-grained PAT に無く、
  `gh api -X DELETE …/rulesets/19438193` は `HTTP 403` で拒否された。**自分で外せない**。
- **可視性**: 正しい修正（PR #24）が止まっていることが**ローカルからは見えない**。担当 agent は
  「34→29 を達成した」と正しく報告でき、その報告は真だが、変更は merge 不能のままだった。
  **報告が真であることと、流れが進んでいることが分離した。**
- **可用性**: ゲートが自分の側の事情（CI の赤・スキャナのバグ）で止まっても、こちらには手がない。

### local 縮退の実例

旧 `.githooks/pre-commit` は python3 欠落・スキャナ欠落を exit 0 で通していた。
その正当化が CI 依存だった。

### 2026-10-29 のユーザー裁定（原文）

裁定原文:
「gh コマンド実行は基本的に禁止でなく、変更があるときには Ask にしてほしい」
「GHA でやれとか依頼がない限り勝手に実行や設定はしない、観測は無条件でできるようにする」。

### 経緯（なぜ以前は全面禁止だったか）

**経緯（なぜ以前は全面禁止だったか）**: 2026-07-24、mutation ワークフローが main で chronic red
だった際、ローカルの pre-push フックが通過した後も、修正が効いたかを確かめるために
`gh workflow run` を試み（403 で拒否）、push が誘発した実 run を `gh run watch` で監視し続け、
GitHub Actions の状態を「本当に終わったか」の追加ゲートとして扱っていた。これは advisory を
事実上の追加ゲートへ格上げした実例であり、上の「CI の green を完了判定にしない」はその是正として
残る。chronic-red-CI チェックと `PREPUSH_SKIP_CI_RED` は `.githooks/pre-push` から撤去済みで
今は存在せず、GHA の状態を読むゲートを再導入しない。

（`crates/stuckguard/src/protection.rs` の grounds が参照する「CLAUDE.md section 7 'Why' (2026-07-24)」はこの経緯を指す。）

## 8. 作業は worktree で行う — main の作業ツリーを共有編集しない

### なぜ分離するのは編集の場か

統合できないのは**同じ index / 作業ツリーを2つのセッションが共有した場合**だけで、
これは git が救えない唯一の競合である。

### 分離しないと何が起きるか（実測 2026-07-23、本 repo で観測）

- 2セッションが main の作業ツリーを共有し、`git status` に双方の未コミット変更が同時に現れた
  （一方は `crates/harness-core/src/boundary.rs`、他方は `crates/condukt/src/worktree.rs`）。
  **触っているファイルは非衝突なのに、index が共有されているため分離できない。**
- `donegate` の required check は workspace 全体（`cargo fmt --all` / `cargo clippy --workspace`）を見るため、
  **自分が触っていない crate の赤が自分の停止をブロック**した。`reviewgate` も他セッションの差分を
  自分の変更としてレビュー要求した。
- その結果、唯一の逃げ道が project root の共有 skip ファイルになる。これは**一度だけ消費される**ため
  他セッションの正当なゲートを素通りさせる — 5. が禁じる fail-open へ構造的に圧力がかかる。
- **worktree 分離はこの3つを同時に消す**（別チェックアウトなので相手のファイルがそもそも存在せず、
  ゲートは自分の変更だけを見る）。

### 撤去した carve-out

旧版にあった「単一セッションしか動いていないことが確かな場合の
軽微な編集だけは可」という carve-out は**撤去した** — その「確かな場合」は検証不能だからである
（実測 2026-07-26: `overwatch status` が no session を返す一方で `scripts/rebuild-plugins.sh` が連続する
2 回の git 呼び出しの間に modified→clean へ変化し、別セッションが main のツリーを触っている証拠になった。
「たぶん自分だけ」は判断＝予測であり 2./6. が禁じる自己許可そのもの）。
