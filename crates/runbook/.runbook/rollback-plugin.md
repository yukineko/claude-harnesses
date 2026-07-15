+++
description = "gate crate canary失敗時にAUTO-ROLLED-BACKを検知し、復旧を確認する手順"
aliases = ["rollback", "canary-rollback"]
+++

# rollback-plugin

## Overview
`scripts/rollout-plugins.sh --canary` で gate crate
（blastguard/propguard/specguard/stuckguard/mutategate/overwatch）をロールアウトした際、
ステージ間の health-gate（`overwatch canary-gate`）が raw-spike または systemic recurrence を
検知すると、そのステージは自動的に AUTO-ROLLED-BACK され、rollout は非0 (exit 4) で停止する。
このとき「検知 → 復元 → 確認」を手順化したもの。プロンプトで `!rollback-plugin`
（または `!rollback` / `!canary-rollback`）と書くとこの手順が注入される。

## Procedure
1. **検知**: `scripts/rollout-plugins.sh --canary` の出力を確認する。
   ```
   health-gate: ROLLBACK — raw-spike or systemic recurrence detected; rolling back stage <N> and halting
   canary: HALTED at stage <N> after auto-rollback.
   ```
   このメッセージと非0 exit code (4) が出ていれば AUTO-ROLLED-BACK が発生している。
   `rollback plan for stage <N>:` 以下に、その場で実行された復元内容（prior version へのポインタ）が
   表示される。

2. **復元済みであることの確認 (installed_plugins.json)**: rollout スクリプトは health-gate が
   ROLLBACK を返した時点で `registry_patch` により `installed_plugins.json` を**その場で**
   直前 (prior) の version dir へ repoint し直している（別途手動 restore は不要）。対象プラグインの
   エントリが元の version を指しているか確認する。
   ```sh
   python3 -c "
   import json
   reg = json.load(open('$HOME/.claude/plugins/installed_plugins.json'))
   print(json.dumps(reg['plugins'].get('<plugin-name>@yukineko', {}), indent=2))
   "
   ```
   `installed_plugins.json.bak-<epoch>`（rollout スクリプトが書き込み前に必ず取るバックアップ）が
   同ディレクトリに残っているので、万一 repoint 後の JSON が壊れている場合はそれと比較・復元する。
   ```sh
   ls -t $HOME/.claude/plugins/installed_plugins.json.bak-* | head -1
   ```

3. **cache 側の version dir が生きていることを確認する**: rollback は既存の version dir へ
   repoint するだけで削除は行わない（rollout スクリプトは「古い version dir を絶対に削除しない」）。
   repoint 先の dir が実在するか確認する。
   ```sh
   ls "$HOME/.claude/plugins/cache/yukineko/<plugin-name>/"
   ```

4. **overwatch 側でロールバック事象を確認する**: rollback 実行時に
   `overwatch record-rollback --plugin <name> --to-version <canary_ver> --stage <N> --reason raw`
   が記録されている（fail-soft: 記録失敗は rollout を止めない）。統合レビューサーフェスで確認する。
   ```sh
   overwatch review-queue                 # [rollback] タグの行を確認（人間可読・新しい順）
   overwatch review-queue --json | jq '[.[] | select(.kind == "rollback")]'
   ```

5. **健全性の再確認**: rollback 後、対象 gate crate が実際に prior version で動いているか
   （新しいバイナリの誤配布が残っていないか）を確認する。
   ```sh
   overwatch violations --systemic          # systemic recurrence が収まっているか
   python3 scripts/check-plugin-versions.py # 3ファイル lockstep が壊れていないか（rollback対象は
                                             # レジストリ側の version のみ変わるため、repo 側の
                                             # version は不変であることを確認）
   ```

6. **原因調査と再ロールアウト**: health-gate が rollback を advise した根本原因（raw-spike の
   内容、systemic recurrence のシグネチャ）を `overwatch violations` / `overwatch review-queue`
   で調査し、原因を修正したうえで改めて `--canary` 付きで再ロールアウトする。原因未解決のまま
   `--no-canary` で強行しない。

## Specifications
- 完了条件: 対象プラグインの `installed_plugins.json` エントリが rollback 前（prior）の
  version/path を指しており、`overwatch review-queue` に該当 `[rollback]` イベントが記録されていること。
- rollback は自動実行される（rollout スクリプトが stage 内で即座に repoint する）。人間が行うのは
  「検知の確認」「復元結果の確認」「原因調査」であり、手動での repoint 操作は通常不要。
- 復元は version dir の**削除を伴わない**（既存 dir への repoint のみ）。

## Forbidden Actions
- rollback が発生した原因を調査せずに `--no-canary` で再ロールアウトしない
- `installed_plugins.json` を手で直接編集して repoint しない（rollout スクリプトの
  `registry_patch`（atomic write + backup + 再パース検証）を経由しない手動編集は整合性を壊す恐れがある）
- cache の version dir を手動で削除しない（rollback 先が失われる）
