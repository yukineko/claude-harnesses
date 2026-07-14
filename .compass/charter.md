## north_star
specguardが2026-07-13の仕様監査(reports/spec-audit/2026-07-13.md)で検出したneeds_user=yesの仕様ドリフト2件をdoc修正で解消し、specguard ackでsentinelを解除する。

## definition_of_done
- crates/condukt/README.md のコマンド表に condukt adversarial の plan と vote サブコマンドの行を追加する
- crates/condukt/README.md の設定例に adversarial セクション(enabled, size, min_voters, block_ratio)を追加する
- crates/condukt/README.md の環境変数表に CONDUKT_ADVERSARIAL の行を追加する
- crates/condukt/skills/condukt/SKILL.md の Phase 7 reconcile 手順に exit 2 (duplicate_completion 検知時のescalate)分岐の説明を追記する
- docs/OVERVIEW.md の harness-core モジュール表に crates/harness-core/src/lessons.rs (text_similarity 関数)を追加する、または表が代表例のみである旨を明記する
- specguard ack が成功し 未処理ドリフトの通知が解消される(specguard pending が警告なしを返す)
- 修正はコミットのみ行い push や PR作成はしない

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(specguardのneeds_user=yesドリフト2件をdoc修正で解消しackでsentinel解除)から現状(condukt README.mdにadversarial plan/vote行・configセクション例・CONDUKT_ADVERSARIAL環境変数行のいずれも未記載、SKILL.md Phase7にreconcileのexit2 duplicate_completion分岐の説明なし、docs/OVERVIEW.mdのharness-coreモジュール表にlessons.rs未掲載、sentinelは.specguard-pendingとして残存)への最大差分: condukt README.mdへの3箇所追記(コマンド表・config例・env var表)、SKILL.md Phase7への1箇所追記、docs/OVERVIEW.mdモジュール表への1行追加、その後specguard ackでsentinel解除、が主スライス。

## next_action

## parked

