use std::path::{Path, PathBuf};

use unicode_width::UnicodeWidthChar;
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
};

use crate::terminal::{Cell, Color, GraphicAttribute, Line};
use crate::view::{TerminalView, Viewport};
use crate::Display;

// 色・アイコンはサイドバーのファイル一覧と共通（見た目を揃える）。
use crate::file_style::{icon_and_color, ACCENT, DIM, DIR_FG, SEL_BG, SEL_FG};

#[derive(Debug, PartialEq, Eq)]
pub enum LauncherOutcome {
    /// このディレクトリでターミナルを開く。
    OpenIn {
        dir: PathBuf,
        command: Option<Vec<String>>,
    },
    /// 何もせず閉じる。
    Cancelled,
    /// まだ操作中。
    None,
}

pub struct Launcher {
    view: TerminalView,
    state: LauncherState,
}

impl Launcher {
    pub fn new(display: Display, viewport: Viewport, recent: &[PathBuf]) -> Self {
        let state = LauncherState::new(recent.to_vec());
        let mut launcher = Self {
            view: TerminalView::with_viewport(
                display,
                viewport,
                crate::TOYTERM_CONFIG.font_size,
                None,
            ),
            state,
        };
        launcher.rebuild();
        launcher
    }

    pub fn set_viewport(&mut self, vp: Viewport) {
        self.view.set_viewport(vp);
        self.rebuild();
    }

    pub fn draw(&mut self, surface: &mut glium::Frame) {
        self.rebuild();
        self.view.draw(surface);
    }

    pub fn handle_key(&mut self, event: &KeyEvent, mods: ModifiersState) -> LauncherOutcome {
        let outcome = self.state.handle_key(event, mods);
        self.rebuild();
        outcome
    }

    pub fn needs_redraw(&self) -> bool {
        self.view.needs_redraw()
    }

