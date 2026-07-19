//! AI 作業タイムライン。
//!
//! Claude Code hooks（`gt hook` → OSC 7717 event）とファイル監視から届いた
//! 変更を時系列で記録する。AI の思考は追わず、観測できた事実（どのファイルが
//! いつ・どのツールで変わったか）だけを持つ。表示専用で、LLM 呼び出しはしない。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::watcher::{merge_kind, ChangeKind};

/// 保持する最大件数。古いものから捨てる。
const MAX_ENTRIES: usize = 200;

/// 同じファイルのイベントをひとつにまとめる時間窓。
/// AI がファイルを書くと hooks のイベントとファイル監視のイベントが
/// ほぼ同時に届くため、この窓内の同一パスは1行に併合する。
const MERGE_WINDOW: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct TimelineEntry {
    pub kind: ChangeKind,
    pub path: PathBuf,
    /// hooks 由来なら Some(ツール名)。ファイル監視由来なら None。
    pub tool: Option<String>,
    pub at: Instant,
    /// 要確認ラベル（依存・設定・削除など）。None なら通常の変更。
    pub risk: Option<&'static str>,
}

#[derive(Default)]
pub struct Timeline {
    entries: Vec<TimelineEntry>,
    /// 直近の SessionStart（hooks）の時刻。表示側はこれより新しいエントリと
    /// 古いエントリの境目に区切り線を出す（「今のセッションで何が変わったか」）。
    session_start: Option<Instant>,
}

impl Timeline {
    pub fn push(&mut self, kind: ChangeKind, path: PathBuf, tool: Option<String>) {
        self.push_at(kind, path, tool, Instant::now());
    }

    fn push_at(&mut self, kind: ChangeKind, path: PathBuf, tool: Option<String>, now: Instant) {
        // 直近の同一パスは1行に併合（hooks とファイル監視の二重計上を防ぐ）。
        // 探すのは新しい側の数件だけで足りる（同時に届くのが前提のため）。
        for entry in self.entries.iter_mut().take(8) {
            if entry.path == path && now.duration_since(entry.at) < MERGE_WINDOW {
                entry.kind = merge_kind(Some(entry.kind), kind);
                if tool.is_some() {
                    entry.tool = tool;
                }
                entry.at = now;
                entry.risk = risk_label(&entry.path, entry.kind);
                return;
            }
        }

        let risk = risk_label(&path, kind);
        self.entries.insert(
            0,
            TimelineEntry {
                kind,
                path,
                tool,
                at: now,
                risk,
            },
        );
        self.entries.truncate(MAX_ENTRIES);
    }

    pub fn entries(&self) -> &[TimelineEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// SessionStart hook を受けたときに呼ぶ。以後の描画で区切り線の基準になる。
    pub fn mark_session_start(&mut self) {
        self.session_start = Some(Instant::now());
    }

    pub fn session_start(&self) -> Option<Instant> {
        self.session_start
    }

    /// 要確認ラベルつきの件数。
    pub fn risk_count(&self) -> usize {
        self.entries.iter().filter(|e| e.risk.is_some()).count()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.session_start = None;
    }
}

/// この変更は人間が目を通すべきか。ルールベース（LLM 不使用）。
/// 優先度順：削除 > 秘密 > 依存 > CI > 設定。
pub fn risk_label(path: &Path, kind: ChangeKind) -> Option<&'static str> {
    if kind == ChangeKind::Deleted {
        return Some("削除");
    }

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();

    // .env / .env.local 等。鍵や接続情報が入りがちで、AI に触られたら必ず見る。
    if lower == ".env" || lower.starts_with(".env.") {
        return Some("秘密");
    }

    // 依存関係（マニフェスト・ロックファイル）。挙動とライセンスが変わる。
    const DEPENDENCY_FILES: &[&str] = &[
        "cargo.toml",
        "cargo.lock",
        "package.json",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "requirements.txt",
        "pyproject.toml",
        "poetry.lock",
        "uv.lock",
        "go.mod",
        "go.sum",
        "gemfile",
        "gemfile.lock",
        "composer.json",
        "composer.lock",
    ];
    if DEPENDENCY_FILES.contains(&lower.as_str()) {
        return Some("依存");
    }

    // CI・自動化。壊れると気づきにくく、権限も持ちがち。
    let path_str = path.to_string_lossy().replace('\\', "/");
    if path_str.contains(".github/workflows/")
        || lower == ".gitlab-ci.yml"
        || lower == "dockerfile"
        || lower.starts_with("docker-compose")
    {
        return Some("CI");
    }

    // 設定ファイル。拡張子と名前ぐせで拾う（データ用 JSON まで拾わないよう
    // json は settings/config を名前に含むものだけ）。
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let config_ext = matches!(ext, "toml" | "ini" | "conf" | "cfg" | "yaml" | "yml");
    let config_name = lower.contains("config") || lower.contains("settings");
    if config_ext || (ext == "json" && config_name) {
        return Some("設定");
    }

    None
}

