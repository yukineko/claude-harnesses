# FIX: GATE_CRATES の定義が参照箇所ごとにズレている（`overwatch` の扱い不一致）

**状態**: 修正済み（§4.1/4.2 の2箇所 + CLAUDE.md 記述 + scripts/tests/canary-gate-crates.sh case4 差し替えをコミット。§5 の sync-checker 新設は別タスクとして見送り、backlog に積んだ）。発見経緯: `/continuous-audit` の監査対象選定を確認する過程で、実際に4箇所のソースを突き合わせて判明。

## 1. 問題

「GATE_CRATES（fleet の防御ゲート本体）」を指す箇所が harness 内に複数あるが、**`overwatch` を含めるかどうかが揃っていない**。

| 参照元 | 実際の値 | クレート数 | `overwatch` 込み？ |
|---|---|---|---|
| `scripts/rollout-plugins.sh`（正典） | `GATE_CRATES="blastguard propguard specguard stuckguard mutategate overwatch"` | 6 | ✅ |
| `scripts/continuous-audit.sh` | `DEFAULT_TARGETS="blastguard,propguard,specguard,stuckguard,mutategate,overwatch"` | 6 | ✅ |
| `.githooks/pre-push` | `GATE_PATTERN='^crates/(blastguard\|propguard\|specguard\|stuckguard\|mutategate)/'` | 5 | ❌ |
| `crates/overwatch/skills/continuous-audit/SKILL.md`「対象 crate (既定)」 | 「blastguard,propguard,specguard,stuckguard,mutategate」 | 5 | ❌ |

