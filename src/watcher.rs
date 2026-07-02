use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// 監視するディレクトリ数の上限。ホーム直下（10万dir超・ネットワークマウント
/// 含む）のような巨大フォルダで inotify 登録が際限なく増えるのを防ぐ。
/// 超えた分は監視されない（部分監視＝ is_partial() が true）。
const MAX_WATCHED_DIRS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    New,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

pub struct WorkspaceWatcher {
    watcher: RecommendedWatcher,
    receiver: Receiver<notify::Result<Event>>,
    root: PathBuf,
    ignore_patterns: Vec<String>,
    watched_dirs: usize,
    partial: bool,
}

impl WorkspaceWatcher {
    /// root 配下を監視する。notify の `RecursiveMode::Recursive` は .git や
    /// target・ネットワークマウントも無条件に舐めて数十秒ブロックするため
    /// 使わず、自前の BFS でディレクトリを1つずつ非再帰登録する（隠し・
    /// ignore 対象・シンボリックリンクには入らない。上限 MAX_WATCHED_DIRS）。
    /// それでもブロックはするので、**UI スレッドから直接呼ばないこと**
    /// （サイドバーがバックグラウンドスレッドから呼ぶ）。
    pub fn new(root: &Path) -> Result<Self, notify::Error> {
        let (sender, receiver) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(
            move |event| {
                let _ = sender.send(event);
            },
            Config::default(),
        )?;
        let ignore_patterns = crate::TOYTERM_CONFIG.watch_ignore.clone();
        let mut watched_dirs = 0;
        let partial = add_watches_bfs(&mut watcher, root, &ignore_patterns, &mut watched_dirs)?;

        Ok(Self {
            watcher,
            receiver,
            root: root.to_path_buf(),
            ignore_patterns,
            watched_dirs,
            partial,
        })
    }

    /// 上限打ち切りで一部のディレクトリしか監視できていないか。
    pub fn is_partial(&self) -> bool {
        self.partial
    }

    pub fn drain(&mut self) -> Vec<FileChange> {
        let mut changes = Vec::new();

        // 監視の追加登録（下記）に &mut self.watcher が要るので、先にイベントを回収する。
        let events: Vec<Event> = self.receiver.try_iter().flatten().collect();
        for event in events {
            let Some(kind) = change_kind_for_event(&event.kind) else {
                continue;
            };

            for path in event.paths {
                let rel_path = relative_path(&self.root, &path);
                if rel_path.as_os_str().is_empty() || is_ignored(&rel_path, &self.ignore_patterns) {
                    continue;
                }
                // 非再帰登録なので、新しく作られたディレクトリは自分で監視に加える
                // （さもないとその中に作られるファイルが見えない）。
                if kind == ChangeKind::New && path.is_dir() {
                    let truncated = add_watches_bfs(
                        &mut self.watcher,
                        &path,
                        &self.ignore_patterns,
                        &mut self.watched_dirs,
                    )
                    .unwrap_or(true);
                    if truncated {
                        self.partial = true;
                    }
                    // ディレクトリ自体は changed files に出さない（中のファイルで分かる）
                    continue;
                }
                changes.push(FileChange {
                    path: rel_path,
                    kind,
                });
            }
        }

        changes
    }
}

/// BFS で root 配下のディレクトリを非再帰登録する。浅い階層を優先するので、
/// 上限で打ち切られても作業フォルダ直下は必ず監視される。
/// 打ち切りが起きたら true を返す。root 自体の登録失敗だけはエラーにする。
fn add_watches_bfs(
    watcher: &mut RecommendedWatcher,
    root: &Path,
    patterns: &[String],
    watched: &mut usize,
) -> Result<bool, notify::Error> {
    if *watched >= MAX_WATCHED_DIRS {
        return Ok(true);
    }
    watcher.watch(root, RecursiveMode::NonRecursive)?;
    *watched += 1;

    let mut queue = VecDeque::new();
    enqueue_subdirs(root, patterns, &mut queue);

    while let Some(dir) = queue.pop_front() {
        if *watched >= MAX_WATCHED_DIRS {
            return Ok(true);
        }
        // 消えた・読めないディレクトリはスキップして監視自体は続ける。
        if watcher.watch(&dir, RecursiveMode::NonRecursive).is_err() {
            continue;
        }
        *watched += 1;
        enqueue_subdirs(&dir, patterns, &mut queue);
    }

    Ok(false)
}