    fn rebuild(&mut self) {
        let cols = (self.view.viewport().w / self.view.cell_size().w).max(1) as usize;
        let rows = (self.view.viewport().h / self.view.cell_size().h).max(1) as usize;
        let lines = self.state.render(cols, rows);
        self.view.update_contents(|view| {
            // ターミナルと同じ透過設定（セルの Color::Background クアッドで半透明を出す）。
            // ポップアップ部分だけ各セルに不透明色を敷いて透過を消す。
            view.bg_color = Color::Background;
            view.skip_default_bg = false;
            view.lines = lines;
            view.images = Vec::new();
            view.cursor = None;
            view.selection_range = None;
            view.scroll_bar = None;
            view.view_focused = true;
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    name: String,
    is_dir: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Browse,
    Recent,
    Agent,
}

#[derive(Clone, Debug)]
struct LauncherState {
    /// いま中身を見せているディレクトリ。
    dir: PathBuf,
    /// dir の中身（親があれば先頭に ".."）。
    entries: Vec<Entry>,
    selected: usize,
    scroll: usize,
    show_hidden: bool,
    recent: Vec<PathBuf>,
    recent_selected: usize,
    mode: Mode,
    filter: Option<String>,
    chosen_dir: Option<PathBuf>,
    agent_selected: usize,
}

impl LauncherState {
    fn new(recent: Vec<PathBuf>) -> Self {
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut state = Self {
            entries: Vec::new(),
            dir,
            selected: 0,
            scroll: 0,
            show_hidden: false,
            recent,
            recent_selected: 0,
            mode: Mode::Browse,
            filter: None,
            chosen_dir: None,
            agent_selected: 0,
        };
        state.reload();
        state
    }

    #[cfg(test)]
    fn with_dir(recent: Vec<PathBuf>, dir: PathBuf) -> Self {
        let mut state = Self {
            entries: Vec::new(),
            dir,
            selected: 0,
            scroll: 0,
            show_hidden: false,
            recent,
            recent_selected: 0,
            mode: Mode::Browse,
            filter: None,
            chosen_dir: None,
            agent_selected: 0,
        };
        state.reload();
        state
    }

    fn handle_key(&mut self, event: &KeyEvent, _mods: ModifiersState) -> LauncherOutcome {
        if event.state != ElementState::Pressed {
            return LauncherOutcome::None;
        }
        let code = match event.physical_key {
            PhysicalKey::Code(code) => code,
            PhysicalKey::Unidentified(_) => return LauncherOutcome::None,
        };
        self.handle_key_parts(code, event.text.as_deref())
    }

    fn handle_key_parts(&mut self, code: KeyCode, text: Option<&str>) -> LauncherOutcome {
        match self.mode {
            Mode::Browse => self.handle_browse_key(code, text),
            Mode::Recent => self.handle_recent_key(code, text),
            Mode::Agent => self.handle_agent_key(code, text),
        }
    }

    fn handle_browse_key(&mut self, code: KeyCode, text: Option<&str>) -> LauncherOutcome {
        if self.filter.is_some() {
            return self.handle_filter_key(code, text);
        }
        match code {
            KeyCode::Escape => return LauncherOutcome::Cancelled,
            KeyCode::Enter => self.choose_target(),
            KeyCode::ArrowDown => self.move_sel(1),
            KeyCode::ArrowUp => self.move_sel(-1),
            KeyCode::ArrowRight => self.descend(),
            KeyCode::ArrowLeft => self.ascend(),
            _ => match text {
                Some("j") => self.move_sel(1),
                Some("k") => self.move_sel(-1),
                Some("l") => self.descend(),
                Some("h") => self.ascend(),
                Some("/") => self.start_filter(),
                Some(".") => {
                    self.show_hidden = !self.show_hidden;
                    self.reload();
                }
                Some("r") if !self.recent.is_empty() => {
                    self.mode = Mode::Recent;
                    self.recent_selected = 0;
                }
                _ => {}
            },
        }
        LauncherOutcome::None
    }

    fn handle_filter_key(&mut self, code: KeyCode, text: Option<&str>) -> LauncherOutcome {
        match code {
            KeyCode::Escape => {
                self.filter = None;
                return LauncherOutcome::None;
            }
            KeyCode::Enter => self.choose_target(),
            KeyCode::ArrowDown => self.move_sel(1),
            KeyCode::ArrowUp => self.move_sel(-1),
            KeyCode::ArrowRight => self.descend(),
            KeyCode::Backspace => self.backspace_filter(),
            // 絞り込み中は文字は全部クエリへ（"l" 等も名前の一部として打てる）。
            // フォルダへ潜るのは → のみ。
            _ => match text {
                Some(input) => self.push_filter_text(input),
                None => {}
            },
        }
        LauncherOutcome::None
    }

    fn handle_recent_key(&mut self, code: KeyCode, text: Option<&str>) -> LauncherOutcome {
        match code {
            KeyCode::Escape => self.mode = Mode::Browse,
            KeyCode::Enter => {
                if let Some(path) = self.recent.get(self.recent_selected) {
                    if let Some(dir) = resolve_existing_dir(path) {
                        self.enter_agent_mode(dir);
                    }
                }
            }
            KeyCode::ArrowDown => self.move_recent(1),
            KeyCode::ArrowUp => self.move_recent(-1),
            _ => match text {
                Some("j") => self.move_recent(1),
                Some("k") => self.move_recent(-1),
                Some("r") => self.mode = Mode::Browse,
                _ => {}
            },
        }
        LauncherOutcome::None
    }

    fn handle_agent_key(&mut self, code: KeyCode, text: Option<&str>) -> LauncherOutcome {
        match code {
            KeyCode::Escape => {
                self.mode = Mode::Browse;
                self.chosen_dir = None;
                self.agent_selected = 0;
            }
            KeyCode::Enter => {
                let Some(dir) = self.chosen_dir.clone() else {
                    self.mode = Mode::Browse;
                    return LauncherOutcome::None;
                };
                let command = if self.agent_selected == 0 {
                    None
                } else {
                    crate::TOYTERM_CONFIG
                        .launcher_agents
                        .get(self.agent_selected - 1)
                        .map(|agent| agent.command.clone())
                };
                return LauncherOutcome::OpenIn { dir, command };
            }
            KeyCode::ArrowDown => self.move_agent(1),
            KeyCode::ArrowUp => self.move_agent(-1),
            _ => match text {
                Some("j") => self.move_agent(1),
                Some("k") => self.move_agent(-1),
                _ => {}
            },
        }
        LauncherOutcome::None
    }

    fn start_filter(&mut self) {
        self.filter = Some(String::new());
        self.select_first_filter_match();
    }

    fn backspace_filter(&mut self) {
        let Some(query) = self.filter.as_mut() else {
            return;
        };
        if query.is_empty() {
            self.filter = None;
            return;
        }
        query.pop();
        self.select_first_filter_match();
    }

    fn push_filter_text(&mut self, input: &str) {
        if !input.chars().all(|ch| !ch.is_control()) {
            return;
        }
        if let Some(query) = self.filter.as_mut() {
            query.push_str(input);
            self.select_first_filter_match();
        }
    }

    fn move_sel(&mut self, delta: isize) {
        let visible = self.visible_entry_indices();
        if visible.is_empty() {
            return;
        }
        let pos = visible
            .iter()
            .position(|idx| *idx == self.selected)
            .unwrap_or(0) as isize;
        let last = (visible.len() - 1) as isize;
        self.selected = visible[(pos + delta).clamp(0, last) as usize];
    }

    fn move_recent(&mut self, delta: isize) {
        if self.recent.is_empty() {
            return;
        }
        let last = (self.recent.len() - 1) as isize;
        self.recent_selected = (self.recent_selected as isize + delta).clamp(0, last) as usize;
    }

    fn move_agent(&mut self, delta: isize) {
        let len = crate::TOYTERM_CONFIG.launcher_agents.len() + 1;
        let last = (len - 1) as isize;
        self.agent_selected = (self.agent_selected as isize + delta).clamp(0, last) as usize;
    }

    /// 選択中のフォルダの中へ入る（l / →）。".." なら親へ。ファイルは何もしない。
    fn descend(&mut self) {
        let Some(entry) = self.current_entry().cloned() else {
            return;
        };
        if entry.name == ".." {
            self.ascend();
        } else if entry.is_dir {
            self.filter = None;
            self.dir = self.dir.join(&entry.name);
            self.reload();
        }
    }

    /// 親フォルダへ戻る（h / ←）。戻ったら元いたフォルダを選択位置にする。
    fn ascend(&mut self) {
        let Some(parent) = self.dir.parent().map(Path::to_path_buf) else {
            return;
        };
        self.filter = None;
        let came_from = self
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        self.dir = parent;
        self.reload();
        if let Some(name) = came_from {
            if let Some(idx) = self.entries.iter().position(|e| e.name == name) {
                self.selected = idx;
            }
        }
    }

    fn current_entry(&self) -> Option<&Entry> {
        if self.filter.is_some() && !self.visible_entry_indices().contains(&self.selected) {
            return None;
        }
        self.entries.get(self.selected)
    }

    fn visible_entry_indices(&self) -> Vec<usize> {
        match self.filter.as_deref() {
            Some(query) => filter_entries(&self.entries, query),
            None => (0..self.entries.len()).collect(),
        }
    }

    fn select_first_filter_match(&mut self) {
        if let Some(idx) = self.visible_entry_indices().first().copied() {
            self.selected = idx;
            self.scroll = 0;
        }
    }

    /// Enter で開く対象。".." なら親、フォルダなら中、ファイルなら今のディレクトリ。
    fn target_dir(&self) -> Option<PathBuf> {
        let entry = self.current_entry()?;
        let target = if entry.name == ".." {
            match self.dir.parent() {
                Some(p) => p.to_path_buf(),
                None => self.dir.clone(),
            }
        } else if entry.is_dir {
            self.dir.join(&entry.name)
        } else {
            self.dir.clone()
        };
        resolve_existing_dir(&target)
    }

    fn choose_target(&mut self) {
        if let Some(dir) = self.target_dir() {
            self.enter_agent_mode(dir);
        }
    }

    fn enter_agent_mode(&mut self, dir: PathBuf) {
        self.chosen_dir = Some(dir);
        self.agent_selected = 0;
        self.mode = Mode::Agent;
    }

    fn reload(&mut self) {
        self.entries = read_entries(&self.dir, self.show_hidden);
        self.selected = 0;
        self.scroll = 0;
    }

    /// 選択中エントリの中身（プレビュー用）。フォルダのときだけ中身を返す。
    fn preview_entries(&self) -> Vec<Entry> {
        let Some(entry) = self.current_entry() else {
            return Vec::new();
        };
        let target = if entry.name == ".." {
            match self.dir.parent() {
                Some(p) => p.to_path_buf(),
                None => return Vec::new(),
            }
        } else if entry.is_dir {
            self.dir.join(&entry.name)
        } else {
            return Vec::new();
        };
        let mut items = read_entries(&target, self.show_hidden);
        // プレビューでは ".." は不要。
        items.retain(|e| e.name != "..");
        items
    }

    fn render(&mut self, cols: usize, rows: usize) -> Vec<Line> {
        match self.mode {
            Mode::Browse => self.render_browse(cols, rows),
            Mode::Recent => self.render_recent(cols, rows),
            Mode::Agent => self.render_agent(cols, rows),
        }
    }

    fn render_browse(&mut self, cols: usize, rows: usize) -> Vec<Line> {
        // ヘッダ2行＋空1行、フッタ2行を確保。
        let body = rows.saturating_sub(5).max(1);
        let visible = self.visible_entry_indices();
        // 選択が見えるようにスクロールを合わせる。
        let selected_pos = visible
            .iter()
            .position(|idx| *idx == self.selected)
            .unwrap_or(0);
        if selected_pos < self.scroll {
            self.scroll = selected_pos;
        } else if selected_pos >= self.scroll + body {
            self.scroll = selected_pos + 1 - body;
        }

        let left_w = (cols.saturating_sub(3) * 2 / 5).clamp(18, cols.saturating_sub(6).max(18));
        let right_w = cols.saturating_sub(left_w + 1);
        let preview = self.preview_entries();

        let mut lines = Vec::with_capacity(rows);
        lines.push(segments_line(
            cols,
            &[("gototerm", DIR_FG), ("  開く場所を選ぶ", DIM)],
        ));
        lines.push(breadcrumb_line(cols, &display_path(&self.dir)));
        lines.push(text_line(cols, "", Color::White));

        for i in 0..body {
            let left_idx = visible.get(self.scroll + i).copied();
            let left = left_idx.and_then(|idx| self.entries.get(idx));
            let right = preview.get(i);
            let (ltext, lfg) = match left {
                Some(e) => (entry_label(e), entry_fg(e)),
                None => (String::new(), Color::White),
            };
            let selected = left_idx.is_some_and(|idx| idx == self.selected);
            let (rtext, rfg) = match right {
                Some(e) => (entry_label(e), entry_fg(e)),
                None => (String::new(), Color::White),
            };
            lines.push(two_pane_row(
                &ltext, lfg, selected, &rtext, rfg, left_w, right_w,
            ));
        }

        lines.push(text_line(cols, &"─".repeat(cols.min(120)), DIM));
        let footer = match self.filter.as_deref() {
            Some(query) => format!(
                "検索: {}_  ↑/↓:移動  l/→:入る  Enter:開く  Backspace/Esc:解除",
                query
            ),
            None => {
                "j/k:移動  l:入る  h:上へ  Enter:ここで開く  .:隠し  r:最近  Esc:閉じる".to_owned()
            }
        };
        lines.push(text_line(cols, &footer, DIM));
        lines.resize_with(rows, || text_line(cols, "", Color::White));
        lines.truncate(rows);
        lines
    }

    fn render_recent(&mut self, cols: usize, rows: usize) -> Vec<Line> {
        let mut lines = Vec::with_capacity(rows);
        lines.push(text_line(
            cols,
            "最近使ったプロジェクト",
            Color::BrightWhite,
        ));
        lines.push(text_line(cols, "", Color::White));

        let body = rows.saturating_sub(4).max(1);
        for i in 0..body {
            match self.recent.get(i) {
                Some(path) => {
                    let selected = i == self.recent_selected;
                    let label = format!("  {}", display_path(path));
                    lines.push(bar_line(cols, &label, DIR_FG, selected));
                }
                None => lines.push(text_line(cols, "", Color::White)),
            }
        }

        lines.push(text_line(cols, &"─".repeat(cols.min(120)), DIM));
        lines.push(text_line(
            cols,
            "j/k:移動  Enter:開く  r/Esc:ブラウザへ戻る",
            DIM,
        ));
        lines.resize_with(rows, || text_line(cols, "", Color::White));
        lines.truncate(rows);
        lines
    }

    /// エージェント選択は、ブラウザを下地に残したまま中央にフローティングの
    /// ポップアップ（nvim のフローティングウィンドウ風）を重ねて表示する。
    fn render_agent(&mut self, cols: usize, rows: usize) -> Vec<Line> {
        let mut lines = self.render_browse(cols, rows);
        self.overlay_agent_popup(&mut lines, cols, rows);
        lines
    }

    fn overlay_agent_popup(&self, lines: &mut [Line], cols: usize, rows: usize) {
        let subtitle = self
            .chosen_dir
            .as_deref()
            .map(display_path)
            .unwrap_or_else(|| display_path(&self.dir));

        // ポップアップの中身（テキスト, 文字色, 選択中の行か）。
        let mut content: Vec<(String, Color, bool)> = Vec::new();
        content.push(("何で開く？".to_owned(), ACCENT, false));
        content.push((subtitle, DIM, false));
        content.push((String::new(), Color::White, false));
        content.push((
            "そのまま作業（シェル）".to_owned(),
            Color::BrightWhite,
            self.agent_selected == 0,
        ));
        for (i, agent) in crate::TOYTERM_CONFIG.launcher_agents.iter().enumerate() {
            content.push((
                agent.name.clone(),
                Color::BrightWhite,
                self.agent_selected == i + 1,
            ));
        }
        content.push((String::new(), Color::White, false));
        content.push(("j/k:選択  Enter:起動  Esc:戻る".to_owned(), DIM, false));

        // 幅・高さと中央位置。
        let inner_w = content
            .iter()
            .map(|(t, _, _)| display_width(t))
            .max()
            .unwrap_or(10)
            .clamp(10, cols.saturating_sub(6).max(10));
        let interior_w = inner_w + 2; // 左右パディング1ずつ
        let box_w = (interior_w + 2).min(cols); // 左右のボーダー
        let box_h = (content.len() + 2).min(rows);
        let x0 = cols.saturating_sub(box_w) / 2;
        let y0 = rows.saturating_sub(box_h) / 2;

        let dash = interior_w.min(box_w.saturating_sub(2));
        // 上ボーダー
        stamp(lines, y0, x0, border_row('╭', '╮', dash));
        // 中身
        for (i, (text, fg, selected)) in content.iter().enumerate() {
            let y = y0 + 1 + i;
            if y >= y0 + box_h - 1 {
                break;
            }
            // ポップアップは不透明（透過を消す）＝不透明パネル色を敷く。選択行は青バー。
            let (fg, bg) = if *selected {
                (SEL_FG, SEL_BG)
            } else {
                (*fg, crate::view::panel_bg_color())
            };
            let interior = column_cells(&format!(" {text}"), fg, bg, interior_w);
            let mut row = Vec::with_capacity(box_w);
            row.push(border_cell('│'));
            row.extend(interior);
            row.push(border_cell('│'));
            stamp(lines, y, x0, row);
        }
        // 下ボーダー
        stamp(lines, y0 + box_h - 1, x0, border_row('╰', '╯', dash));
    }
}

fn display_width(s: &str) -> usize {
    s.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

/// ポップアップのボーダーセル（青のアクセント・不透明背景）。
fn border_cell(ch: char) -> Cell {
    let mut attr = GraphicAttribute::default();
    attr.fg = DIR_FG;
    attr.bg = crate::view::panel_bg_color();
    Cell::head(ch, 1, attr)
}

fn border_row(left: char, right: char, dash: usize) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(dash + 2);
    cells.push(border_cell(left));
    for _ in 0..dash {
        cells.push(border_cell('─'));
    }
    cells.push(border_cell(right));
    cells
}

/// base 行の列 x0 から、popup のセル列を上書きする。
fn stamp(lines: &mut [Line], y: usize, x0: usize, cells: Vec<Cell>) {
    let Some(line) = lines.get_mut(y) else {
        return;
    };
    let dst = line.cells_mut();
    for (i, cell) in cells.into_iter().enumerate() {
        if let Some(slot) = dst.get_mut(x0 + i) {
            *slot = cell;
        }
    }
}

fn filter_entries(entries: &[Entry], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.name != ".." && entry.name.to_lowercase().contains(&query))
        .map(|(idx, _)| idx)
        .collect()
}

/// ディレクトリの中身を読む。フォルダ→ファイルの順、名前昇順。親があれば先頭に ".."。
fn read_entries(dir: &Path, show_hidden: bool) -> Vec<Entry> {
    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten().take(2000) {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                dirs.push(name);
            } else {
                files.push(name);
            }
        }
    }
    dirs.sort();
    files.sort();

