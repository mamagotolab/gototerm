use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthChar;
use winit::{
    dpi::PhysicalPosition,
    event::{ElementState, KeyEvent},
    keyboard::{KeyCode, PhysicalKey},
};

use crate::config::resolve_editor;
use crate::preview::{FilePreview, PreviewLines};
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
    last_refresh: Instant,
    changes: Vec<FileChange>,
    preview: FilePreview,
    pinned: bool,
    mode: SidebarMode,
    sidebar_view: SidebarView,
    reader_scroll: usize,
    reader_lines: Vec<StyledLine>,
    reader_notice: Option<String>,
    browse_dir: PathBuf,
    browse_entries: Vec<(String, bool)>,
    browse_pending: Option<(PathBuf, Receiver<Result<Vec<(String, bool)>, String>>)>,
    browse_error: Option<String>,
    browse_scroll: usize,
    row_actions: Vec<Option<RowAction>>,
    focused: bool,
    browse_selected: usize,
    watcher: Option<WorkspaceWatcher>,
    /// バックグラウンドで生成中の watcher。生成（ディレクトリ登録）は
    /// フォルダ規模によってはブロックするので、UI スレッドでは行わない。
    watcher_pending: Option<(PathBuf, Receiver<Result<WorkspaceWatcher, notify::Error>>)>,
    watcher_root: Option<PathBuf>,
    watch_failed: bool,
    remote_location: Option<(String, PathBuf)>,
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
            last_refresh: Instant::now() - REFRESH_INTERVAL,
            changes: Vec::new(),
            preview: FilePreview::new(),
            pinned: false,
            mode: SidebarMode::Files,
            sidebar_view: SidebarView::List,
            reader_scroll: 0,
            reader_lines: Vec::new(),
            reader_notice: None,
            browse_dir: PathBuf::new(),
            browse_entries: Vec::new(),
            browse_pending: None,
            browse_error: None,
            browse_scroll: 0,
            row_actions: Vec::new(),
            focused: false,
            browse_selected: 0,
            watcher: None,
            watcher_pending: None,
            watcher_root: None,
            watch_failed: false,
            remote_location: None,
        }
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

        if code == KeyCode::Escape {
            return SidebarKeyResult::ReleaseFocus;
        }

        if self.sidebar_view == SidebarView::Reader {
            return self.on_reader_key(code);
        }

        self.on_list_key(code)
    }

    fn run_row_action(&mut self, action: RowAction) -> Option<SidebarRequest> {
        match action {
            RowAction::ToggleMode => {
                self.toggle_mode();
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
            RowAction::PreviewFile(path) => {
                self.preview_pinned(&path);
                Some(SidebarRequest::RefreshLayout)
            }
            RowAction::Unpin => {
                self.unpin();
                None
            }
            RowAction::EditFile(path) => Some(SidebarRequest::EditFile(path)),
            RowAction::CloseReader => {
                self.sidebar_view = SidebarView::List;
                self.reader_scroll = 0;
                self.reader_notice = None;
                self.rebuild();
                Some(SidebarRequest::RefreshLayout)
            }
            RowAction::OpenWithSystem(path) => Some(SidebarRequest::OpenWithSystem(path)),
        }
    }

    fn on_list_key(&mut self, code: KeyCode) -> SidebarKeyResult {
        match code {
            KeyCode::ArrowUp => {
                self.move_list_selection(-1);
                SidebarKeyResult::Consumed
            }
            KeyCode::ArrowDown => {
                self.move_list_selection(1);
                SidebarKeyResult::Consumed
            }
            KeyCode::PageUp => {
                self.move_list_selection(-(self.list_page_step() as isize));
                SidebarKeyResult::Consumed
            }
            KeyCode::PageDown => {
                self.move_list_selection(self.list_page_step() as isize);
                SidebarKeyResult::Consumed
            }
            KeyCode::Home => {
                self.set_list_selection(0);
                SidebarKeyResult::Consumed
            }
            KeyCode::End => {
                let last = self.list_selectable_len().saturating_sub(1);
                self.set_list_selection(last);
                SidebarKeyResult::Consumed
            }
            KeyCode::Enter => self
                .selected_row_action()
                .and_then(|action| self.run_row_action(action))
                .map_or(SidebarKeyResult::Consumed, SidebarKeyResult::Request),
            KeyCode::Backspace | KeyCode::ArrowLeft => {
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
            _ => SidebarKeyResult::Consumed,
        }
    }

    fn on_reader_key(&mut self, code: KeyCode) -> SidebarKeyResult {
        match code {
            KeyCode::ArrowUp => {
                self.scroll_reader_by(-1);
                SidebarKeyResult::Consumed
            }
            KeyCode::ArrowDown => {
                self.scroll_reader_by(1);
                SidebarKeyResult::Consumed
            }
            KeyCode::PageUp => {
                self.scroll_reader_by(-(self.reader_body_slots() as isize));
                SidebarKeyResult::Consumed
            }
            KeyCode::PageDown => {
                self.scroll_reader_by(self.reader_body_slots() as isize);
                SidebarKeyResult::Consumed
            }
            KeyCode::Home => {
                self.set_reader_scroll(0);
                SidebarKeyResult::Consumed
            }
            KeyCode::End => {
                let max = reader_scroll_max(self.reader_lines.len(), self.reader_body_slots());
                self.set_reader_scroll(max);
                SidebarKeyResult::Consumed
            }
            KeyCode::Backspace | KeyCode::ArrowLeft => self
                .run_row_action(RowAction::CloseReader)
                .map_or(SidebarKeyResult::Consumed, SidebarKeyResult::Request),
            KeyCode::KeyE => self
                .reader_action(|action| matches!(action, RowAction::EditFile(_)))
                .and_then(|action| self.run_row_action(action))
                .map_or(SidebarKeyResult::Consumed, SidebarKeyResult::Request),
            KeyCode::KeyO => self
                .reader_action(|action| matches!(action, RowAction::OpenWithSystem(_)))
                .and_then(|action| self.run_row_action(action))
                .map_or(SidebarKeyResult::Consumed, SidebarKeyResult::Request),
            _ => SidebarKeyResult::Consumed,
        }
    }

    pub fn on_scroll(&mut self, delta: i32) {
        if !self.visible || delta == 0 {
            return;
        }

        if self.sidebar_view == SidebarView::Reader {
            self.scroll_reader_by(-(delta as isize));
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

    fn scroll_reader_by(&mut self, delta: isize) {
        let next = if delta.is_negative() {
            self.reader_scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.reader_scroll.saturating_add(delta as usize)
        };
        self.set_reader_scroll(next);
    }

    fn set_reader_scroll(&mut self, offset: usize) {
        let clamped =
            clamp_reader_scroll(offset, self.reader_lines.len(), self.reader_body_slots());
        if clamped != self.reader_scroll {
            self.reader_scroll = clamped;
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

    fn reader_action(&self, predicate: impl Fn(&RowAction) -> bool) -> Option<RowAction> {
        self.row_actions
            .iter()
            .flatten()
            .find(|action| predicate(action))
            .cloned()
    }

    pub fn preview_pinned(&mut self, abs_path: &Path) {
        let display_path = self
            .watcher_root
            .as_deref()
            .or_else(|| self.info.as_ref().map(|info| info.cwd.as_path()))
            .and_then(|root| abs_path.strip_prefix(root).ok())
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| abs_path.to_path_buf());

        self.pinned = true;
        self.sidebar_view = SidebarView::Reader;
        self.reader_scroll = 0;
        self.reader_notice = None;
        self.preview
            .set_target_abs(abs_path.to_path_buf(), display_path);
        self.refresh_reader_document();
        self.rebuild();
    }

    pub fn current_ratio(&self) -> f64 {
        if self.sidebar_view == SidebarView::Reader {
            crate::TOYTERM_CONFIG.preview_ratio
        } else {
            crate::TOYTERM_CONFIG.sidebar_ratio
        }
    }

    pub fn show_missing_editor(&mut self, command: &str) {
        self.reader_notice = Some(format!(
            " （編集コマンドが見つかりません: {command}。config.toml の editor で設定できます）"
        ));
        self.refresh_reader_document();
        self.rebuild();
    }

    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.view.set_viewport(viewport);
        if self.sidebar_view == SidebarView::Reader {
            self.refresh_reader_document();
        }
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
        if self.preview.poll() {
            self.refresh_reader_document();
            changed = true;
        }
        if self.poll_browse_dir() {
            changed = true;
        }
        // cd やフォーカス切替で作業フォルダが変わったら、5秒を待たずに反映する。
        let cwd_changed = self
            .info
            .as_ref()
            .map_or(true, |info| info.cwd.as_path() != cwd);
        if cwd_changed || self.last_refresh.elapsed() >= REFRESH_INTERVAL {
            self.refresh(cwd);
        } else if changed {
            self.rebuild();
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
        self.watcher = None;
        self.watcher_pending = None;
        self.watcher_root = None;
        self.watch_failed = false;
        self.changes.clear();
        self.preview.clear();
        self.pinned = false;
        self.mode = SidebarMode::Files;
        self.sidebar_view = SidebarView::List;
        self.reader_scroll = 0;
        self.reader_lines.clear();
        self.reader_notice = None;
        self.browse_entries.clear();
        self.browse_pending = None;
        self.browse_error = None;
        self.browse_scroll = 0;
        self.row_actions.clear();
        self.browse_selected = 0;
    }

    fn refresh(&mut self, cwd: &Path) {
        self.drain_watcher(cwd);
        self.info = Some(workspace::collect(cwd));
        if self.browse_dir.as_os_str().is_empty() {
            self.browse_dir = cwd.to_path_buf();
        }
        if self.pinned
            && self
                .preview
                .target_abs()
                .is_some_and(|path| !path.starts_with(cwd))
        {
            self.preview.refresh_current();
            self.refresh_reader_document();
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
            self.preview.clear();
            self.pinned = false;
            self.watcher = None;
            self.watch_failed = false;
            self.watcher_root = Some(cwd.to_path_buf());
            self.sidebar_view = SidebarView::List;
            self.reader_scroll = 0;
            self.reader_lines.clear();
            self.reader_notice = None;
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
                    if self.pinned {
                        let changed_abs = root.join(&change.path);
                        if self.preview.target_abs() == Some(changed_abs.as_path()) {
                            self.preview.notify_changed(root, change.path);
                        }
                    } else {
                        self.preview.notify_changed(root, change.path);
                    }
                }
                ChangeKind::Deleted => {
                    if self.pinned {
                        let changed_abs = root.join(&change.path);
                        if self.preview.target_abs() == Some(changed_abs.as_path()) {
                            self.preview.mark_deleted(change.path);
                        }
                    } else if self.preview.target() == Some(change.path.as_path()) {
                        if let Some(next) = self
                            .changes
                            .iter()
                            .skip(1)
                            .find(|stored| stored.kind != ChangeKind::Deleted)
                        {
                            self.preview.set_target(root, next.path.clone());
                        } else {
                            self.preview.mark_deleted(change.path);
                        }
                    }
                }
            }
        }
        self.changes.truncate(100);
        if self.sidebar_view == SidebarView::Reader {
            self.refresh_reader_document();
        }
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
                "リモートの内容を見るには（Phase 8b で対応予定）:",
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

            if self.sidebar_view == SidebarView::Reader {
                self.push_reader(&mut lines, &mut row_actions, cols, rows);
            } else {
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
                push_separator(&mut lines, &mut row_actions, cols);
                self.push_preview(&mut lines, &mut row_actions, cols, content_rows);
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
            view.bg_color = Color::Black;
            view.lines = lines;
            view.images = Vec::new();
            view.cursor = None;
            view.selection_range = None;
        });
    }

    fn apply_selection_style(&self, lines: &mut [Line], row_actions: &[Option<RowAction>]) {
        if self.sidebar_view != SidebarView::List {
            return;
        }
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
                cell.attr.fg = Color::Black;
                cell.attr.bg = Color::White;
            }
        }
    }

    fn push_key_hint(
        &self,
        lines: &mut Vec<Line>,
        row_actions: &mut Vec<Option<RowAction>>,
        cols: usize,
    ) {
        let hint = if self.sidebar_view == SidebarView::Reader {
            " ↑↓:スクロール  e:編集  BS:戻る  Esc:端末"
        } else {
            " ↑↓:選択  Enter:開く  BS:上へ  Esc:端末"
        };
        push_line(lines, row_actions, cols, hint, Color::BrightBlack, None);
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
            let marker = if self.preview.target() == Some(change.path.as_path()) {
                " ▶ "
            } else {
                "   "
            };
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
                Some(RowAction::PreviewFile(
                    self.watcher_root
                        .as_deref()
                        .unwrap_or_else(|| Path::new(""))
                        .join(&change.path),
                )),
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

    fn push_preview(
        &self,
        lines: &mut Vec<Line>,
        row_actions: &mut Vec<Option<RowAction>>,
        cols: usize,
        rows: usize,
    ) {
        if let Some(target) = self.preview.target() {
            let suffix = if self.pinned {
                " 📌 (クリックで追従に戻る)"
            } else {
                " follow"
            };
            let available = cols.saturating_sub(3 + display_width(suffix));
            let path = abbreviate_start(&target.display().to_string(), available);
            let action = self.pinned.then_some(RowAction::Unpin);
            push_line(
                lines,
                row_actions,
                cols,
                &format!(" ▶ {path}{suffix}"),
                Color::BrightWhite,
                action,
            );
            if let Some(abs_path) = self.preview.target_abs() {
                let env_editor = std::env::var("EDITOR").ok();
                let editor = resolve_editor(&crate::TOYTERM_CONFIG.editor, env_editor.as_deref());
                // 起動する実体を見せるため、引数ではなく先頭コマンドだけ表示する。
                push_line(
                    lines,
                    row_actions,
                    cols,
                    &format!("   [クリックで編集: {}]", editor[0]),
                    Color::Cyan,
                    Some(RowAction::EditFile(abs_path.to_path_buf())),
                );
            }
        }

        let available = rows.saturating_sub(lines.len());
        if available == 0 {
            return;
        }

        match self.preview.lines() {
            PreviewLines::Text(text_lines) => {
                let start = text_lines.len().saturating_sub(available);
                for line in &text_lines[start..] {
                    push_line(lines, row_actions, cols, line, Color::White, None);
                }
            }
            PreviewLines::Message(message) => {
                push_line(lines, row_actions, cols, message, Color::BrightBlack, None);
            }
        }
    }

    fn push_reader(
        &self,
        lines: &mut Vec<Line>,
        row_actions: &mut Vec<Option<RowAction>>,
        cols: usize,
        rows: usize,
    ) {
        push_line(
            lines,
            row_actions,
            cols,
            " ← 戻る",
            Color::BrightWhite,
            Some(RowAction::CloseReader),
        );

        if let Some(target) = self.preview.target() {
            let suffix = if self.pinned { " 📌" } else { "" };
            let available = cols.saturating_sub(1 + display_width(suffix));
            let path = abbreviate_start(&target.display().to_string(), available);
            push_line(
                lines,
                row_actions,
                cols,
                &format!(" {path}{suffix}"),
                Color::BrightWhite,
                None,
            );
        }

        if let Some(abs_path) = self.preview.target_abs() {
            let env_editor = std::env::var("EDITOR").ok();
            let editor = resolve_editor(&crate::TOYTERM_CONFIG.editor, env_editor.as_deref());
            push_line(
                lines,
                row_actions,
                cols,
                &format!(" [編集: {}]", editor[0]),
                Color::Cyan,
                Some(RowAction::EditFile(abs_path.to_path_buf())),
            );
            push_line(
                lines,
                row_actions,
                cols,
                " [OSの既定アプリで開く]",
                Color::Cyan,
                Some(RowAction::OpenWithSystem(abs_path.to_path_buf())),
            );
        }

        if let Some(notice) = &self.reader_notice {
            push_line(lines, row_actions, cols, notice, Color::BrightBlack, None);
        }

        push_separator(lines, row_actions, cols);

        let available = rows.saturating_sub(lines.len());
        if available == 0 {
            return;
        }

        let scroll = clamp_reader_scroll(self.reader_scroll, self.reader_lines.len(), available);
        for line in self.reader_lines.iter().skip(scroll).take(available) {
            push_styled_line(lines, row_actions, cols, line, None);
        }
    }

    fn refresh_reader_document(&mut self) {
        self.reader_lines = match self.preview.lines() {
            PreviewLines::Text(text_lines) => {
                let joined = text_lines.join("\n");
                if self
                    .preview
                    .target()
                    .and_then(Path::extension)
                    .and_then(|ext| ext.to_str())
                    .is_some_and(is_markdown_extension)
                {
                    render_markdown(&joined)
                        .into_iter()
                        .flat_map(|line| wrap_styled_line(&line, self.reader_wrap_cols()))
                        .collect()
                } else {
                    text_lines
                        .iter()
                        .flat_map(|line| wrap_line(line, self.reader_wrap_cols()))
                        .map(|line| styled_plain(line, Color::White))
                        .collect()
                }
            }
            PreviewLines::Message(message) => {
                vec![styled_plain(message.to_owned(), Color::BrightBlack)]
            }
        };

        self.reader_scroll = clamp_reader_scroll(
            self.reader_scroll,
            self.reader_lines.len(),
            self.reader_body_slots(),
        );
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
            push_line(
                lines,
                row_actions,
                cols,
                "   ../",
                Color::Blue,
                Some(RowAction::BrowseParent),
            );
        }

        for (name, is_dir) in self
            .browse_entries
            .iter()
            .skip(scroll)
            .take(visible_entries)
        {
            let suffix = if *is_dir { "/" } else { "" };
            let label = abbreviate_start(&format!("   {name}{suffix}"), cols);
            let path = self.browse_dir.join(name);
            let action = if *is_dir {
                RowAction::BrowseDir(path)
            } else {
                RowAction::PreviewFile(path)
            };
            push_line(
                lines,
                row_actions,
                cols,
                &label,
                if *is_dir { Color::Blue } else { Color::White },
                Some(action),
            );
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

    fn unpin(&mut self) {
        self.pinned = false;
        if let Some(root) = self.watcher_root.as_deref() {
            if let Some(next) = self
                .changes
                .iter()
                .find(|stored| stored.kind != ChangeKind::Deleted)
            {
                self.preview.set_target(root, next.path.clone());
            } else {
                self.preview.clear();
            }
        } else {
            self.preview.clear();
        }
        self.rebuild();
    }

    fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            SidebarMode::Changes => SidebarMode::Files,
            SidebarMode::Files => SidebarMode::Changes,
        };
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

    fn reader_wrap_cols(&self) -> usize {
        let cols = (self.view.viewport().w / self.view.cell_size().w).max(1) as usize;
        cols.saturating_sub(1).max(1)
    }

    fn reader_body_slots(&self) -> usize {
        let rows = (self.view.viewport().h / self.view.cell_size().h).max(1) as usize;
        let header_rows = 5
            + usize::from(self.preview.target_abs().is_some())
            + usize::from(self.reader_notice.is_some())
            + usize::from(self.focused);
        rows.saturating_sub(header_rows)
    }
}

