---
name: webqa-gen
description: Playwright e2e test 生成スキル。仕様文または URL から、ブラウザ探索 → シナリオ列挙 → test-first な spec 生成 → RED 観測 → 採用 まで一貫して回す。F→P オラクル（tdd red→green）で検証可能なテスト生成を保証する。
argument-hint: '<仕様文 or URL> [--task "<id>"]'
---

`/webqa-gen <仕様>` で、**仕様文 or URL → ブラウザ探索 → シナリオ生成 → spec 作成 → RED 観測 → 採用** を1サイクル回す。生成したテストは、実装前の状態で RED（失敗）を観測してから採用する。これにより「推測で locator を書いていないか」「ゆるすぎるマッチャーになっていないか」が検証可能になる。

**役割分担**: ブラウザ探索（Playwright MCP/claude-in-chrome）と spec 生成は、**機能実装とは別の subagent が行う**。実装者が生成したテストを先に見ることはない（CLAUDE.md §2(a)）。

## 前提確認

Playwright が環境に入っているか確認: `npx playwright --version`。無ければ `npm install -D @playwright/test` を案内する。
タスク ID を1つ決める(例: `form-submission`)。以降 `tdd red/green --task <id>` に使う。

## 不変条件(外さない)

1. **推測は禁止**—— ブラウザが手元に無い場合（Playwright MCP・claude-in-chrome の両方が無い場合）、locator を推測で書かず、人間に Ask する。
2. **意味的 locator 優先** — `getByRole` / `getByLabel` / `getByText` など semantic locator を第一選択肢に。CSS selector・XPath・nth-child は最終手段。accessibility の観点からも堅牢性が高い。
3. **waitForTimeout 禁止、auto-waiting 推奨** — `waitForTimeout(5000)` のような固定待機は使わない。Playwright の auto-wait（click, fill 前の自動待機）と `expect(...).toBeVisible()` の assertion-based waiting を使う。
4. **RED を必ず観測** — 実装前の状態（未実装 or 旧版 rev）で `tdd red --task <id> --cmd "npx playwright test <spec>"` を実行し、テストが失敗することを確認。RED 証跡がない GREEN は採用しない。
5. **弱体化を検査** — コミット前に spec を stage して `webqa weaken-check`（既定で `git diff --cached` を検査。差分ファイルなら `--diff-file <path>`）で、test.skip/.only・expect 削除・waitForTimeout 追加・matcher 緩和をスキャンし、検出されたら修正。
6. **データ・認証・本番操作を避ける** — テストデータはハードコード禁止、環境変数から。認証情報も同様。本番 URL には破壊的操作（DELETE/削除）をしない。staging/dev URL を優先。

## 手順

### Phase 1 — 仕様の確認・ブラウザ探索
仕様文 or URL が与えられたら、**実装前の現在の状態**（或いは未実装の機能）をブラウザで探索する。Playwright MCP を使い、以下を確認:
- ページ構造・input/button の role・label・accessible name
- どの locator strategy が使えるか（role がない場合は text fallback）
- ネットワークリクエスト・待機ポイント（非同期の完了）

Playwright MCP が無い場合は claude-in-chrome を使う。どちらも無い場合は Ask する。

### Phase 2 — シナリオ列挙
探索結果から、テストすべき **happy path と主要な error case** を列挙:
- ユーザーが実行する一連の操作（fill → click → wait）
- 期待される UI 遷移・データ表示
- 境界条件（空入力・最大長など）
- エラーハンドリング（失敗メッセージ表示）

### Phase 3 — spec 生成
列挙したシナリオを Playwright spec (`*.spec.ts`) に落とす。ルール:
- `test()` で各シナリオを分ける。明記的なテスト名。
- `await page.goto(testUrl)` で基点設定。`testUrl` は環境変数から取得。
- locator は semantic から: `page.getByRole('button', { name: /Save/ })` など。
- assertions は期待状態を明確に: `expect(dialog).toBeVisible()` など。
- `toBeVisible()` / `toHaveValue()` など state assertion を使う。`waitForSelector()` は不要（auto-wait 機構に任せる）。

### Phase 4 — RED を観測
```
cd <repo> && tdd red --task <id> --cmd "npx playwright test <spec>"
```
**実装前の状態**でこのコマンドを実行。テストが意図通り失敗することを確認。失敗出力を見て、
- locator が正しく要素を見つけているか
- 期待値のミスマッチが出ているか（わざと失敗させられているか）

を確認する。RED 証跡が書かれたら次へ。

### Phase 5 — 実装と RED→GREEN
実装者が機能を実装。その後、テスト実行者が:
```
tdd green --task <id> --cmd "npx playwright test <spec>"
```
テストが GREEN になることを確認。RED→GREEN の証跡が揃ったら、tdd の F→P オラクルで本物の Fail→Pass 遷移かを確かめる:
```
tdd oracle --task <id>   # exit 0 かつ "transition":"fail_to_pass" のときだけ採用
```

### Phase 6 — 弱体化チェック・採用
コミットの直前に必ず:
```
git add <spec> && webqa weaken-check   # exit 0=弱体化なし / 1=所見あり / 2=判定不能（block 扱い）
```
test.skip / .only / waitForTimeout / matcher 緩和を検出したら修正。その後、Green 状態を final verify してから採用。

## 失敗モード
- **Playwright MCP・claude-in-chrome 両方不在** → Ask して人間に任せる。推測で locator を書かない。
- **RED が通らない（最初からテストが pass）** — 未実装の機能か旧版の状態で実行しているか確認。実装が既に完了していないか見直す。
- **weaken-check で RED** — waitForTimeout や matcher 緩和が見つかった。修正して再チェック。
- **Stop hook でブロック** — webqa gate が関連 spec を実行して判定。spec の integrity を確認（弱体化がないか、RED→GREEN 証跡があるか）。