    let mut entries = Vec::with_capacity(dirs.len() + files.len() + 1);
    if dir.parent().is_some() {
        entries.push(Entry {
            name: "..".to_owned(),
            is_dir: true,
        });
    }
    entries.extend(dirs.into_iter().map(|name| Entry { name, is_dir: true }));
    entries.extend(files.into_iter().map(|name| Entry {
        name,
        is_dir: false,
    }));
    entries
}

fn entry_label(e: &Entry) -> String {
    let (icon, _) = entry_icon_fg(e);
    if e.name == ".." {
        format!("  {icon}  ../")
    } else if e.is_dir {
        format!("  {icon}  {}/", e.name)
    } else {
        format!("  {icon}  {}", e.name)
    }
}

fn entry_fg(e: &Entry) -> Color {
    entry_icon_fg(e).1
}

fn entry_icon_fg(e: &Entry) -> (char, Color) {
    icon_and_color(&e.name, e.is_dir)
}

fn resolve_existing_dir(path: &Path) -> Option<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let dir = if path.is_dir() {
        path
    } else if path.is_file() {
        path.parent()?.to_path_buf()
    } else {
        return None;
    };
    // ".." やシンボリックリンクを畳んで recent に綺麗なパスを残す。
    Some(std::fs::canonicalize(&dir).unwrap_or(dir))
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn display_path(path: &Path) -> String {
    if let Some(home) = home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_owned();
            }
            return format!("~/{}", rest.to_string_lossy());
        }
    }
    path.to_string_lossy().into_owned()
}