pub enum SidebarRequest {
    EditFile(PathBuf),
    OpenWithSystem(PathBuf),
    RefreshLayout,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarView {
    List,
    Reader,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RowAction {
    ToggleMode,
    BrowseParent,
    BrowseDir(PathBuf),
    PreviewFile(PathBuf),
    Unpin,
    EditFile(PathBuf),
    OpenWithSystem(PathBuf),
    CloseReader,
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

pub(crate) fn clamp_reader_scroll(offset: usize, len: usize, visible: usize) -> usize {
    offset.min(reader_scroll_max(len, visible))
}

fn reader_scroll_max(len: usize, visible: usize) -> usize {
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

fn push_styled_line(
    lines: &mut Vec<Line>,
    row_actions: &mut Vec<Option<RowAction>>,
    cols: usize,
    line: &StyledLine,
    action: Option<RowAction>,
) {
    lines.push(Line::from_cells(cells_for_styled_line(line, cols), false));
    row_actions.push(action);
}

fn cells_for_line(text: &str, cols: usize, fg: Color) -> Vec<Cell> {
    cells_for_segments(&[(text, fg)], cols)
}

fn push_segments(
    lines: &mut Vec<Line>,
    row_actions: &mut Vec<Option<RowAction>>,
    cols: usize,
    segments: &[(&str, Color)],
    action: Option<RowAction>,
) {
    lines.push(Line::from_cells(cells_for_segments(segments, cols), false));
    row_actions.push(action);
}

fn cells_for_segments(segments: &[(&str, Color)], cols: usize) -> Vec<Cell> {
    let styled: Vec<StyledSegment> = segments
        .iter()
        .map(|(text, fg)| StyledSegment {
            text: (*text).to_owned(),
            style: TextStyle {
                fg: *fg,
                bold: false,
            },
        })
        .collect();
    cells_for_styled_line(&styled, cols)
}

fn cells_for_styled_line(segments: &StyledLine, cols: usize) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut used = 0usize;

    for segment in segments {
        for ch in segment.text.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width == 0 || used + width > cols {
                return cells;
            }

            let attr = GraphicAttribute {
                fg: segment.style.fg,
                bold: i8::from(segment.style.bold),
                ..GraphicAttribute::default()
            };
            cells.push(Cell::head(ch, width as u16, attr));
            for i in 1..width {
                cells.push(Cell::spacer(i as u16));
            }
            used += width;
        }
    }

    cells
}

pub(crate) type StyledLine = Vec<StyledSegment>;

#[derive(Clone, Debug)]
pub(crate) struct StyledSegment {
    text: String,
    style: TextStyle,
}

#[derive(Clone, Copy, Debug)]
struct TextStyle {
    fg: Color,
    bold: bool,
}

fn styled_plain(text: String, fg: Color) -> StyledLine {
    vec![StyledSegment {
        text,
        style: TextStyle { fg, bold: false },
    }]
}

pub(crate) fn wrap_line(line: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    if line.is_empty() {
        return vec![String::new()];
    }

    let mut out = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for ch in line.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width == 0 {
            continue;
        }
        if used > 0 && used + width > cols {
            out.push(current);
            current = String::new();
            used = 0;
        }
        current.push(ch);
        used += width;
    }
    out.push(current);
    out
}

fn wrap_styled_line(line: &StyledLine, cols: usize) -> Vec<StyledLine> {
    let cols = cols.max(1);
    if line.is_empty() || styled_width(line) == 0 {
        return vec![Vec::new()];
    }

    let mut out = Vec::new();
    let mut current = Vec::new();
    let mut used = 0usize;
    for segment in line {
        for ch in segment.text.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width == 0 {
                continue;
            }
            if used > 0 && used + width > cols {
                out.push(current);
                current = Vec::new();
                used = 0;
            }
            push_char_segment(&mut current, ch, segment.style);
            used += width;
        }
    }
    out.push(current);
    out
}

