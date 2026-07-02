use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthChar;
use winit::dpi::PhysicalPosition;

use crate::preview::{FilePreview, PreviewLines};
use crate::terminal::{Cell, Color, GraphicAttribute, Line};
use crate::view::{TerminalView, Viewport};
use crate::watcher::{merge_kind, ChangeKind, FileChange, WorkspaceWatcher};
use crate::workspace::{self, WorkspaceInfo};
use crate::Display;

const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

pub struct Sidebar {
    view: TerminalView,
    visible: bool,
    info: Option<WorkspaceInfo>,
    last_refresh: Instant,
    changes: Vec<FileChange>,
    preview: FilePreview,
    pinned: bool,
    row_actions: Vec<Option<RowAction>>,
    watcher: Option<WorkspaceWatcher>,
    /// バックグラウンドで生成中の watcher。生成（ディレクトリ登録）は
    /// フォルダ規模によってはブロックするので、UI スレッドでは行わない。
    watcher_pending: Option<(PathBuf, Receiver<Result<WorkspaceWatcher, notify::Error>>)>,
    watcher_root: Option<PathBuf>,
    watch_failed: bool,
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
            row_actions: Vec::new(),
            watcher: None,
            watcher_pending: None,
            watcher_root: None,
            watch_failed: false,
        }
    }

    pub fn toggle(&mut self, cwd: &Path) {
        self.visible = !self.visible;
        if self.visible {
            self.refresh(cwd);
        } else {
            // 非表示中の監視コストをゼロにする（生成待ちも破棄。生成スレッドの
            // 結果は誰も受け取らないだけで害はない）。
            self.watcher = None;
            self.watcher_pending = None;
            self.watcher_root = None;
            self.watch_failed = false;
            self.preview.clear();
            self.pinned = false;
            self.row_actions.clear();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn contains(&self, p: PhysicalPosition<f64>) -> bool {
        self.visible && self.view.viewport().contains(p)
    }

    pub fn on_click(&mut self, p: PhysicalPosition<f64>) {
        if !self.contains(p) {
            return;
        }
        let Some(action) = sidebar_action_at(
            &self.row_actions,
            click_row(self.view.viewport(), self.view.cell_size().h, p),
        )
        .cloned() else {
            return;
        };

        match action {
            RowAction::PreviewFile(path) => self.preview_pinned(&path),
            RowAction::Unpin => self.unpin(),
        }
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
        self.preview
            .set_target_abs(abs_path.to_path_buf(), display_path);
        self.rebuild();
    }

    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.view.set_viewport(viewport);
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

    pub fn refresh_if_stale(&mut self, cwd: &Path) {
        if !self.visible {
            return;
        }
        let mut changed = self.drain_watcher(cwd);
        if self.preview.poll() {
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

    fn refresh(&mut self, cwd: &Path) {
        self.drain_watcher(cwd);
        self.info = Some(workspace::collect(cwd));
        if self.pinned
            && self
                .preview
                .target_abs()
                .is_some_and(|path| !path.starts_with(cwd))
        {
            self.preview.refresh_current();
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
    }

    fn rebuild(&mut self) {
        if !self.visible {
            return;
        }

        let cols = (self.view.viewport().w / self.view.cell_size().w).max(1) as usize;
        let rows = (self.view.viewport().h / self.view.cell_size().h).max(1) as usize;
        let mut lines = Vec::new();
        let mut row_actions = Vec::new();

        if let Some(info) = &self.info {
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

            push_separator(&mut lines, &mut row_actions, cols);
            self.push_changed_files(&mut lines, &mut row_actions, cols);
            push_separator(&mut lines, &mut row_actions, cols);
            self.push_preview(&mut lines, &mut row_actions, cols, rows);
        }

        lines.truncate(rows);
        row_actions.truncate(rows);
        self.row_actions = row_actions;

        self.view.update_contents(|view| {
            view.bg_color = Color::Black;
            view.lines = lines;
            view.images = Vec::new();
            view.cursor = None;
            view.selection_range = None;
        });
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RowAction {
    PreviewFile(PathBuf),
    Unpin,
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
    let mut cells = Vec::new();
    let mut used = 0usize;

    for (text, fg) in segments {
        for ch in text.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width == 0 || used + width > cols {
                return cells;
            }

            let attr = GraphicAttribute {
                fg: *fg,
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
    use super::{click_row, sidebar_action_at, RowAction};
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
}