/// 左に文字を置くだけの1行（背景はパネル下地に任せる）。
fn text_line(cols: usize, text: &str, fg: Color) -> Line {
    Line::from_cells(fill_cells(text, fg, Color::Background, cols), false)
}

/// 複数の色つき区切り（セグメント）を左から並べた1行。ヘッダやパンくずに使う。
fn segments_line(cols: usize, segs: &[(&str, Color)]) -> Line {
    let mut cells = Vec::new();
    let mut used = 0usize;
    for (text, fg) in segs {
        for ch in text.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if w == 0 || used + w > cols {
                break;
            }
            let mut attr = GraphicAttribute::default();
            attr.fg = *fg;
            attr.bg = Color::Background;
            cells.push(Cell::head(ch, w as u16, attr));
            for i in 1..w {
                cells.push(Cell::spacer(i as u16));
            }
            used += w;
        }
    }
    while used < cols {
        let mut cell = Cell::new_ascii(' ');
        cell.attr.bg = Color::Background;
        cells.push(cell);
        used += 1;
    }
    Line::from_cells(cells, false)
}

/// パンくず（末尾のフォルダ名だけアクセント色で目立たせる）。
fn breadcrumb_line(cols: usize, path: &str) -> Line {
    match path.rfind('/') {
        Some(i) => segments_line(cols, &[(&path[..=i], DIM), (&path[i + 1..], ACCENT)]),
        None => segments_line(cols, &[(path, ACCENT)]),
    }
}

