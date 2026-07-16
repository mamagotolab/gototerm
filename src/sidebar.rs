use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthChar;
use winit::{
    dpi::PhysicalPosition,
    event::{ElementState, KeyEvent},
    keyboard::{KeyCode, PhysicalKey},
};

use crate::terminal::{Cell, Color, GraphicAttribute, Line};
use crate::view::{TerminalView, Viewport};
use crate::vt::ShellLocation;
use crate::watcher::{merge_kind, ChangeKind, FileChange, WorkspaceWatcher};
use crate::workspace::{self, WorkspaceInfo};
use crate::Display;

const REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const FILE_BROWSER_VISIBLE_ROWS: usize = 15;
const FILE_BROWSER_MAX_ENTRIES: usize = 500;

pub struct Sidebar {
    view: TerminalView,
    visible: bool,
    info: Option<WorkspaceInfo>,
    /// バックグラウンドで取得中の workspace 情報（git status は Windows では
    /// プロセス起動が重く、UI スレッドで実行すると数百 ms 固まるため）。
    info_pending: Option<(PathBuf, Receiver<WorkspaceInfo>)>,
    last_refresh: Instant,
    changes: Vec<FileChange>,
    mode: SidebarMode,
    browse_dir: PathBuf,
    browse_entries: Vec<(String, bool)>,
    browse_pending: Option<(PathBuf, Receiver<Result<Vec<(String, bool)>, String>>)>,
    browse_error: Option<String>,
    browse_scroll: usize,
    row_actions: Vec<Option<RowAction>>,
    focused: bool,
    browse_selected: usize,
    /// `/` 検索中の入力文字列（None=非検索）。前方一致で選択がジャンプする。
    search: Option<String>,
    watcher: Option<WorkspaceWatcher>,
    /// バックグラウンドで生成中の watcher。生成（ディレクトリ登録）は
    /// フォルダ規模によってはブロックするので、UI スレッドでは行わない。
    watcher_pending: Option<(PathBuf, Receiver<Result<WorkspaceWatcher, notify::Error>>)>,
    watcher_root: Option<PathBuf>,
    watch_failed: bool,
    remote_location: Option<(String, PathBuf)>,
    follow_target: Option<PathBuf>,
    /// 自動追従をフリーズ中か。true の間はプレビューを最新変更へ飛ばさない
    /// （他ターミナルの AI が同じプロジェクトを触ってもプレビューが動かない）。
    follow_frozen: bool,
    ai_activity: Option<AiActivity>,
}

impl Sidebar {
    pub fn new(display: Display, viewport: Viewport) -> Self {
        Sidebar {
            view: TerminalView::with_viewport(
                display,
                viewport,
                crate::TOYTERM_CONFIG.font_size,
                None,
            ),
            visible: false,
            info: None,
            info_pending: None,
            last_refresh: Instant::now() - REFRESH_INTERVAL,
            changes: Vec::new(),
            mode: SidebarMode::Files,
            browse_dir: PathBuf::new(),
            browse_entries: Vec::new(),
            browse_pending: None,
            browse_error: None,
            browse_scroll: 0,
            row_actions: Vec::new(),
            focused: false,
            browse_selected: 0,
            search: None,
            watcher: None,
            watcher_pending: None,
            watcher_root: None,
            watch_failed: false,
            remote_location: None,
            follow_target: None,
            follow_frozen: false,
            ai_activity: None,
        }
    }

    /// 自動追従の ON/OFF を切り替える。
    pub fn toggle_follow(&mut self) {
        self.follow_frozen = !self.follow_frozen;
        if self.follow_frozen {
            // フリーズ中は保留中の追従先を捨てる（解除時に飛ばないように）。
            self.follow_target = None;
        }
        self.rebuild();
    }