fn push_char_segment(line: &mut StyledLine, ch: char, style: TextStyle) {
    line.push(StyledSegment {
        text: ch.to_string(),
        style,
    });
}

fn styled_width(line: &StyledLine) -> usize {
    line.iter()
        .map(|segment| display_width(&segment.text))
        .sum()
}

pub(crate) fn render_markdown(src: &str) -> Vec<StyledLine> {
    let mut renderer = MarkdownRenderer::new();
    let parser = Parser::new_ext(src, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    for event in parser {
        renderer.handle(event);
    }
    renderer.finish()
}

struct MarkdownRenderer {
    lines: Vec<StyledLine>,
    current: StyledLine,
    style: TextStyle,
    list_depth: usize,
    in_code_block: bool,
    image_alt: Option<String>,
}

impl MarkdownRenderer {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            current: Vec::new(),
            style: TextStyle {
                fg: Color::White,
                bold: false,
            },
            list_depth: 0,
            in_code_block: false,
            image_alt: None,
        }
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text, false),
            Event::Code(text) => self.text(&text, true),
            Event::SoftBreak | Event::HardBreak => self.flush_current(),
            Event::Rule => self
                .lines
                .push(styled_plain(" ────".to_owned(), Color::BrightBlack)),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { level, .. } => {
                self.flush_current();
                self.style = TextStyle {
                    fg: Color::Cyan,
                    bold: true,
                };
                self.push_text(&format!("{} ", heading_marks(level)));
            }
            Tag::List(_) => self.list_depth += 1,
            Tag::Item => {
                self.flush_current();
                self.push_text(&format!(
                    "{}• ",
                    "  ".repeat(self.list_depth.saturating_sub(1))
                ));
            }
            Tag::CodeBlock(_) => {
                self.flush_current();
                self.in_code_block = true;
                self.style = TextStyle {
                    fg: Color::Green,
                    bold: false,
                };
            }
            Tag::BlockQuote(_) => {
                self.flush_current();
                self.current.push(StyledSegment {
                    text: "│ ".to_owned(),
                    style: TextStyle {
                        fg: Color::BrightBlack,
                        bold: false,
                    },
                });
            }
            Tag::Strong => self.style.bold = true,
            Tag::Image { .. } => self.image_alt = Some(String::new()),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.flush_current();
                self.style = TextStyle {
                    fg: Color::White,
                    bold: false,
                };
            }
            TagEnd::List(_) => self.list_depth = self.list_depth.saturating_sub(1),
            TagEnd::Item => self.flush_current(),
            TagEnd::CodeBlock => {
                self.flush_current();
                self.in_code_block = false;
                self.style = TextStyle {
                    fg: Color::White,
                    bold: false,
                };
            }
            TagEnd::BlockQuote(_) => {
                self.flush_current();
            }
            TagEnd::Strong => self.style.bold = false,
            TagEnd::Image => {
                if let Some(alt) = self.image_alt.take() {
                    self.push_text(&format!("[画像: {alt}]"));
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str, inline_code: bool) {
        if let Some(alt) = &mut self.image_alt {
            alt.push_str(text);
            return;
        }

        let previous = self.style;
        if inline_code {
            self.style = TextStyle {
                fg: Color::Yellow,
                bold: previous.bold,
            };
        }

        if self.in_code_block {
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    self.flush_current();
                }
                self.push_text(line);
            }
        } else {
            self.push_text(text);
        }

        if inline_code {
            self.style = previous;
        }
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.current.push(StyledSegment {
            text: text.to_owned(),
            style: self.style,
        });
    }

    fn flush_current(&mut self) {
        if self.current.is_empty() {
            return;
        }
        self.lines.push(std::mem::take(&mut self.current));
    }

    fn finish(mut self) -> Vec<StyledLine> {
        self.flush_current();
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
        self.lines
    }
}