/// 選択時に青バーになる1行（recent 一覧などフル幅の行に使う）。
fn bar_line(cols: usize, text: &str, fg: Color, selected: bool) -> Line {
    let (fg, bg) = if selected {
        (SEL_FG, SEL_BG)
    } else {
        (fg, Color::Background)
    };
    Line::from_cells(fill_cells(text, fg, bg, cols), false)
}

/// 2ペイン行：左＝エントリ（選択時は白バー）、区切り │、右＝プレビュー。
fn two_pane_row(
    ltext: &str,
    lfg: Color,
    selected: bool,
    rtext: &str,
    rfg: Color,
    left_w: usize,
    right_w: usize,
) -> Line {
    let (lfg, lbg) = if selected {
        (SEL_FG, SEL_BG)
    } else {
        (lfg, Color::Background)
    };
    let mut cells = column_cells(ltext, lfg, lbg, left_w);
    let mut sattr = GraphicAttribute::default();
    sattr.fg = DIM;
    sattr.bg = Color::Background;
    cells.push(Cell::head('│', 1, sattr));
    cells.extend(column_cells(rtext, rfg, Color::Background, right_w));
    Line::from_cells(cells, false)
}

/// 指定 fg/bg で、ちょうど width セル分（足りなければ空白で埋める）を作る。
fn column_cells(text: &str, fg: Color, bg: Color, width: usize) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w == 0 || used + w > width {
            break;
        }
        let mut attr = GraphicAttribute::default();
        attr.fg = fg;
        attr.bg = bg;
        cells.push(Cell::head(ch, w as u16, attr));
        for i in 1..w {
            cells.push(Cell::spacer(i as u16));
        }
        used += w;
    }
    while used < width {
        let mut cell = Cell::new_ascii(' ');
        cell.attr.fg = fg;
        cell.attr.bg = bg;
        cells.push(cell);
        used += 1;
    }
    cells
}

