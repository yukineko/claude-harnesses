use crate::hypothesis::Hypothesis;
use std::path::{Path, PathBuf};

/// charter.md を探す。
///
/// `repo_root` を起点に上方向に最大5階層まで `.compass/charter.md` を探す。
/// 見つかった最初のパスを返す。見つからなければ `None`。
fn find_charter(repo_root: &Path) -> Option<PathBuf> {
    let mut dir: Option<&Path> = Some(repo_root);
    for _ in 0..5 {
        let d = dir?;
        let candidate = d.join(".compass").join("charter.md");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// charter.md の内容から (north_star, dod_lines) を抽出する。
///
/// - `## north_star` 見出しの直後の段落テキストを north_star とする
/// - `## definition_of_done` 見出しの直後の箇条書き行を dod_lines とする
fn parse_charter(content: &str) -> (String, Vec<String>) {
    #[derive(PartialEq)]
    enum Section {
        None,
        NorthStar,
        Dod,
    }

    let mut section = Section::None;
    let mut north_star = String::new();
    let mut dod_lines: Vec<String> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "## north_star" {
            section = Section::NorthStar;
            continue;
        }
        if trimmed == "## definition_of_done" {
            section = Section::Dod;
            continue;
        }
        // 別の ## heading に入ったらセクション終了
        if trimmed.starts_with("## ") {
            section = Section::None;
            continue;
        }

        match section {
            Section::NorthStar => {
                if !trimmed.is_empty() && north_star.is_empty() {
                    // 最初の非空行を north_star として採用
                    north_star = trimmed.to_string();
                }
            }
            Section::Dod => {
                if let Some(stripped) = trimmed.strip_prefix("- ") {
                    dod_lines.push(stripped.trim().to_string());
                }
            }
            Section::None => {}
        }
    }

    (north_star, dod_lines)
}

/// 各仮説の `linked_goal` が compass charter の north_star / definition_of_done と
/// 整合しているかを判定し、整合しない (unlinked) 仮説の id リストを返す。
///
/// - `linked_goal` が `None` → unlinked
/// - `linked_goal` が `Some(text)` → charter の north_star または dod_lines のいずれかに
///   `text` が部分一致 (`contains`) すれば linked、しなければ unlinked
/// - charter が見つからない → 全仮説を unlinked 扱い (エラーにしない)
pub fn check_goal_link(hypotheses: &[Hypothesis], repo_root: &Path) -> Vec<String> {
    // charter を読む。読めなければ全仮説を unlinked とする
    let charter_text = find_charter(repo_root).and_then(|p| std::fs::read_to_string(p).ok());

    let (north_star, dod_lines) = match charter_text {
        Some(ref content) => parse_charter(content),
        None => return hypotheses.iter().map(|h| h.id.clone()).collect(),
    };

    let mut unlinked = Vec::new();

    for h in hypotheses {
        let linked = match &h.linked_goal {
            None => false,
            Some(goal) => {
                north_star.contains(goal.as_str())
                    || dod_lines.iter().any(|line| line.contains(goal.as_str()))
            }
        };
        if !linked {
            unlinked.push(h.id.clone());
        }
    }

    unlinked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hypothesis::{Hypothesis, Status};
    use std::fs;
    use tempfile::TempDir;

    fn make_hypothesis(id: &str, linked_goal: Option<&str>) -> Hypothesis {
        Hypothesis {
            id: id.to_string(),
            text: format!("hypothesis {id}"),
            status: Status::Open,
            evidence: vec![],
            linked_goal: linked_goal.map(|s| s.to_string()),
            condukt_run: None,
            success_criterion: None,
            kill_criterion: None,
            assumptions: vec![],
            confidence: crate::hypothesis::default_confidence(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            shipped_at: None,
        }
    }

    fn write_charter(dir: &Path, content: &str) {
        let compass_dir = dir.join(".compass");
        fs::create_dir_all(&compass_dir).unwrap();
        fs::write(compass_dir.join("charter.md"), content).unwrap();
    }

    const SAMPLE_CHARTER: &str = r#"## north_star
Claude Code 用 developer productivity プラグイン集。condukt・compass・session-insights・specguard 等の連携により、AI-assisted 開発の品質・可観測性・安全性・自律性を継続的に向上させる。

## definition_of_done
- cargo test --workspace が全件 pass する
- 全プラグインが plugin リストに表示され、plugin install コマンドから導入可能
- session-insights が SessionEnd フックでスケルトンノートを自動生成し、record スキルで散文を記入するフローが完成する
"#;

    // -------------------------------------------------------------------------
    // parse_charter のテスト
    // -------------------------------------------------------------------------

    #[test]
    fn goal_link_parse_charter_north_star() {
        let (north_star, _) = parse_charter(SAMPLE_CHARTER);
        assert!(
            north_star.contains("developer productivity"),
            "north_star should contain 'developer productivity', got: {north_star}"
        );
    }

    #[test]
    fn goal_link_parse_charter_dod_lines() {
        let (_, dod) = parse_charter(SAMPLE_CHARTER);
        assert_eq!(dod.len(), 3, "should have 3 DoD lines");
        assert!(dod[0].contains("cargo test"));
        assert!(dod[1].contains("plugin リスト"));
        assert!(dod[2].contains("session-insights"));
    }

    // -------------------------------------------------------------------------
    // check_goal_link のテスト
    // -------------------------------------------------------------------------

    #[test]
    fn goal_link_linked_north_star() {
        let tmp = TempDir::new().unwrap();
        write_charter(tmp.path(), SAMPLE_CHARTER);

        let hypotheses = vec![make_hypothesis("h1", Some("developer productivity"))];
        let unlinked = check_goal_link(&hypotheses, tmp.path());
        assert!(
            unlinked.is_empty(),
            "h1 should be linked via north_star match"
        );
    }

    #[test]
    fn goal_link_linked_dod() {
        let tmp = TempDir::new().unwrap();
        write_charter(tmp.path(), SAMPLE_CHARTER);

        let hypotheses = vec![make_hypothesis("h2", Some("cargo test"))];
        let unlinked = check_goal_link(&hypotheses, tmp.path());
        assert!(unlinked.is_empty(), "h2 should be linked via DoD match");
    }

    #[test]
    fn goal_link_unlinked_no_match() {
        let tmp = TempDir::new().unwrap();
        write_charter(tmp.path(), SAMPLE_CHARTER);

        let hypotheses = vec![make_hypothesis("h3", Some("totally unrelated goal"))];
        let unlinked = check_goal_link(&hypotheses, tmp.path());
        assert_eq!(unlinked, vec!["h3"], "h3 should be unlinked");
    }

    #[test]
    fn goal_link_unlinked_none_goal() {
        let tmp = TempDir::new().unwrap();
        write_charter(tmp.path(), SAMPLE_CHARTER);

        let hypotheses = vec![make_hypothesis("h4", None)];
        let unlinked = check_goal_link(&hypotheses, tmp.path());
        assert_eq!(
            unlinked,
            vec!["h4"],
            "h4 (no linked_goal) should be unlinked"
        );
    }

    #[test]
    fn goal_link_charter_absent_all_unlinked() {
        let tmp = TempDir::new().unwrap();
        // .compass/charter.md を作成しない

        let hypotheses = vec![
            make_hypothesis("h5", Some("developer productivity")),
            make_hypothesis("h6", None),
        ];
        let unlinked = check_goal_link(&hypotheses, tmp.path());
        assert_eq!(
            unlinked.len(),
            2,
            "all hypotheses should be unlinked when charter is absent"
        );
        assert!(unlinked.contains(&"h5".to_string()));
        assert!(unlinked.contains(&"h6".to_string()));
    }

    #[test]
    fn goal_link_mixed_linked_and_unlinked() {
        let tmp = TempDir::new().unwrap();
        write_charter(tmp.path(), SAMPLE_CHARTER);

        let hypotheses = vec![
            make_hypothesis("linked1", Some("condukt")), // north_star に含まれる
            make_hypothesis("linked2", Some("plugin リスト")), // dod に含まれる
            make_hypothesis("unlinked1", Some("no match")), // 不一致
            make_hypothesis("unlinked2", None),          // None
        ];
        let unlinked = check_goal_link(&hypotheses, tmp.path());
        assert!(
            !unlinked.contains(&"linked1".to_string()),
            "linked1 should be linked"
        );
        assert!(
            !unlinked.contains(&"linked2".to_string()),
            "linked2 should be linked"
        );
        assert!(
            unlinked.contains(&"unlinked1".to_string()),
            "unlinked1 should be unlinked"
        );
        assert!(
            unlinked.contains(&"unlinked2".to_string()),
            "unlinked2 should be unlinked"
        );
    }
}