    pub fn toggle(&mut self, location: &ShellLocation) {
        self.visible = !self.visible;
        if self.visible {
            self.refresh_location(location);
        } else {
            self.focused = false;
            self.clear_live_state();
            self.remote_location = None;
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn set_focused(&mut self, focused: bool) {
        if self.focused == focused {
            return;
        }
        self.focused = focused && self.visible;
        // フォーカスが外れたら検索状態を捨てる（再フォーカスで入力欄が残らないよう）。
        if !self.focused {
            self.search = None;
        }
        self.rebuild();
    }

    pub fn contains(&self, p: PhysicalPosition<f64>) -> bool {
        self.visible && self.view.viewport().contains(p)
    }

    pub fn cell_height(&self) -> u32 {
        self.view.cell_size().h
    }

    pub fn on_click(&mut self, p: PhysicalPosition<f64>) -> Option<SidebarRequest> {
        if !self.contains(p) {
            return None;
        }
        let Some(action) = sidebar_action_at(
            &self.row_actions,
            click_row(self.view.viewport(), self.view.cell_size().h, p),
        )
        .cloned() else {
            return None;
        };

        self.run_row_action(action)
    }

    pub fn on_key(&mut self, key: &KeyEvent) -> SidebarKeyResult {
        if key.state != ElementState::Pressed {
            return SidebarKeyResult::Consumed;
        }
        let code = match key.physical_key {
            PhysicalKey::Code(code) => code,
            PhysicalKey::Unidentified(_) => return SidebarKeyResult::Consumed,
        };

        // 検索中は入力欄として振る舞い、他のキー（h/j/k/l 等）は横取りしない。
        if self.search.is_some() {
            return self.on_search_key(code, key.text.as_deref());
        }

        if code == KeyCode::Escape {
            return SidebarKeyResult::ReleaseFocus;
        }

        // `/` で検索開始（ranger/yazi 流儀）。数が多い一覧で頭文字ジャンプに使う。
        if key.text.as_deref() == Some("/") {
            self.search = Some(String::new());
            self.rebuild();
            return SidebarKeyResult::Consumed;
        }

        self.on_list_key(code)
    }

    fn on_search_key(&mut self, code: KeyCode, text: Option<&str>) -> SidebarKeyResult {
        match code {
            // Enter/Esc で検索終了（選択はジャンプ先に残す）。
            KeyCode::Escape | KeyCode::Enter => {
                self.search = None;
                self.rebuild();
            }
            KeyCode::Backspace => {
                if let Some(q) = self.search.as_mut() {
                    q.pop();
                }
                self.jump_to_search();
            }
            _ => {
                // 制御文字でない入力文字だけをクエリに足す。
                if let Some(t) = text {
                    if !t.is_empty() && t.chars().all(|c| !c.is_control()) {
                        if let Some(q) = self.search.as_mut() {
                            q.push_str(t);
                        }
                        self.jump_to_search();
                    }
                }
            }
        }
        SidebarKeyResult::Consumed
    }

    /// クエリに前方一致（大文字小文字無視）する最初の項目へ選択を移す。
    fn jump_to_search(&mut self) {
        let Some(query) = self.search.clone() else {
            return;
        };
        if query.is_empty() {
            self.rebuild();
            return;
        }
        let q = query.to_lowercase();
        match self.mode {
            SidebarMode::Files => {
                let parent_rows = usize::from(self.browse_dir.parent().is_some());
                if let Some(i) = self
                    .browse_entries
                    .iter()
                    .position(|(name, _)| name.to_lowercase().starts_with(&q))
                {
                    self.set_list_selection(parent_rows + i);
                    return;
                }
            }
            SidebarMode::Changes => {
                if let Some(i) = self.changes.iter().take(5).position(|c| {
                    c.path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.to_lowercase().starts_with(&q))
                        .unwrap_or(false)
                }) {
                    self.set_list_selection(i);
                    return;
                }
            }
        }
        // 一致なし：クエリ表示を更新するため再描画だけする。
        self.rebuild();
    }

    fn run_row_action(&mut self, action: RowAction) -> Option<SidebarRequest> {
        match action {
            RowAction::ToggleMode => {
                self.toggle_mode();
                None
            }
            RowAction::ToggleFollow => {
                self.toggle_follow();
                None
            }
            RowAction::BrowseParent => {
                if let Some(parent) = self.browse_dir.parent().map(Path::to_path_buf) {
                    self.set_browse_dir(parent);
                }
                None
            }
            RowAction::BrowseDir(path) => {
                self.set_browse_dir(path);
                None
            }
            RowAction::PreviewFile(path) => Some(SidebarRequest::PreviewFile(path)),
        }
    }

    fn on_list_key(&mut self, code: KeyCode) -> SidebarKeyResult {
        match code {
            // 上下移動：矢印＋ vim の k/j（ranger/yazi 流儀）。
            KeyCode::ArrowUp | KeyCode::KeyK => {
                self.move_list_selection(-1);
                SidebarKeyResult::Consumed
            }
            KeyCode::ArrowDown | KeyCode::KeyJ => {
                self.move_list_selection(1);
                SidebarKeyResult::Consumed
            }
            KeyCode::PageUp => SidebarKeyResult::Request(SidebarRequest::ScrollPreview(
                -(self.list_page_step() as isize),
            )),
            KeyCode::PageDown => SidebarKeyResult::Request(SidebarRequest::ScrollPreview(
                self.list_page_step() as isize,
            )),
            KeyCode::Home => {
                self.set_list_selection(0);
                SidebarKeyResult::Consumed
            }
            KeyCode::End => {
                let last = self.list_selectable_len().saturating_sub(1);
                self.set_list_selection(last);
                SidebarKeyResult::Consumed
            }
            // 開く／進む：Enter・→・vim の l（yazi/ranger 流儀: hで戻る/lで進む）。
            KeyCode::Enter | KeyCode::ArrowRight | KeyCode::KeyL => self
                .selected_row_action()
                .and_then(|action| self.run_row_action(action))
                .map_or(SidebarKeyResult::Consumed, SidebarKeyResult::Request),
            // 親へ戻る：Backspace・←・vim の h。
            KeyCode::Backspace | KeyCode::ArrowLeft | KeyCode::KeyH => {
                if self.mode == SidebarMode::Files {
                    return self
                        .run_row_action(RowAction::BrowseParent)
                        .map_or(SidebarKeyResult::Consumed, SidebarKeyResult::Request);
                }
                SidebarKeyResult::Consumed
            }
            KeyCode::Tab => {
                self.run_row_action(RowAction::ToggleMode);
                SidebarKeyResult::Consumed
            }
            KeyCode::KeyE => SidebarKeyResult::Request(SidebarRequest::EditPreview),
            KeyCode::KeyO => SidebarKeyResult::Request(SidebarRequest::OpenPreview),
            _ => SidebarKeyResult::Consumed,
        }
    }

    pub fn on_scroll(&mut self, delta: i32) {
        if !self.visible || delta == 0 {
            return;
        }

        if self.mode != SidebarMode::Files {
            return;
        }

        let visible = self.browse_entry_slots();
        let max = browse_scroll_max(self.browse_entries.len(), visible);
        let next = if delta > 0 {
            self.browse_scroll.saturating_sub(delta as usize)
        } else {
            self.browse_scroll
                .saturating_add(delta.unsigned_abs() as usize)
        };
        let clamped = clamp_browse_scroll(next, self.browse_entries.len(), visible);
        if clamped != self.browse_scroll {
            self.browse_scroll = clamped.min(max);
            self.rebuild();
        }
    }

    fn list_page_step(&self) -> usize {
        match self.mode {
            SidebarMode::Changes => 5,
            SidebarMode::Files => self.browse_entry_slots().max(1),
        }
    }

    fn list_selectable_len(&self) -> usize {
        match self.mode {
            SidebarMode::Changes => self.changes.len().min(5),
            SidebarMode::Files => {
                self.browse_entries.len() + usize::from(self.browse_dir.parent().is_some())
            }
        }
    }

    fn move_list_selection(&mut self, delta: isize) {
        let len = self.list_selectable_len();
        if len == 0 {
            self.browse_selected = 0;
            return;
        }
        let current = self.browse_selected.min(len - 1);
        let next = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta as usize).min(len - 1)
        };
        self.set_list_selection(next);
    }

    fn set_list_selection(&mut self, selected: usize) {
        let len = self.list_selectable_len();
        if len == 0 {
            self.browse_selected = 0;
            return;
        }
        self.browse_selected = selected.min(len - 1);
        if self.mode == SidebarMode::Files {
            let parent_rows = usize::from(self.browse_dir.parent().is_some());
            if self.browse_selected >= parent_rows {
                let entry_selected = self.browse_selected - parent_rows;
                self.browse_scroll = keep_selection_visible(
                    self.browse_scroll,
                    entry_selected,
                    self.browse_entries.len(),
                    self.browse_entry_slots(),
                );
            }
        }
        self.rebuild();
    }

    fn selected_row_action(&self) -> Option<RowAction> {
        match self.mode {
            SidebarMode::Changes => {
                if self.remote_location.is_some() {
                    return None;
                }
                let change = self.changes.get(self.browse_selected)?;
                Some(RowAction::PreviewFile(
                    self.watcher_root
                        .as_deref()
                        .unwrap_or_else(|| Path::new(""))
                        .join(&change.path),
                ))
            }
            SidebarMode::Files => {
                let parent_rows = usize::from(self.browse_dir.parent().is_some());
                if self.browse_selected < parent_rows {
                    return Some(RowAction::BrowseParent);
                }
                let entry = self.browse_selected.checked_sub(parent_rows)?;
                let (name, is_dir) = self.browse_entries.get(entry)?;
                let path = self.browse_dir.join(name);
                Some(if *is_dir {
                    RowAction::BrowseDir(path)
                } else {
                    RowAction::PreviewFile(path)
                })
            }
        }
    }

    pub fn take_follow_target(&mut self) -> Option<PathBuf> {
        if self.follow_frozen {
            self.follow_target = None;
            return None;
        }
        self.follow_target.take()
    }

    pub fn apply_gt_event(
        &mut self,
        root: Option<&Path>,
        kind: ChangeKind,
        path: PathBuf,
        tool: Option<String>,
    ) {
        let existing = self.changes.iter().position(|stored| stored.path == path);
        let prev = existing.map(|index| self.changes.remove(index).kind);
        let merged = merge_kind(prev, kind);
        self.changes.insert(
            0,
            FileChange {
                path: path.clone(),
                kind: merged,
            },
        );
        self.changes.truncate(100);

        self.ai_activity = Some(AiActivity {
            kind: merged,
            path: path.clone(),
            tool,
            at: Instant::now(),
        });

        if self.remote_location.is_none() {
            if let (Some(root), ChangeKind::New | ChangeKind::Modified) = (root, merged) {
                self.follow_target = Some(root.join(path));
            }
        }

        self.rebuild();
    }

    pub fn root(&self) -> Option<&Path> {
        self.watcher_root
            .as_deref()
            .or_else(|| self.info.as_ref().map(|info| info.cwd.as_path()))
    }

    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.view.set_viewport(viewport);
        self.rebuild();
    }

    pub fn change_font_size(&mut self, size_diff: i32) {
        self.view.increase_font_size(size_diff);
        self.rebuild();
    }

    pub fn draw(&mut self, surface: &mut glium::Frame) {
        if self.visible {
            self.view.draw(surface);
        }
    }

    pub fn needs_redraw(&self) -> bool {
        self.visible && self.view.needs_redraw()
    }

    pub fn refresh_if_stale(&mut self, location: &ShellLocation) {
        if !self.visible {
            return;
        }
        let ShellLocation::Local(cwd) = location else {
            self.set_remote(location);
            return;
        };
        if self.remote_location.is_some() {
            self.remote_location = None;
            self.info = None;
            self.refresh(cwd);
            return;
        }
        let mut changed = self.drain_watcher(cwd);
        if self.poll_browse_dir() {
            changed = true;
        }
        if self.poll_workspace() {
            changed = true;
        }
        // cd やフォーカス切替で作業フォルダが変わったら、5秒を待たずに反映する。
        // 取得中／取得済みの cwd と比べる（info の反映が遅れても再発注しないため）。
        let current_cwd = self
            .info_pending
            .as_ref()
            .map(|(p, _)| p.as_path())
            .or_else(|| self.info.as_ref().map(|info| info.cwd.as_path()));
        let cwd_changed = current_cwd != Some(cwd);
        if cwd_changed || self.last_refresh.elapsed() >= REFRESH_INTERVAL {
            self.refresh(cwd);
        } else if changed {
            self.rebuild();
        }
    }

    /// バックグラウンドの git 取得が完了していれば取り込む。true=更新した。
    fn poll_workspace(&mut self) -> bool {
        let Some((_, rx)) = &self.info_pending else {
            return false;
        };
        match rx.try_recv() {
            Ok(info) => {
                self.info = Some(info);
                self.info_pending = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.info_pending = None;
                false
            }
        }
    }

    fn refresh_location(&mut self, location: &ShellLocation) {
        match location {
            ShellLocation::Local(cwd) => {
                self.remote_location = None;
                self.refresh(cwd);
            }
            ShellLocation::Remote { .. } => self.set_remote(location),
        }
    }

    fn set_remote(&mut self, location: &ShellLocation) {
        let ShellLocation::Remote { host, path } = location else {
            return;
        };
        let next = Some((host.clone(), path.clone()));
        if self.remote_location.as_ref() != next.as_ref() {
            self.remote_location = next;
            self.info = None;
            self.clear_live_state();
            self.rebuild();
        }
    }

    fn clear_live_state(&mut self) {
        // リモート出力をローカルFSとして扱わないため、監視・一覧・プレビュー状態を捨てる。
        self.info_pending = None;
        self.watcher = None;
        self.watcher_pending = None;
        self.watcher_root = None;
        self.watch_failed = false;
        self.changes.clear();
        self.mode = SidebarMode::Files;
        self.browse_entries.clear();
        self.browse_pending = None;
        self.browse_error = None;
        self.browse_scroll = 0;
        self.row_actions.clear();
        self.browse_selected = 0;
        self.follow_target = None;
        self.ai_activity = None;
    }

    fn refresh(&mut self, cwd: &Path) {
        self.drain_watcher(cwd);
        // git status はバックグラウンドで取る（Windows で UI が固まるのを防ぐ）。
        // 既に同じ cwd を取得中なら二重発注しない。
        if self.info_pending.as_ref().map(|(p, _)| p.as_path()) != Some(cwd) {
            self.info_pending = Some(spawn_workspace_collect(cwd));
        }
        if self.browse_dir.as_os_str().is_empty() {
            self.browse_dir = cwd.to_path_buf();
        }
        self.last_refresh = Instant::now();
        self.rebuild();
    }

    fn drain_watcher(&mut self, cwd: &Path) -> bool {
        let mut changed = false;

        // root が変わったら履歴ごと作り直す。生成はブロックし得るので
        // バックグラウンドスレッドに投げ、完了は下で非ブロッキングに受け取る。
        // 失敗した root では再試行しない（毎tickの再試行はフリーズの元）。
        if self.watcher_root.as_deref() != Some(cwd) {
            self.changes.clear();
            self.watcher = None;
            self.watch_failed = false;
            self.watcher_root = Some(cwd.to_path_buf());
            self.follow_target = None;
            self.set_browse_dir_for_root(cwd.to_path_buf());
            self.watcher_pending = Some(spawn_watcher(cwd));
            changed = true;
        }

        if let Some((pending_root, rx)) = &self.watcher_pending {
            match rx.try_recv() {
                Ok(result) => {
                    // 生成中に root が変わっていたら結果ごと捨てる
                    // （root 変化は上の分岐が検知して作り直す）。
                    if self.watcher_root.as_deref() == Some(pending_root.as_path()) {
                        match result {
                            Ok(watcher) => self.watcher = Some(watcher),
                            Err(_) => self.watch_failed = true,
                        }
                        changed = true;
                    }
                    self.watcher_pending = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    // 生成スレッドが panic した等。失敗扱いにして再試行しない。
                    self.watcher_pending = None;
                    self.watch_failed = true;
                    changed = true;
                }
            }
        }

        if let Some(watcher) = &mut self.watcher {
            let drained = watcher.drain();
            if !drained.is_empty() {
                changed = true;
            }
            self.apply_changes(cwd, drained);
        }
        changed
    }

    fn apply_changes(&mut self, root: &Path, changes: Vec<FileChange>) {
        for change in changes {
            let existing = self
                .changes
                .iter()
                .position(|stored| stored.path == change.path);
            let prev = existing.map(|index| self.changes.remove(index).kind);
            let kind = merge_kind(prev, change.kind);
            self.changes.insert(
                0,
                FileChange {
                    path: change.path.clone(),
                    kind,
                },
            );

            match kind {
                ChangeKind::New | ChangeKind::Modified => {
                    self.follow_target = Some(root.join(&change.path));
                }
                ChangeKind::Deleted => {}
            }
        }
        self.changes.truncate(100);
    }

    fn rebuild(&mut self) {
        if !self.visible {
            return;
        }

        let cols = (self.view.viewport().w / self.view.cell_size().w).max(1) as usize;
        let rows = (self.view.viewport().h / self.view.cell_size().h).max(1) as usize;
        let content_rows = rows.saturating_sub(usize::from(self.focused));
        let mut lines = Vec::new();
        let mut row_actions = Vec::new();

        if let Some((host, path)) = &self.remote_location {
            let label = format!("{host}:{}", path.display());
            let dir = abbreviate_start(&label, cols.saturating_sub(9));
            push_segments(
                &mut lines,
                &mut row_actions,
                cols,
                &[(" remote ", Color::BrightWhite), (&dir, Color::White)],
                None,
            );
            self.push_ai_activity(&mut lines, &mut row_actions, cols);
            push_line(&mut lines, &mut row_actions, cols, "", Color::White, None);
            push_line(
                &mut lines,
                &mut row_actions,
                cols,
                &format!("リモート接続中 ({host})"),
                Color::BrightWhite,
                None,
            );
            push_line(
                &mut lines,
                &mut row_actions,
                cols,
                "ローカルのファイル監視・一覧は",
                Color::White,
                None,
            );
            push_line(
                &mut lines,
                &mut row_actions,
                cols,
                "このペインでは使えません。",
                Color::White,
                None,
            );
            push_line(&mut lines, &mut row_actions, cols, "", Color::White, None);
            push_line(
                &mut lines,
                &mut row_actions,
                cols,
                "リモートの内容を見るには:",
                Color::BrightBlack,
                None,
            );
            push_line(
                &mut lines,
                &mut row_actions,
                cols,
                "  gt view <file>",
                Color::BrightWhite,
                None,
            );
            push_line(&mut lines, &mut row_actions, cols, "", Color::White, None);
            self.push_changed_files(&mut lines, &mut row_actions, cols);
        } else if let Some(info) = &self.info {
            let dir = abbreviate_start(&info.cwd.display().to_string(), cols.saturating_sub(12));
            push_segments(
                &mut lines,
                &mut row_actions,
                cols,
                &[(" workspace ", Color::BrightWhite), (&dir, Color::White)],
                None,
            );

            match &info.git {
                Some(git) => {
                    push_line(
                        &mut lines,
                        &mut row_actions,
                        cols,
                        &format!(
                            " git: {}  +{} ~{} ?{}",
                            git.branch, git.staged, git.modified, git.untracked
                        ),
                        Color::White,
                        None,
                    );
                }
                None => {
                    push_line(
                        &mut lines,
                        &mut row_actions,
                        cols,
                        " git: not a git repo",
                        Color::BrightBlack,
                        None,
                    );
                }
            }

            self.push_follow_status(&mut lines, &mut row_actions, cols);
            self.push_ai_activity(&mut lines, &mut row_actions, cols);
            self.push_mode_switch(&mut lines, &mut row_actions, cols);
            push_separator(&mut lines, &mut row_actions, cols);
            match self.mode {
                SidebarMode::Changes => {
                    self.push_changed_files(&mut lines, &mut row_actions, cols);
                }
                SidebarMode::Files => {
                    self.push_file_browser(&mut lines, &mut row_actions, cols);
                }
            }
        }

        lines.truncate(content_rows);
        row_actions.truncate(content_rows);
        if self.focused {
            self.apply_selection_style(&mut lines, &row_actions);
            self.push_key_hint(&mut lines, &mut row_actions, cols);
        }
        self.row_actions = row_actions;

        self.view.update_contents(|view| {
            // ターミナル／ランチャーと同じ透過（セルの Color::Background クアッドで
            // 半透明を出す）。区切りはマネージャの黒フレームクリアが担う。
            view.bg_color = Color::Background;
            view.skip_default_bg = false;
            view.lines = lines;
            view.images = Vec::new();
            view.cursor = None;
            view.selection_range = None;
        });
    }

    fn apply_selection_style(&self, lines: &mut [Line], row_actions: &[Option<RowAction>]) {
        let Some(selected) = self.selected_row_action() else {
            return;
        };
        let Some(row) = row_actions
            .iter()
            .position(|action| action.as_ref() == Some(&selected))
        else {
            return;
        };
        if let Some(line) = lines.get_mut(row) {
            for cell in line.cells_mut() {
                // ランチャーと同じ Tokyo Night の青い選択バー。
                cell.attr.fg = crate::file_style::SEL_FG;
                cell.attr.bg = crate::file_style::SEL_BG;
            }
        }
    }

    fn push_key_hint(
        &self,
        lines: &mut Vec<Line>,
        row_actions: &mut Vec<Option<RowAction>>,
        cols: usize,
    ) {
        // 検索中はクエリを見せる。カーソル代わりに末尾へ '_' を付ける。
        if let Some(query) = &self.search {
            let line = format!(" /{query}_");
            push_line(lines, row_actions, cols, &line, Color::BrightYellow, None);
            return;
        }
        // ソリッド背景では BrightBlack(#414868) だと暗くて沈むので、Tokyo Night の
        // コメント色(#565F89)で読める明るさにする。
        let hint = " j/k:選択 l:開く h:上へ /:検索 Esc:端末";
        push_line(
            lines,
            row_actions,
            cols,
            hint,
            Color::Rgb { rgba: 0x565F_89FF },
            None,
        );
    }

    fn push_ai_activity(
        &self,
        lines: &mut Vec<Line>,
        row_actions: &mut Vec<Option<RowAction>>,
        cols: usize,
    ) {
        let Some(activity) = &self.ai_activity else {
            return;
        };
        if activity.at.elapsed() > Duration::from_secs(60) {
            return;
        }

        let (label, _) = change_label(activity.kind);
        let suffix = activity
            .tool
            .as_ref()
            .map(|tool| format!(" ({tool})"))
            .unwrap_or_default();
        let fixed = display_width(" ● claude  ") + display_width(label) + 1;
        let available = cols
            .saturating_sub(fixed)
            .saturating_sub(display_width(&suffix));
        let path = abbreviate_start(&activity.path.display().to_string(), available);
        push_segments(
            lines,
            row_actions,
            cols,
            &[
                (" ", Color::White),
                ("●", Color::Green),
                (" claude  ", Color::White),
                (label, Color::BrightWhite),
                (" ", Color::White),
                (&path, Color::White),
                (&suffix, Color::BrightBlack),
            ],
            None,
        );
    }

    /// 自動追従の状態を1行で見せる（クリックで切替。キーは Ctrl+Shift+P）。
    fn push_follow_status(
        &self,
        lines: &mut Vec<Line>,
        row_actions: &mut Vec<Option<RowAction>>,
        cols: usize,
    ) {
        let (icon, icon_color, label) = if self.follow_frozen {
            ("⏸", Color::BrightYellow, "追従: フリーズ中")
        } else {
            ("●", Color::Green, "追従: 自動")
        };
        push_segments(
            lines,
            row_actions,
            cols,
            &[
                (" ", Color::White),
                (icon, icon_color),
                (" ", Color::White),
                (label, Color::White),
                ("  (Ctrl+Shift+P)", Color::Rgb { rgba: 0x565F_89FF }),
            ],
            Some(RowAction::ToggleFollow),
        );
    }

    fn push_mode_switch(
        &self,
        lines: &mut Vec<Line>,
        row_actions: &mut Vec<Option<RowAction>>,
        cols: usize,
    ) {
        let changes = if self.mode == SidebarMode::Changes {
            "[changes]"
        } else {
            " changes "
        };
        let files = if self.mode == SidebarMode::Files {
            "[files]"
        } else {
            " files "
        };
        push_segments(
            lines,
            row_actions,
            cols,
            &[
                (" ", Color::White),
                (changes, Color::BrightWhite),
                ("  ", Color::White),
                (files, Color::BrightWhite),
            ],
            Some(RowAction::ToggleMode),
        );
    }

    fn push_changed_files(
        &self,
        lines: &mut Vec<Line>,
        row_actions: &mut Vec<Option<RowAction>>,
        cols: usize,
    ) {
        push_line(
            lines,
            row_actions,
            cols,
            " changed files",
            Color::BrightWhite,
            None,
        );

        if self.watch_failed {
            push_line(
                lines,
                row_actions,
                cols,
                "   watch: 監視を開始できません",
                Color::BrightBlack,
                None,
            );
            return;
        }

        if self.watcher_pending.is_some() && self.watcher.is_none() {
            push_line(
                lines,
                row_actions,
                cols,
                "   (監視を準備中…)",
                Color::BrightBlack,
                None,
            );
            return;
        }

        if self.changes.is_empty() {
            push_line(
                lines,
                row_actions,
                cols,
                "   (変更なし)",
                Color::BrightBlack,
                None,
            );
            return;
        }

        let item_rows = self.changes.len().min(5);
        let partial_note = self.watcher.as_ref().is_some_and(|w| w.is_partial());

        for change in self.changes.iter().take(item_rows) {
            let (label, color) = change_label(change.kind);
            let path = change.path.display().to_string();
            let path = abbreviate_start(&path, cols.saturating_sub(10));
            let marker = "   ";
            let action = self.remote_location.is_none().then(|| {
                RowAction::PreviewFile(
                    self.watcher_root
                        .as_deref()
                        .unwrap_or_else(|| Path::new(""))
                        .join(&change.path),
                )
            });
            push_segments(
                lines,
                row_actions,
                cols,
                &[
                    (marker, Color::White),
                    (label, color),
                    ("  ", Color::White),
                    (&path, Color::White),
                ],
                action,
            );
        }

        if self.changes.len() > item_rows {
            let rest = self.changes.len().saturating_sub(item_rows);
            push_line(
                lines,
                row_actions,
                cols,
                &format!("   … ほか {rest} 件"),
                Color::BrightBlack,
                None,
            );
        }

        if partial_note {
            push_line(
                lines,
                row_actions,
                cols,
                "   (フォルダが多いため一部のみ監視)",
                Color::BrightBlack,
                None,
            );
        }
    }

    fn push_file_browser(
        &self,
        lines: &mut Vec<Line>,
        row_actions: &mut Vec<Option<RowAction>>,
        cols: usize,
    ) {
        let dir = abbreviate_start(
            &self.browse_dir.display().to_string(),
            cols.saturating_sub(8),
        );
        push_segments(
            lines,
            row_actions,
            cols,
            &[(" files  ", Color::BrightWhite), (&dir, Color::White)],
            None,
        );

        if self.browse_pending.is_some() {
            push_line(
                lines,
                row_actions,
                cols,
                "   (読込中…)",
                Color::BrightBlack,
                None,
            );
            return;
        }

        if self.browse_error.is_some() {
            push_line(
                lines,
                row_actions,
                cols,
                "   (読込できません)",
                Color::BrightBlack,
                None,
            );
            return;
        }

        let visible_entries = self.browse_entry_slots();
        let scroll = clamp_browse_scroll(
            self.browse_scroll,
            self.browse_entries.len(),
            visible_entries,
        );

        if self.browse_dir.parent().is_some() {
            let (icon, color) = crate::file_style::icon_and_color("..", true);
            push_line(
                lines,
                row_actions,
                cols,
                &format!("  {icon}  ../"),
                color,
                Some(RowAction::BrowseParent),
            );
        }

        for (name, is_dir) in self
            .browse_entries
            .iter()
            .skip(scroll)
            .take(visible_entries)
        {
            let (icon, color) = crate::file_style::icon_and_color(name, *is_dir);
            let suffix = if *is_dir { "/" } else { "" };
            // アイコン＋余白(計5列)を空けて、名前だけを幅に合わせて省略する。
            let inner = abbreviate_start(&format!("{name}{suffix}"), cols.saturating_sub(5));
            let label = format!("  {icon}  {inner}");
            let path = self.browse_dir.join(name);
            let action = if *is_dir {
                RowAction::BrowseDir(path)
            } else {
                RowAction::PreviewFile(path)
            };
            push_line(lines, row_actions, cols, &label, color, Some(action));
        }

        if self.browse_entries.is_empty() {
            push_line(
                lines,
                row_actions,
                cols,
                "   (空)",
                Color::BrightBlack,
                None,
            );
        } else {
            let shown = self
                .browse_entries
                .len()
                .saturating_sub(scroll)
                .min(visible_entries);
            let hidden = self
                .browse_entries
                .len()
                .saturating_sub(scroll.saturating_add(shown));
            if hidden > 0 {
                push_line(
                    lines,
                    row_actions,
                    cols,
                    &format!("   … ほか {hidden} 件"),
                    Color::BrightBlack,
                    None,
                );
            }
        }
    }

    fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            SidebarMode::Changes => SidebarMode::Files,
            SidebarMode::Files => SidebarMode::Changes,
        };
        self.search = None;
        self.browse_selected = 0;
        self.browse_scroll = 0;
        if self.mode == SidebarMode::Files && self.browse_entries.is_empty() {
            self.request_browse_dir();
        }
        self.rebuild();
    }

    fn set_browse_dir_for_root(&mut self, dir: PathBuf) {
        self.browse_dir = dir;
        self.browse_entries.clear();
        self.browse_pending = None;
        self.browse_error = None;
        self.browse_scroll = 0;
        self.browse_selected = 0;
        if self.mode == SidebarMode::Files {
            self.request_browse_dir();
        }
    }

    fn set_browse_dir(&mut self, dir: PathBuf) {
        self.browse_dir = dir;
        self.search = None;
        self.browse_scroll = 0;
        self.browse_selected = 0;
        self.request_browse_dir();
        self.rebuild();
    }

    fn request_browse_dir(&mut self) {
        self.browse_entries.clear();
        self.browse_error = None;
        self.browse_pending = Some(spawn_read_dir(self.browse_dir.clone()));
    }

    fn poll_browse_dir(&mut self) -> bool {
        let Some((pending_dir, rx)) = &self.browse_pending else {
            return false;
        };

        match rx.try_recv() {
            Ok(result) => {
                if *pending_dir == self.browse_dir {
                    match result {
                        Ok(entries) => {
                            let parent_rows = usize::from(self.browse_dir.parent().is_some());
                            let base = FILE_BROWSER_VISIBLE_ROWS.saturating_sub(parent_rows);
                            let visible = if entries.len() > base {
                                base.saturating_sub(1)
                            } else {
                                base
                            };
                            self.browse_scroll =
                                clamp_browse_scroll(self.browse_scroll, entries.len(), visible);
                            self.browse_entries = entries;
                            let selectable = self.browse_entries.len()
                                + usize::from(self.browse_dir.parent().is_some());
                            self.browse_selected =
                                self.browse_selected.min(selectable.saturating_sub(1));
                            self.browse_error = None;
                        }
                        Err(error) => {
                            self.browse_entries.clear();
                            self.browse_error = Some(error);
                        }
                    }
                }
                self.browse_pending = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.browse_pending = None;
                self.browse_entries.clear();
                self.browse_error = Some("read thread disconnected".to_owned());
                true
            }
        }
    }

    fn browse_entry_slots(&self) -> usize {
        let parent_rows = usize::from(self.browse_dir.parent().is_some());
        let base = FILE_BROWSER_VISIBLE_ROWS.saturating_sub(parent_rows);
        if self.browse_entries.len() > base {
            // 「ほか N 件」行を同じ 15 行枠に収めるため、長い一覧では1行予約する。
            base.saturating_sub(1)
        } else {
            base
        }
    }
}