根拠（file:line）:
- [scripts/rollout-plugins.sh:111](../scripts/rollout-plugins.sh#L111)
- [scripts/continuous-audit.sh:72](../scripts/continuous-audit.sh#L72)
- [.githooks/pre-push:26](../.githooks/pre-push#L26)
- `crates/overwatch/skills/continuous-audit/SKILL.md`「## 対象 crate (既定)」節

**正典**（`rollout-plugins.sh`）と一致しているのは `continuous-audit.sh` だけ。`.githooks/pre-push` と SKILL.md の説明文はどちらも `overwatch` を見落としている。

## 2. 実際に起きる不整合（具体例）

`crates/overwatch/` **だけ**を変更して push した場合:

1. `rollout-plugins.sh` の視点では `overwatch` は GATE_CRATES の一員 → 変更には canary 経由の慎重なロールアウトが求められる重要クレート。
2. しかし `.githooks/pre-push` の `GATE_PATTERN` に `overwatch` が含まれていないため、**「continuous-audit を検討して」という advisory が一切出ない**。
3. `/continuous-audit` を人間が手で（`--target` 省略で）呼べば、SKILL.md の説明文（5クレート）を読んだ人間/LLMは overwatch を対象に含めない可能性があるが、実際にバックエンドで叩かれる `continuous-audit.sh` の既定は6クレートなので、**skill の説明文だけを信じた運用と、スクリプトの実際の挙動がズレる**。

つまり「overwatch 自身への変更が一番敵対的レビューを必要とする」（continuous-audit.sh 自身の実行主体であり、canary health-gate もこれに依存している）にもかかわらず、**push 時の気づきの経路（pre-push advisory）と、skill のドキュメント上の既定が、両方ともそれを見落とすように書かれている**。

## 3. 推定される根本原因

`rollout-plugins.sh` の `GATE_CRATES` に `overwatch` が**後から追加された**（`continuous-audit.sh` 側のコメントに「the canary health-gate depends on it too, so it gets the same audit coverage as the crates it protects」という追記の跡がある）際に、`continuous-audit.sh` の `DEFAULT_TARGETS` は追随して更新されたが、`.githooks/pre-push`（先に書かれた `GATE_PATTERN`）と SKILL.md の説明文は**追随更新されなかった**。3箇所以上に同じ定義がハードコードされている構造そのものが、今回のようなドリフトを生みやすい。

## 4. 修正方針

`.githooks/pre-push` と SKILL.md の該当箇所を、正典（`rollout-plugins.sh` の `GATE_CRATES`）に合わせて **`overwatch` を含む6クレートに揃える**。

### 4.1 `.githooks/pre-push`

```diff
-GATE_PATTERN='^crates/(blastguard|propguard|specguard|stuckguard|mutategate)/'
+GATE_PATTERN='^crates/(blastguard|propguard|specguard|stuckguard|mutategate|overwatch)/'
```

advisory メッセージ本文中の crate 列挙（コメント含む）も同様に `overwatch` を追記する。

### 4.2 `crates/overwatch/skills/continuous-audit/SKILL.md`

「## 対象 crate (既定)」節の説明文を実際の `continuous-audit.sh` の既定と一致させる:

```diff
-既定の target は fleet の GATE crates: `blastguard,propguard,specguard,stuckguard,mutategate`
-(`scripts/rollout-plugins.sh` の GATE_CRATES と同期)。
+既定の target は fleet の GATE crates: `blastguard,propguard,specguard,stuckguard,mutategate,overwatch`
+(`scripts/rollout-plugins.sh` の GATE_CRATES と同期。overwatch はこのループ自身が依存する
+バイナリであり、canary health-gate もこれに依存するため、保護対象のクレートと同じ監査対象に含まれる)。
```

## 5. 再発防止 — 定義の重複自体を機械チェックで縛る

3箇所以上に同じクレート集合をハードコードしている限り、今後も同種のドリフトが起きうる。このリポジトリには `check-plugin-versions.py`/`check-version-bumped.py` のような「複数箇所の一致を機械的に検証するゲート」を追加する文化がすでにあるので、同じパターンを適用する。

**提案**: `scripts/check-gate-crates-sync.py` を新設し、次を検証する:
- `scripts/rollout-plugins.sh` の `GATE_CRATES=` 行から正典セットを抽出。
- `scripts/continuous-audit.sh` の `DEFAULT_TARGETS=` 行を抽出し、正典と集合として一致するか確認。
- `.githooks/pre-push` の `GATE_PATTERN=` 行の正規表現から crate 名を抽出し、正典と一致するか確認。
- `crates/overwatch/skills/continuous-audit/SKILL.md` の該当節からクレート列挙を抽出し、正典と一致するか確認。
- 不一致があれば非0終了・該当ファイルと差分を表示（`check-plugin-versions.py` と同じ出力作法）。

CI もしくは `.githooks/pre-commit`（advisory 層）に配線するかは、既存の injectguard/bin-reproducibility ゲートと同様の判断基準（非バイパス本ゲートは CI 側）に従う。

## 6. 受け入れ基準

- [x] `.githooks/pre-push` の `GATE_PATTERN` に `overwatch` が含まれる。
- [x] `.githooks/pre-push` のコメント中のクレート列挙も更新されている。
- [x] SKILL.md「対象 crate (既定)」節が6クレート（`overwatch` 込み）を正しく説明している。
- [ ] `scripts/check-gate-crates-sync.py`（新設）が4箇所すべてを比較し、現状（修正後）で green を返す。
      → **見送り（別タスク）**: 今回は実際の drift（4箇所の不一致）の是正のみを右サイズの一手として実施。
      再発防止の機械チェッカー新設は backlog に別項目として積んだ。
- [ ] 上記チェッカーに、いずれか1箇所だけ crate を追加/削除したケースのユニットテスト（drift を検知して非0終了することを確認）がある。
      → 同上、チェッカー本体と合わせて見送り。
- [x] 変更した plugin（該当すれば）の version が3ファイル lockstep で上がっている（`.githooks/pre-push` 自体は plugin 資産ではないため対象外、SKILL.md は `overwatch` plugin 内なので該当）。overwatch 0.1.32→0.1.33。

追加で当初のスコープに無かったが同時に是正した箇所:
- [x] `CLAUDE.md`（36, 62行目）の GATE_CRATES 記述に `overwatch` を追加。
- [x] `scripts/tests/canary-gate-crates.sh` の case4 が `overwatch`（現在は gate crate）を non-gate の例として誤用していたため、真の non-gate crate（`session-insights`）に差し替え、overwatch 自身が gate 扱いされることを確認する case5 を新設。

## 7. 非目標

- `overwatch` を GATE_CRATES から外す方向での統一（正典である `rollout-plugins.sh` 側の判断を変更すること）は今回の対象外。今回はあくまで**正典に他を合わせる**修正。
- 4箇所を1箇所の共有設定ファイル（例: `gate-crates.txt`）に統合するような大きな構造変更は、今回は提案しない（§5 のsyncチェッカーで当面は十分と判断。将来ドリフトが繰り返すようなら再検討）。