/// 行全体（cols 幅）を fg/bg で満たす。
fn fill_cells(text: &str, fg: Color, bg: Color, cols: usize) -> Vec<Cell> {
    column_cells(text, fg, bg, cols)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tree() -> PathBuf {
        // テストは並列実行されるので、呼び出しごとに一意なディレクトリにする。
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("gototerm-launcher-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("apple").join("core")).unwrap();
        std::fs::create_dir_all(base.join("banana")).unwrap();
        std::fs::create_dir_all(base.join(".hidden")).unwrap();
        std::fs::write(base.join("readme.txt"), "hi").unwrap();
        base
    }

    #[test]
    fn read_entries_dirs_first_and_hides_dotfiles() {
        let base = temp_tree();
        let entries = read_entries(&base, false);
        // 先頭は ".."、次にフォルダ（apple, banana）、最後にファイル（readme.txt）。
        assert_eq!(entries[0].name, "..");
        assert_eq!(entries[1].name, "apple");
        assert!(entries[1].is_dir);
        assert_eq!(entries[2].name, "banana");
        assert_eq!(entries.last().unwrap().name, "readme.txt");
        assert!(!entries.iter().any(|e| e.name == ".hidden"));

        let with_hidden = read_entries(&base, true);
        assert!(with_hidden.iter().any(|e| e.name == ".hidden"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn descend_and_ascend_navigate_tree() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());

        // ".." を飛ばして apple を選び、中へ入る。
        state.selected = state
            .entries
            .iter()
            .position(|e| e.name == "apple")
            .unwrap();
        state.descend();
        assert_eq!(state.dir, base.join("apple"));
        assert!(state.entries.iter().any(|e| e.name == "core"));

        // 親へ戻ると、元いた apple が選択されている。
        state.ascend();
        assert_eq!(state.dir, base);
        assert_eq!(state.entries[state.selected].name, "apple");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn filter_entries_matches_case_insensitive_substrings_and_excludes_parent() {
        let entries = vec![
            Entry {
                name: "..".to_owned(),
                is_dir: true,
            },
            Entry {
                name: "Apple".to_owned(),
                is_dir: true,
            },
            Entry {
                name: "banana".to_owned(),
                is_dir: true,
            },
            Entry {
                name: "readme.txt".to_owned(),
                is_dir: false,
            },
        ];

        assert_eq!(filter_entries(&entries, "app"), vec![1]);
        assert_eq!(filter_entries(&entries, "ANA"), vec![2]);
        assert_eq!(filter_entries(&entries, "me."), vec![3]);
        assert_eq!(filter_entries(&entries, ""), vec![1, 2, 3]);
    }

    #[test]
    fn browse_enter_moves_to_agent_mode_with_chosen_directory() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        state.selected = state
            .entries
            .iter()
            .position(|e| e.name == "banana")
            .unwrap();

        let outcome = state.handle_key_parts(KeyCode::Enter, None);
        let expected = std::fs::canonicalize(base.join("banana")).unwrap();
        assert_eq!(outcome, LauncherOutcome::None);
        assert_eq!(state.mode, Mode::Agent);
        assert_eq!(state.chosen_dir, Some(expected));
        assert_eq!(state.agent_selected, 0);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn enter_on_file_chooses_containing_dir_for_agent_mode() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        state.selected = state
            .entries
            .iter()
            .position(|e| e.name == "readme.txt")
            .unwrap();

        let outcome = state.handle_key_parts(KeyCode::Enter, None);
        let expected = std::fs::canonicalize(&base).unwrap();
        assert_eq!(outcome, LauncherOutcome::None);
        assert_eq!(state.mode, Mode::Agent);
        assert_eq!(state.chosen_dir, Some(expected));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn escape_cancels() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        assert_eq!(
            state.handle_key_parts(KeyCode::Escape, None),
            LauncherOutcome::Cancelled
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn recent_mode_toggles_and_chooses_agent_dir() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(vec![base.join("banana")], base.clone());
        // r で recent モードへ。
        state.handle_key_parts(KeyCode::KeyR, Some("r"));
        assert_eq!(state.mode, Mode::Recent);
        // Enter で recent の先頭を Agent モードの対象にする。
        let outcome = state.handle_key_parts(KeyCode::Enter, None);
        let expected = std::fs::canonicalize(base.join("banana")).unwrap();
        assert_eq!(outcome, LauncherOutcome::None);
        assert_eq!(state.mode, Mode::Agent);
        assert_eq!(state.chosen_dir, Some(expected));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn agent_first_entry_enters_shell_with_no_command() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        let expected = std::fs::canonicalize(&base).unwrap();
        state.enter_agent_mode(expected.clone());

        let outcome = state.handle_key_parts(KeyCode::Enter, None);
        assert_eq!(
            outcome,
            LauncherOutcome::OpenIn {
                dir: expected,
                command: None
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn agent_second_entry_enters_claude_command() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        let expected = std::fs::canonicalize(&base).unwrap();
        state.enter_agent_mode(expected.clone());
        state.handle_key_parts(KeyCode::ArrowDown, None);

        let outcome = state.handle_key_parts(KeyCode::Enter, None);
        assert_eq!(
            outcome,
            LauncherOutcome::OpenIn {
                dir: expected,
                command: Some(vec!["claude".to_owned()])
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn agent_escape_returns_to_browse() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        let expected = std::fs::canonicalize(&base).unwrap();
        state.enter_agent_mode(expected);

        let outcome = state.handle_key_parts(KeyCode::Escape, None);
        assert_eq!(outcome, LauncherOutcome::None);
        assert_eq!(state.mode, Mode::Browse);
        assert_eq!(state.chosen_dir, None);
        assert_eq!(state.agent_selected, 0);
        let _ = std::fs::remove_dir_all(&base);
    }
}
