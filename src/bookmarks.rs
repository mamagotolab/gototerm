//! よく使うフォルダのブックマーク。ランチャーの `m` で付け外しし、`b` で一覧から飛ぶ。
//!
//! 「最近使った」(recent.rs) が自動で溜まる履歴なのに対し、こちらは本人が選んで残すもの。
//! なので保存先はキャッシュではなく設定側に置く（キャッシュを消しても消えないように）。

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Bookmarks {
    entries: Vec<PathBuf>,
    /// 保存先。None なら保存しない（テストが本物のブックマークを壊さないように）。
    path: Option<PathBuf>,
}

impl Bookmarks {
    pub fn load() -> Self {
        Self::load_from_path(&bookmarks_path())
    }

    /// ディスクを触らない空のブックマーク（テスト用）。
    #[cfg(test)]
    pub fn empty_for_test() -> Self {
        Self {
            entries: Vec::new(),
            path: None,
        }
    }

    fn load_from_path(path: &Path) -> Self {
        let entries = std::fs::read_to_string(path)
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            entries,
            path: Some(path.to_path_buf()),
        }
    }

    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.entries.iter().any(|entry| entry == path)
    }

    /// 付いていなければ付け、付いていれば外す。戻り値 true=付けた。
    pub fn toggle(&mut self, path: &Path) -> bool {
        let added = toggle_path(&mut self.entries, path);
        self.save();
        added
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut text = String::new();
        for entry in &self.entries {
            text.push_str(&entry.to_string_lossy());
            text.push('\n');
        }
        let _ = std::fs::write(path, text);
    }
}

fn toggle_path(entries: &mut Vec<PathBuf>, path: &Path) -> bool {
    if let Some(index) = entries.iter().position(|entry| entry == path) {
        entries.remove(index);
        return false;
    }
    // 新しいものを上に積む（よく使うものほど手前に来るように）。
    entries.insert(0, path.to_path_buf());
    true
}

fn bookmarks_path() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));

    base.unwrap_or_else(std::env::temp_dir)
        .join("gototerm")
        .join("bookmarks.txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_adds_then_removes() {
        let mut entries = Vec::new();
        let path = Path::new("/work/programs");

        assert!(toggle_path(&mut entries, path), "1回目は付ける");
        assert_eq!(entries, vec![PathBuf::from("/work/programs")]);

        assert!(!toggle_path(&mut entries, path), "2回目は外す");
        assert!(entries.is_empty());
    }

    #[test]
    fn newest_bookmark_comes_first() {
        let mut entries = Vec::new();

        toggle_path(&mut entries, Path::new("/a"));
        toggle_path(&mut entries, Path::new("/b"));

        assert_eq!(entries, vec![PathBuf::from("/b"), PathBuf::from("/a")]);
    }

    #[test]
    fn removing_one_keeps_the_others() {
        let mut entries = Vec::new();
        toggle_path(&mut entries, Path::new("/a"));
        toggle_path(&mut entries, Path::new("/b"));
        toggle_path(&mut entries, Path::new("/c"));

        toggle_path(&mut entries, Path::new("/b"));

        assert_eq!(entries, vec![PathBuf::from("/c"), PathBuf::from("/a")]);
    }

    /// テスト用のブックマークは保存先を持たない＝本物の bookmarks.txt を書き換えない。
    #[test]
    fn test_bookmarks_never_touch_disk() {
        let mut marks = Bookmarks::empty_for_test();

        marks.toggle(Path::new("/work/a"));

        assert!(marks.path.is_none());
        assert_eq!(marks.entries(), [PathBuf::from("/work/a")]);
    }

    #[test]
    fn saving_writes_only_to_its_own_path() {
        let path = std::env::temp_dir().join(format!(
            "gototerm-bookmarks-save-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut marks = Bookmarks::load_from_path(&path);

        marks.toggle(Path::new("/work/a"));

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "/work/a\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_returns_empty_when_file_is_missing() {
        let path = std::env::temp_dir().join(format!(
            "gototerm-missing-bookmarks-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        assert!(Bookmarks::load_from_path(&path).entries().is_empty());
    }

    #[test]
    fn load_reads_saved_lines_and_ignores_blanks() {
        let path = std::env::temp_dir().join(format!(
            "gototerm-bookmarks-load-{}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "/work/a\n\n/work/b\n").unwrap();

        let marks = Bookmarks::load_from_path(&path);

        assert_eq!(
            marks.entries(),
            [PathBuf::from("/work/a"), PathBuf::from("/work/b")]
        );
        assert!(marks.contains(Path::new("/work/b")));
        let _ = std::fs::remove_file(&path);
    }
}