pub enum SidebarRequest {
    PreviewFile(PathBuf),
    ScrollPreview(isize),
    EditPreview,
    OpenPreview,
}

pub enum SidebarKeyResult {
    Consumed,
    ReleaseFocus,
    Request(SidebarRequest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarMode {
    Changes,
    Files,
}

struct AiActivity {
    kind: ChangeKind,
    path: PathBuf,
    tool: Option<String>,
    at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RowAction {
    ToggleMode,
    ToggleFollow,
    BrowseParent,
    BrowseDir(PathBuf),
    PreviewFile(PathBuf),
}

/// watcher の生成をバックグラウンドで行う（生成＝ディレクトリ登録は
/// フォルダ規模によっては長くブロックするため、UI スレッドから外す）。
fn spawn_watcher(root: &Path) -> (PathBuf, Receiver<Result<WorkspaceWatcher, notify::Error>>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let root_owned = root.to_path_buf();
    let thread_root = root_owned.clone();
    std::thread::spawn(move || {
        let _ = tx.send(WorkspaceWatcher::new(&thread_root));
    });
    (root_owned, rx)
}

/// git status を別スレッドで取る（Windows のプロセス起動遅延で UI を固めない）。
fn spawn_workspace_collect(cwd: &Path) -> (PathBuf, Receiver<WorkspaceInfo>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let cwd_owned = cwd.to_path_buf();
    let thread_cwd = cwd_owned.clone();
    std::thread::spawn(move || {
        let _ = tx.send(workspace::collect(&thread_cwd));
    });
    (cwd_owned, rx)
}

fn spawn_read_dir(dir: PathBuf) -> (PathBuf, Receiver<Result<Vec<(String, bool)>, String>>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let thread_dir = dir.clone();
    std::thread::spawn(move || {
        let result = read_dir_entries(&thread_dir);
        let _ = tx.send(result);
    });
    (dir, rx)
}

fn read_dir_entries(dir: &Path) -> Result<Vec<(String, bool)>, String> {
    let mut entries = Vec::new();
    let read_dir = std::fs::read_dir(dir).map_err(|error| error.to_string())?;

    for entry in read_dir {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir();
        entries.push((name, is_dir));
    }

    Ok(sort_entries(entries, FILE_BROWSER_MAX_ENTRIES))
}

/// read_dir の結果を表示順に並べる。隠しファイルは選択対象にしないためここで落とす。
pub(crate) fn sort_entries(entries: Vec<(String, bool)>, max: usize) -> Vec<(String, bool)> {
    let mut entries: Vec<(String, bool)> = entries
        .into_iter()
        .filter(|(name, _)| !name.starts_with('.'))
        .collect();
    entries.sort_by(|(left_name, left_dir), (right_name, right_dir)| {
        right_dir
            .cmp(left_dir)
            .then_with(|| left_name.cmp(right_name))
    });
    entries.truncate(max);
    entries
}

pub(crate) fn clamp_browse_scroll(offset: usize, len: usize, visible: usize) -> usize {
    offset.min(browse_scroll_max(len, visible))
}

pub(crate) fn keep_selection_visible(
    scroll: usize,
    selected: usize,
    len: usize,
    visible: usize,
) -> usize {
    if visible == 0 || len == 0 {
        return clamp_browse_scroll(scroll, len, visible);
    }
    let scroll = clamp_browse_scroll(scroll, len, visible);
    if selected < scroll {
        selected
    } else if selected >= scroll.saturating_add(visible) {
        clamp_browse_scroll(
            selected.saturating_add(1).saturating_sub(visible),
            len,
            visible,
        )
    } else {
        scroll
    }
}

fn browse_scroll_max(len: usize, visible: usize) -> usize {
    len.saturating_sub(visible)
}

fn change_label(kind: ChangeKind) -> (&'static str, Color) {
    match kind {
        ChangeKind::New => ("NEW", Color::Green),
        ChangeKind::Modified => ("MOD", Color::Yellow),
        ChangeKind::Deleted => ("DEL", Color::Red),
    }
}

fn click_row(viewport: Viewport, cell_height: u32, p: PhysicalPosition<f64>) -> usize {
    ((p.y - viewport.y as f64) / cell_height.max(1) as f64) as usize
}

fn sidebar_action_at(row_actions: &[Option<RowAction>], row: usize) -> Option<&RowAction> {
    row_actions.get(row).and_then(Option::as_ref)
}

fn push_separator(lines: &mut Vec<Line>, row_actions: &mut Vec<Option<RowAction>>, cols: usize) {
    push_line(lines, row_actions, cols, " \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}", Color::BrightBlack, None);
}

fn push_line(
    lines: &mut Vec<Line>,
    row_actions: &mut Vec<Option<RowAction>>,
    cols: usize,
    text: &str,
    fg: Color,
    action: Option<RowAction>,
) {
    lines.push(Line::from_cells(cells_for_line(text, cols, fg), false));
    row_actions.push(action);
}

fn cells_for_line(text: &str, cols: usize, fg: Color) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut used = 0usize;

    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width == 0 || used + width > cols {
            return cells;
        }
        let mut cell = Cell::head(ch, width as u16, GraphicAttribute::default());
        cell.attr.fg = fg;
        cells.push(cell);
        for i in 1..width {
            cells.push(Cell::spacer(i as u16));
        }
        used += width;
    }

    cells
}

fn push_segments(
    lines: &mut Vec<Line>,
    row_actions: &mut Vec<Option<RowAction>>,
    cols: usize,
    segments: &[(&str, Color)],
    action: Option<RowAction>,
) {
    let mut cells = Vec::new();
    let mut used = 0usize;

    for (text, fg) in segments {
        for ch in text.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width == 0 || used + width > cols {
                lines.push(Line::from_cells(cells, false));
                row_actions.push(action);
                return;
            }

            let mut cell = Cell::head(ch, width as u16, GraphicAttribute::default());
            cell.attr.fg = *fg;
            cells.push(cell);
            for i in 1..width {
                cells.push(Cell::spacer(i as u16));
            }
            used += width;
        }
    }