fn heading_marks(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "#",
        HeadingLevel::H2 => "##",
        HeadingLevel::H3 => "###",
        HeadingLevel::H4 => "####",
        HeadingLevel::H5 => "#####",
        HeadingLevel::H6 => "######",
    }
}

fn is_markdown_extension(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
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
        clamp_browse_scroll, clamp_reader_scroll, click_row, keep_selection_visible,
        render_markdown, sidebar_action_at, sort_entries, wrap_line, RowAction, StyledLine,
    };
    use crate::view::Viewport;
    use std::path::PathBuf;
    use winit::dpi::PhysicalPosition;

    fn line_text(line: &StyledLine) -> String {
        line.iter().map(|segment| segment.text.as_str()).collect()
    }

    fn rendered_text(src: &str) -> Vec<String> {
        render_markdown(src).iter().map(line_text).collect()
    }

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
            Some(RowAction::Unpin),
        ];

        assert_eq!(sidebar_action_at(&actions, 0), None);
        assert_eq!(
            sidebar_action_at(&actions, 1),
            Some(&RowAction::PreviewFile(file))
        );
        assert_eq!(sidebar_action_at(&actions, 2), Some(&RowAction::Unpin));
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

    #[test]
    fn clamp_reader_scroll_stays_within_visible_range() {
        assert_eq!(clamp_reader_scroll(0, 20, 5), 0);
        assert_eq!(clamp_reader_scroll(12, 20, 5), 12);
        assert_eq!(clamp_reader_scroll(99, 20, 5), 15);
        assert_eq!(clamp_reader_scroll(3, 4, 10), 0);
    }

    #[test]
    fn wrap_line_wraps_ascii() {
        assert_eq!(wrap_line("abcdef", 3), vec!["abc", "def"]);
    }

    #[test]
    fn wrap_line_wraps_mixed_width_text() {
        assert_eq!(wrap_line("ab日本cd", 4), vec!["ab日", "本cd"]);
    }

    #[test]
    fn wrap_line_keeps_exact_boundary() {
        assert_eq!(wrap_line("abcd", 4), vec!["abcd"]);
    }

    #[test]
    fn wrap_line_preserves_empty_line() {
        assert_eq!(wrap_line("", 4), vec![""]);
    }

    #[test]
    fn render_markdown_formats_representative_blocks() {
        let lines = rendered_text(
            "# Title\n\nnormal `code`\n\n- item\n\n> quote\n\n```rust\nlet x = 1;\n```\n\n---\n\n![alt](image.png)",
        );

        assert!(lines.contains(&"# Title".to_owned()));
        assert!(lines.contains(&"normal code".to_owned()));
        assert!(lines.contains(&"• item".to_owned()));
        assert!(lines.contains(&"│ quote".to_owned()));
        assert!(lines.contains(&"let x = 1;".to_owned()));
        assert!(lines.contains(&" ────".to_owned()));
        assert!(lines.contains(&"[画像: alt]".to_owned()));
    }
}
