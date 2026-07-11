use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 20;

pub struct RecentProjects {
    entries: Vec<PathBuf>,
}

impl RecentProjects {
    pub fn load() -> Self {
        Self::load_from_path(&recent_projects_path())
    }

    fn load_from_path(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self {
                entries: Vec::new(),
            };
        };

        let mut recent = Self {
            entries: Vec::new(),
        };
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            recent.record_in_memory(PathBuf::from(line));
        }
        recent
    }

    pub fn record(&mut self, path: &Path) {
        self.record_in_memory(path.to_path_buf());
        self.save();
    }

    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    fn record_in_memory(&mut self, path: PathBuf) {
        record_path(&mut self.entries, path);
    }

    fn save(&self) {
        let path = recent_projects_path();
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

fn record_path(entries: &mut Vec<PathBuf>, path: PathBuf) {
    entries.retain(|entry| entry != &path);
    entries.insert(0, path);
    entries.truncate(MAX_RECENT);
}

fn recent_projects_path() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));

    base.unwrap_or_else(std::env::temp_dir)
        .join("gototerm")
        .join("recent_projects.txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_moves_existing_path_to_front_without_duplicate() {
        let mut entries = vec![
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/c"),
        ];

        record_path(&mut entries, PathBuf::from("/b"));

        assert_eq!(
            entries,
            vec![
                PathBuf::from("/b"),
                PathBuf::from("/a"),
                PathBuf::from("/c")
            ]
        );
    }

    #[test]
    fn record_keeps_only_twenty_entries() {
        let mut entries = Vec::new();

        for i in 0..25 {
            record_path(&mut entries, PathBuf::from(format!("/project-{i}")));
        }

        assert_eq!(entries.len(), 20);
        assert_eq!(entries.first(), Some(&PathBuf::from("/project-24")));
        assert_eq!(entries.last(), Some(&PathBuf::from("/project-5")));
    }

    #[test]
    fn record_preserves_nonexistent_paths_without_stat() {
        let mut entries = Vec::new();
        let path = PathBuf::from("/definitely/not/a/real/gototerm/project");

        record_path(&mut entries, path.clone());

        assert_eq!(entries, vec![path]);
    }

    #[test]
    fn load_returns_empty_when_file_is_missing() {
        let path = std::env::temp_dir().join(format!(
            "gototerm-missing-recent-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let recent = RecentProjects::load_from_path(&path);

        assert!(recent.entries().is_empty());
    }
}