    lines.push(Line::from_cells(cells, false));
    row_actions.push(action);
}

fn abbreviate_start(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "\u{2026}".to_owned();
    }

    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars().rev() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width == 0 || used + width > max_width - 1 {
            break;
        }
        out.insert(0, ch);
        used += width;
    }
    format!("\u{2026}{out}")
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::{
        clamp_browse_scroll, click_row, keep_selection_visible, sidebar_action_at, sort_entries,
        RowAction,
    };
    use crate::view::Viewport;
    use std::path::PathBuf;
    use winit::dpi::PhysicalPosition;

    #[test]
    fn click_row_uses_viewport_origin_and_cell_height() {
        let viewport = Viewport {
            x: 100,
            y: 40,
            w: 300,
            h: 200,
        };

        assert_eq!(
            click_row(viewport, 20, PhysicalPosition::new(120.0, 40.0)),
            0
        );
        assert_eq!(
            click_row(viewport, 20, PhysicalPosition::new(120.0, 79.0)),
            1
        );
        assert_eq!(
            click_row(viewport, 20, PhysicalPosition::new(120.0, 80.0)),
            2
        );
    }

    #[test]
    fn sidebar_action_at_returns_only_action_rows() {
        let file = PathBuf::from("/tmp/demo.txt");
        let actions = vec![
            None,
            Some(RowAction::PreviewFile(file.clone())),
            Some(RowAction::BrowseParent),
        ];

        assert_eq!(sidebar_action_at(&actions, 0), None);
        assert_eq!(
            sidebar_action_at(&actions, 1),
            Some(&RowAction::PreviewFile(file))
        );
        assert_eq!(
            sidebar_action_at(&actions, 2),
            Some(&RowAction::BrowseParent)
        );
        assert_eq!(sidebar_action_at(&actions, 3), None);
    }

    #[test]
    fn sort_entries_puts_directories_first_then_names() {
        let entries = vec![
            ("z.txt".to_owned(), false),
            ("src".to_owned(), true),
            ("README.md".to_owned(), false),
            ("docs".to_owned(), true),
        ];

        assert_eq!(
            sort_entries(entries, 10),
            vec![
                ("docs".to_owned(), true),
                ("src".to_owned(), true),
                ("README.md".to_owned(), false),
                ("z.txt".to_owned(), false),
            ]
        );
    }

    #[test]
    fn sort_entries_filters_hidden_names() {
        let entries = vec![
            (".git".to_owned(), true),
            ("src".to_owned(), true),
            (".env".to_owned(), false),
            ("Cargo.toml".to_owned(), false),
        ];

        assert_eq!(
            sort_entries(entries, 10),
            vec![("src".to_owned(), true), ("Cargo.toml".to_owned(), false),]
        );
    }

    #[test]
    fn sort_entries_truncates_after_sorting() {
        let entries = vec![
            ("c.txt".to_owned(), false),
            ("b_dir".to_owned(), true),
            ("a_dir".to_owned(), true),
            ("a.txt".to_owned(), false),
        ];

        assert_eq!(
            sort_entries(entries, 2),
            vec![("a_dir".to_owned(), true), ("b_dir".to_owned(), true)]
        );
    }

    #[test]
    fn sort_entries_handles_empty_directory() {
        assert!(sort_entries(Vec::new(), 10).is_empty());
    }

    #[test]
    fn clamp_browse_scroll_stays_within_visible_range() {
        assert_eq!(clamp_browse_scroll(0, 20, 5), 0);
        assert_eq!(clamp_browse_scroll(12, 20, 5), 12);
        assert_eq!(clamp_browse_scroll(99, 20, 5), 15);
    }

    #[test]
    fn clamp_browse_scroll_handles_short_or_empty_lists() {
        assert_eq!(clamp_browse_scroll(3, 4, 10), 0);
        assert_eq!(clamp_browse_scroll(3, 0, 10), 0);
        assert_eq!(clamp_browse_scroll(3, 4, 0), 3);
    }

    #[test]
    fn keep_selection_visible_scrolls_down_to_selected_entry() {
        assert_eq!(keep_selection_visible(0, 5, 20, 5), 1);
        assert_eq!(keep_selection_visible(1, 5, 20, 5), 1);
        assert_eq!(keep_selection_visible(1, 6, 20, 5), 2);
    }

    #[test]
    fn keep_selection_visible_scrolls_up_to_selected_entry() {
        assert_eq!(keep_selection_visible(10, 9, 20, 5), 9);
        assert_eq!(keep_selection_visible(10, 10, 20, 5), 10);
        assert_eq!(keep_selection_visible(10, 14, 20, 5), 10);
    }

    #[test]
    fn keep_selection_visible_clamps_to_scroll_range() {
        assert_eq!(keep_selection_visible(99, 19, 20, 5), 15);
        assert_eq!(keep_selection_visible(3, 0, 0, 5), 0);
        assert_eq!(keep_selection_visible(3, 2, 4, 0), 3);
    }
}