/// dir 直下のうち「入るべき」サブディレクトリをキューに足す。
fn enqueue_subdirs(dir: &Path, patterns: &[String], queue: &mut VecDeque<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // file_type() はシンボリックリンクを辿らない＝リンク先の巨大ツリーや
        // ループに引き込まれない。
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if should_descend(name, patterns) {
            queue.push_back(entry.path());
        }
    }
}

/// 監視ツリーに入るべきディレクトリ名か。
/// 隠しディレクトリ（.cache 等は巨大）と ignore 対象には入らない。
pub(crate) fn should_descend(name: &str, patterns: &[String]) -> bool {
    !name.starts_with('.') && !patterns.iter().any(|pattern| name == pattern)
}

fn change_kind_for_event(kind: &EventKind) -> Option<ChangeKind> {
    match kind {
        EventKind::Create(_) => Some(ChangeKind::New),
        EventKind::Modify(_) => Some(ChangeKind::Modified),
        EventKind::Remove(_) => Some(ChangeKind::Deleted),
        _ => None,
    }
}

/// 同一パスに複数イベントが来たときの合成規則。
/// New→Modified は New のまま（「新規作成されて編集中」）。
/// なんであれ最後に Deleted が来たら Deleted。
/// Deleted の後に Create が来たら Modified（上書き保存のパターン）。
pub(crate) fn merge_kind(prev: Option<ChangeKind>, next: ChangeKind) -> ChangeKind {
    match (prev, next) {
        (_, ChangeKind::Deleted) => ChangeKind::Deleted,
        (Some(ChangeKind::Deleted), ChangeKind::New) => ChangeKind::Modified,
        (Some(ChangeKind::New), ChangeKind::Modified) => ChangeKind::New,
        (_, next) => next,
    }
}

/// パスのどこかの構成要素が patterns に一致したら無視。
pub(crate) fn is_ignored(rel_path: &Path, patterns: &[String]) -> bool {
    rel_path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .any(|component| patterns.iter().any(|pattern| component == pattern))
}

pub(crate) fn relative_path(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_keeps_new_when_modified_after_create() {
        assert_eq!(
            merge_kind(Some(ChangeKind::New), ChangeKind::Modified),
            ChangeKind::New
        );
    }

    #[test]
    fn merge_delete_wins_over_previous_kind() {
        assert_eq!(
            merge_kind(Some(ChangeKind::New), ChangeKind::Deleted),
            ChangeKind::Deleted
        );
        assert_eq!(
            merge_kind(Some(ChangeKind::Modified), ChangeKind::Deleted),
            ChangeKind::Deleted
        );
    }

    #[test]
    fn merge_create_after_delete_becomes_modified() {
        assert_eq!(
            merge_kind(Some(ChangeKind::Deleted), ChangeKind::New),
            ChangeKind::Modified
        );
    }

    #[test]
    fn ignored_matches_any_path_component() {
        let patterns = vec![".git".to_owned(), "target".to_owned()];

        assert!(is_ignored(Path::new(".git/index"), &patterns));
        assert!(is_ignored(Path::new("crates/app/target/foo.o"), &patterns));
        assert!(!is_ignored(Path::new("src/targeted.rs"), &patterns));
    }

    #[test]
    fn relative_path_strips_watched_root() {
        assert_eq!(
            relative_path(Path::new("/repo"), Path::new("/repo/src/main.rs")),
            PathBuf::from("src/main.rs")
        );
    }

    #[test]
    fn descend_skips_hidden_and_ignored_dirs() {
        let patterns = vec!["node_modules".to_owned(), "target".to_owned()];

        assert!(should_descend("src", &patterns));
        assert!(should_descend("docs", &patterns));
        assert!(!should_descend(".git", &patterns));
        assert!(!should_descend(".cache", &patterns));
        assert!(!should_descend("node_modules", &patterns));
        assert!(!should_descend("target", &patterns));
    }
}