/// 経過時間の短い表示。タイムゾーン不要の相対表記にする
/// （chrono 依存を持ち込まない。作業ログは「どれぐらい前か」が分かれば足りる）。
pub fn format_age(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 10 {
        "いま".to_owned()
    } else if secs < 60 {
        format!("{secs}秒")
    } else if secs < 60 * 60 {
        format!("{}分", secs / 60)
    } else if secs < 60 * 60 * 24 {
        format!("{}時間", secs / 3600)
    } else {
        format!("{}日", secs / (3600 * 24))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_keeps_newest_first_and_caps_length() {
        let mut timeline = Timeline::default();
        let t0 = Instant::now();
        for i in 0..(MAX_ENTRIES + 10) {
            // 各エントリを別パスにして併合させない。
            timeline.push_at(
                ChangeKind::Modified,
                PathBuf::from(format!("file{i}.rs")),
                None,
                t0 + Duration::from_secs(i as u64 * 10),
            );
        }
        assert_eq!(timeline.len(), MAX_ENTRIES);
        assert_eq!(
            timeline.entries()[0].path,
            PathBuf::from(format!("file{}.rs", MAX_ENTRIES + 9))
        );
    }

    #[test]
    fn merges_same_path_within_window() {
        let mut timeline = Timeline::default();
        let t0 = Instant::now();
        // 監視イベント（tool なし）→ 直後に hooks イベント（tool あり）。
        timeline.push_at(ChangeKind::New, PathBuf::from("a.rs"), None, t0);
        timeline.push_at(
            ChangeKind::Modified,
            PathBuf::from("a.rs"),
            Some("Edit".to_owned()),
            t0 + Duration::from_secs(1),
        );
        assert_eq!(timeline.len(), 1);
        let entry = &timeline.entries()[0];
        // New→Modified の併合は New のまま（merge_kind の規則）。
        assert_eq!(entry.kind, ChangeKind::New);
        assert_eq!(entry.tool.as_deref(), Some("Edit"));
    }

    #[test]
    fn mark_session_start_records_a_time_and_clear_resets_it() {
        let mut timeline = Timeline::default();
        assert_eq!(timeline.session_start(), None);
        timeline.mark_session_start();
        assert!(timeline.session_start().is_some());
        timeline.clear();
        assert_eq!(timeline.session_start(), None);
    }

    #[test]
    fn does_not_merge_outside_window() {
        let mut timeline = Timeline::default();
        let t0 = Instant::now();
        timeline.push_at(ChangeKind::Modified, PathBuf::from("a.rs"), None, t0);
        timeline.push_at(
            ChangeKind::Modified,
            PathBuf::from("a.rs"),
            None,
            t0 + Duration::from_secs(10),
        );
        assert_eq!(timeline.len(), 2);
    }

    #[test]
    fn risk_labels_by_rule() {
        use ChangeKind::*;
        // 削除は何であれ要確認。
        assert_eq!(risk_label(Path::new("src/lib.rs"), Deleted), Some("削除"));
        // 秘密・依存・CI・設定。
        assert_eq!(risk_label(Path::new(".env"), Modified), Some("秘密"));
        assert_eq!(risk_label(Path::new(".env.local"), Modified), Some("秘密"));
        assert_eq!(risk_label(Path::new("Cargo.toml"), Modified), Some("依存"));
        assert_eq!(
            risk_label(Path::new("web/package-lock.json"), Modified),
            Some("依存")
        );
        assert_eq!(
            risk_label(Path::new(".github/workflows/ci.yml"), New),
            Some("CI")
        );
        assert_eq!(risk_label(Path::new("Dockerfile"), Modified), Some("CI"));
        assert_eq!(risk_label(Path::new("config.toml"), Modified), Some("設定"));
        assert_eq!(
            risk_label(Path::new(".claude/settings.json"), Modified),
            Some("設定")
        );
        // 通常のコード・データは要確認にしない。
        assert_eq!(risk_label(Path::new("src/main.rs"), Modified), None);
        assert_eq!(risk_label(Path::new("data/events.json"), Modified), None);
        assert_eq!(risk_label(Path::new("README.md"), New), None);
    }

    #[test]
    fn risk_count_counts_flagged_entries() {
        let mut timeline = Timeline::default();
        let t0 = Instant::now();
        timeline.push_at(ChangeKind::Modified, PathBuf::from("src/main.rs"), None, t0);
        timeline.push_at(ChangeKind::Modified, PathBuf::from("Cargo.toml"), None, t0);
        timeline.push_at(ChangeKind::Deleted, PathBuf::from("old.rs"), None, t0);
        assert_eq!(timeline.risk_count(), 2);
    }

    #[test]
    fn format_age_buckets() {
        assert_eq!(format_age(Duration::from_secs(3)), "いま");
        assert_eq!(format_age(Duration::from_secs(42)), "42秒");
        assert_eq!(format_age(Duration::from_secs(5 * 60 + 30)), "5分");
        assert_eq!(format_age(Duration::from_secs(3 * 3600)), "3時間");
        assert_eq!(format_age(Duration::from_secs(2 * 24 * 3600)), "2日");
    }
}
