use std::path::{Path, PathBuf};

use unicode_width::UnicodeWidthChar;
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
};

use crate::bookmarks::Bookmarks;
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
    /// このファイルをエディタで開く（新タブ、cwd=親フォルダ）。
    OpenFile { file: PathBuf, dir: PathBuf },
    /// OS の既定アプリで開く。ランチャーは開いたまま（続けて選べる）。
    OpenExternal { file: PathBuf },
    /// 何もせず閉じる。
    Cancelled,
    /// まだ操作中。
    None,
}

/// エディタでなく OS の既定アプリで開くべき拡張子か（画像・PDF・音楽・圧縮など）。
/// ここに無いものは「テキスト」とみなしてエディタで開く。
fn opens_externally(name: &str) -> bool {
    let ext = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => return false,
    };
    matches!(
        ext.as_str(),
        // 画像（svg はテキストなのでエディタ側）
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "heic"
            | "avif"
            // 文書
            | "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp"
            // 音声・動画
            | "mp3" | "wav" | "flac" | "ogg" | "m4a" | "opus" | "mp4" | "mkv" | "mov" | "avi"
            | "webm"
            // 圧縮・イメージ
            | "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "jar"
            | "iso" | "img" | "deb" | "rpm" | "apk" | "msi"
            // バイナリ
            | "exe" | "dll" | "so" | "dylib" | "bin" | "o" | "a" | "class"
            // フォント
            | "ttf" | "otf" | "ttc" | "woff" | "woff2"
    )
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

    pub fn change_font_size(&mut self, size_diff: i32) {
        self.view.increase_font_size(size_diff);
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
    Bookmarks,
    Agent,
}

#[derive(Clone, Debug)]
struct LauncherState {
    /// いま中身を見せているディレクトリ。
    dir: PathBuf,
    /// dir の中身（親があれば先頭に ".."）。
    entries: Vec<Entry>,
    /// dir を畳んだ絶対パス。★印の判定を Enter で開く対象（正規化済み）と揃えるために持つ。
    /// 行ごとに canonicalize すると毎フレーム stat が走るので、reload のときだけ求める。
    canonical_dir: PathBuf,
    selected: usize,
    scroll: usize,
    show_hidden: bool,
    recent: Vec<PathBuf>,
    recent_selected: usize,
    /// よく使うフォルダ（本人が m で付けたもの）。
    bookmarks: Bookmarks,
    bookmark_selected: usize,
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
            canonical_dir: dir.clone(),
            dir,
            selected: 0,
            scroll: 0,
            show_hidden: false,
            recent,
            recent_selected: 0,
            bookmarks: Bookmarks::load(),
            bookmark_selected: 0,
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
            canonical_dir: dir.clone(),
            dir,
            selected: 0,
            scroll: 0,
            show_hidden: false,
            recent,
            recent_selected: 0,
            bookmarks: Bookmarks::empty_for_test(),
            bookmark_selected: 0,
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
            Mode::Bookmarks => self.handle_bookmark_key(code, text),
            Mode::Agent => self.handle_agent_key(code, text),
        }
    }

    fn handle_browse_key(&mut self, code: KeyCode, text: Option<&str>) -> LauncherOutcome {
        if self.filter.is_some() {
            return self.handle_filter_key(code, text);
        }
        match code {
            KeyCode::Escape => return LauncherOutcome::Cancelled,
            KeyCode::Enter => return self.choose_target(),
            KeyCode::ArrowDown => self.move_sel(1),
            KeyCode::ArrowUp => self.move_sel(-1),
            KeyCode::ArrowRight => self.descend(),
            KeyCode::ArrowLeft => self.ascend(),
            _ => match text {
                Some("j") => self.move_sel(1),
                Some("k") => self.move_sel(-1),
                Some("l") => self.descend(),
                Some("h") => self.ascend(),
                Some("o") => return self.open_selected_external(),
                Some("/") => self.start_filter(),
                Some(".") => {
                    self.show_hidden = !self.show_hidden;
                    self.reload();
                }
                Some("r") if !self.recent.is_empty() => {
                    self.mode = Mode::Recent;
                    self.recent_selected = 0;
                }
                // m でいま開こうとしているフォルダ（Enter と同じ対象）を付け外し。
                Some("m") => self.toggle_bookmark(),
                Some("b") if !self.bookmarks.entries().is_empty() => {
                    self.mode = Mode::Bookmarks;
                    self.bookmark_selected = 0;
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
            KeyCode::Enter => return self.choose_target(),
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

    fn handle_bookmark_key(&mut self, code: KeyCode, text: Option<&str>) -> LauncherOutcome {
        match code {
            KeyCode::Escape => self.mode = Mode::Browse,
            // Enter=そこで開く。l/→=そこへ移動して中を見る（ブラウザと同じ流儀）。
            KeyCode::Enter => self.open_selected_bookmark(),
            KeyCode::ArrowRight => self.browse_selected_bookmark(),
            KeyCode::ArrowDown => self.move_bookmark(1),
            KeyCode::ArrowUp => self.move_bookmark(-1),
            _ => match text {
                Some("j") => self.move_bookmark(1),
                Some("k") => self.move_bookmark(-1),
                Some("l") => self.browse_selected_bookmark(),
                Some("b") => self.mode = Mode::Browse,
                // 一覧からそのまま外せる（消したいときに探し直さなくて済む）。
                Some("m") | Some("d") => self.remove_selected_bookmark(),
                _ => {}
            },
        }
        LauncherOutcome::None
    }

    fn selected_bookmark(&self) -> Option<PathBuf> {
        self.bookmarks.entries().get(self.bookmark_selected).cloned()
    }

    fn open_selected_bookmark(&mut self) {
        if let Some(dir) = self.selected_bookmark().as_deref().and_then(resolve_existing_dir) {
            self.enter_agent_mode(dir);
        }
    }

    fn browse_selected_bookmark(&mut self) {
        if let Some(dir) = self.selected_bookmark().as_deref().and_then(resolve_existing_dir) {
            self.dir = dir;
            self.filter = None;
            self.mode = Mode::Browse;
            self.reload();
        }
    }

    fn remove_selected_bookmark(&mut self) {
        let Some(path) = self.selected_bookmark() else {
            return;
        };
        self.bookmarks.toggle(&path);
        if self.bookmarks.entries().is_empty() {
            self.mode = Mode::Browse;
            return;
        }
        self.bookmark_selected = self
            .bookmark_selected
            .min(self.bookmarks.entries().len() - 1);
    }

    fn move_bookmark(&mut self, delta: isize) {
        let len = self.bookmarks.entries().len();
        if len == 0 {
            return;
        }
        let last = (len - 1) as isize;
        self.bookmark_selected =
            (self.bookmark_selected as isize + delta).clamp(0, last) as usize;
    }

    /// 一覧の行頭に出す印。ブックマーク済みのフォルダなら "★"、それ以外は同じ幅の空白。
    fn bookmark_mark(&self, entry: &Entry) -> &'static str {
        if entry.name == ".." || !entry.is_dir {
            return " ";
        }
        if self.bookmarks.contains(&self.canonical_dir.join(&entry.name)) {
            "★"
        } else {
            " "
        }
    }

    /// Enter で開く対象と同じフォルダを付け外しする（選択中フォルダ、`..`/ファイルなら今の場所）。
    fn toggle_bookmark(&mut self) {
        if let Some(dir) = self.target_dir() {
            self.bookmarks.toggle(&dir);
        }
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
            // `..` は「上へ移動する行」であって開く対象ではない。フォルダへ入った直後は
            // ここが選ばれているので、Enter では素直に「いま居るフォルダ」を開く
            // （親を開きたいときは h で上がってから Enter）。
            self.dir.clone()
        } else if entry.is_dir {
            self.dir.join(&entry.name)
        } else {
            self.dir.clone()
        };
        resolve_existing_dir(&target)
    }

    fn choose_target(&mut self) -> LauncherOutcome {
        // ファイル上の Enter は「そのファイルを開く」。フォルダ（と ".."）は従来どおり
        // エージェント選択へ。人の期待（ファイルを選んで Enter＝開く）に合わせる。
        if let Some(entry) = self.current_entry().cloned() {
            if entry.name != ".." && !entry.is_dir {
                return self.open_file_outcome(&entry.name);
            }
        }
        if let Some(dir) = self.target_dir() {
            self.enter_agent_mode(dir);
        }
        LauncherOutcome::None
    }

    /// 選択中ファイルを開く Outcome を作る。画像・PDF 等は OS の既定アプリ、
    /// それ以外（テキスト）はエディタで開く。
    fn open_file_outcome(&self, name: &str) -> LauncherOutcome {
        let file = self.canonical_dir.join(name);
        if !file.is_file() {
            return LauncherOutcome::None;
        }
        if opens_externally(name) {
            return LauncherOutcome::OpenExternal { file };
        }
        let dir = file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.canonical_dir.clone());
        LauncherOutcome::OpenFile { file, dir }
    }

    /// o キー：選択中ファイルを OS の既定アプリで開く（テキストでも強制的に）。
    fn open_selected_external(&self) -> LauncherOutcome {
        let Some(entry) = self.current_entry() else {
            return LauncherOutcome::None;
        };
        if entry.name == ".." || entry.is_dir {
            return LauncherOutcome::None;
        }
        let file = self.canonical_dir.join(&entry.name);
        if !file.is_file() {
            return LauncherOutcome::None;
        }
        LauncherOutcome::OpenExternal { file }
    }

    fn enter_agent_mode(&mut self, dir: PathBuf) {
        self.chosen_dir = Some(dir);
        self.agent_selected = 0;
        self.mode = Mode::Agent;
    }

    fn reload(&mut self) {
        self.entries = read_entries(&self.dir, self.show_hidden);
        self.canonical_dir = resolve_existing_dir(&self.dir).unwrap_or_else(|| self.dir.clone());
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
            Mode::Bookmarks => self.render_bookmarks(cols, rows),
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
                // 左ペインだけ、ブックマーク済みのフォルダに ★ を付ける（どれが登録済みか分かるように）。
                Some(e) => (
                    format!("{}{}", self.bookmark_mark(e), entry_label(e)),
                    entry_fg(e),
                ),
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
                "j/k:移動  l:入る  h:上へ  Enter:開く  o:既定アプリ  m:★登録  b:★一覧  .:隠し  r:最近  Esc:閉じる"
                    .to_owned()
            }
        };
        lines.push(text_line(cols, &footer, DIM));
        lines.resize_with(rows, || text_line(cols, "", Color::White));
        lines.truncate(rows);
        lines
    }

    fn render_bookmarks(&mut self, cols: usize, rows: usize) -> Vec<Line> {
        let mut lines = Vec::with_capacity(rows);
        lines.push(text_line(cols, "★ ブックマーク", Color::BrightWhite));
        lines.push(text_line(cols, "", Color::White));

        let body = rows.saturating_sub(4).max(1);
        for i in 0..body {
            match self.bookmarks.entries().get(i) {
                Some(path) => {
                    let selected = i == self.bookmark_selected;
                    let label = format!("  {}", display_path(path));
                    lines.push(bar_line(cols, &label, DIR_FG, selected));
                }
                None => lines.push(text_line(cols, "", Color::White)),
            }
        }

        lines.push(text_line(cols, &"─".repeat(cols.min(120)), DIM));
        lines.push(text_line(
            cols,
            "j/k:移動  Enter:開く  l:そこへ移動  m/d:外す  b/Esc:ブラウザへ戻る",
            DIM,
        ));
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
    fn opens_externally_by_extension() {
        // 画像・PDF・圧縮は既定アプリ、テキスト系はエディタ。
        assert!(opens_externally("photo.PNG"));
        assert!(opens_externally("report.pdf"));
        assert!(opens_externally("archive.tar.gz"));
        assert!(!opens_externally("README.md"));
        assert!(!opens_externally("main.rs"));
        assert!(!opens_externally("diagram.svg"));
        // 拡張子なし・ドットファイルはエディタ側。
        assert!(!opens_externally("Makefile"));
        assert!(!opens_externally(".gitignore"));
    }

    #[test]
    fn enter_on_text_file_opens_editor() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        let idx = state
            .entries
            .iter()
            .position(|e| e.name == "readme.txt")
            .unwrap();
        state.selected = idx;

        let outcome = state.choose_target();
        let canonical = base.canonicalize().unwrap();
        assert_eq!(
            outcome,
            LauncherOutcome::OpenFile {
                file: canonical.join("readme.txt"),
                dir: canonical,
            }
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn enter_on_image_file_opens_external() {
        let base = temp_tree();
        std::fs::write(base.join("shot.png"), b"png").unwrap();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        let idx = state
            .entries
            .iter()
            .position(|e| e.name == "shot.png")
            .unwrap();
        state.selected = idx;

        let outcome = state.choose_target();
        let canonical = base.canonicalize().unwrap();
        assert_eq!(
            outcome,
            LauncherOutcome::OpenExternal {
                file: canonical.join("shot.png"),
            }
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn enter_on_dir_still_enters_agent_mode() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        let idx = state
            .entries
            .iter()
            .position(|e| e.name == "apple")
            .unwrap();
        state.selected = idx;

        let outcome = state.choose_target();
        assert_eq!(outcome, LauncherOutcome::None);
        assert_eq!(state.mode, Mode::Agent);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn o_key_forces_external_for_any_file() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        let idx = state
            .entries
            .iter()
            .position(|e| e.name == "readme.txt")
            .unwrap();
        state.selected = idx;

        // テキストでも o なら既定アプリ。
        let outcome = state.open_selected_external();
        let canonical = base.canonicalize().unwrap();
        assert_eq!(
            outcome,
            LauncherOutcome::OpenExternal {
                file: canonical.join("readme.txt"),
            }
        );

        // フォルダでは何もしない。
        let idx = state.entries.iter().position(|e| e.name == "apple").unwrap();
        state.selected = idx;
        assert_eq!(state.open_selected_external(), LauncherOutcome::None);

        let _ = std::fs::remove_dir_all(&base);
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

    /// m で付けた印は、Enter で開く対象と同じフォルダに付く（★も出る）。
    #[test]
    fn m_marks_the_folder_enter_would_open() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        state.selected = state
            .entries
            .iter()
            .position(|e| e.name == "apple")
            .unwrap();

        state.handle_key_parts(KeyCode::KeyM, Some("m"));

        let apple = std::fs::canonicalize(base.join("apple")).unwrap();
        assert!(state.bookmarks.contains(&apple), "選択中フォルダが登録される");
        let entry = state.entries[state.selected].clone();
        assert_eq!(state.bookmark_mark(&entry), "★", "一覧に印が出る");

        // もう一度 m で外れる。
        state.handle_key_parts(KeyCode::KeyM, Some("m"));
        assert!(!state.bookmarks.contains(&apple));
        assert_eq!(state.bookmark_mark(&entry), " ");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// フォルダに入った直後（`..` が選択）の m は、いま居るフォルダを登録する。
    #[test]
    fn m_on_parent_row_marks_the_current_dir() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        state.selected = state
            .entries
            .iter()
            .position(|e| e.name == "apple")
            .unwrap();
        state.descend();

        state.handle_key_parts(KeyCode::KeyM, Some("m"));

        let apple = std::fs::canonicalize(base.join("apple")).unwrap();
        assert!(state.bookmarks.contains(&apple), "親ではなく apple が登録される");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn bookmark_list_opens_with_enter_and_jumps_with_l() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        let banana = std::fs::canonicalize(base.join("banana")).unwrap();
        state.bookmarks.toggle(&banana);
        state.mode = Mode::Bookmarks;
        state.bookmark_selected = 0;

        // l はそのフォルダへ移動して中を見る。
        state.handle_key_parts(KeyCode::KeyL, Some("l"));
        assert_eq!(state.mode, Mode::Browse);
        assert_eq!(state.dir, banana);

        // Enter はそこで開く（エージェント選択へ）。
        state.mode = Mode::Bookmarks;
        state.handle_key_parts(KeyCode::Enter, None);
        assert_eq!(state.mode, Mode::Agent);
        assert_eq!(state.chosen_dir, Some(banana.clone()));

        state.bookmarks.toggle(&banana);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn removing_the_last_bookmark_returns_to_browse() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        let banana = std::fs::canonicalize(base.join("banana")).unwrap();
        state.bookmarks.toggle(&banana);
        state.mode = Mode::Bookmarks;

        state.handle_key_parts(KeyCode::KeyD, Some("d"));

        assert!(state.bookmarks.entries().is_empty());
        assert_eq!(state.mode, Mode::Browse, "空になったら一覧に留まらない");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn b_does_nothing_when_there_are_no_bookmarks() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());

        state.handle_key_parts(KeyCode::KeyB, Some("b"));

        assert_eq!(state.mode, Mode::Browse, "空の一覧は開かない");

        let _ = std::fs::remove_dir_all(&base);
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

    /// フォルダへ入った直後は選択が `..` に戻る。そこで Enter を押したときに
    /// 開くのは「いま居るフォルダ」であって、親ではない。
    #[test]
    fn enter_on_parent_row_opens_the_current_dir_not_its_parent() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        state.selected = state
            .entries
            .iter()
            .position(|e| e.name == "apple")
            .unwrap();
        state.descend();
        assert_eq!(state.entries[state.selected].name, "..", "入った直後は .. が選択");

        let outcome = state.handle_key_parts(KeyCode::Enter, None);
        let expected = std::fs::canonicalize(base.join("apple")).unwrap();
        assert_eq!(outcome, LauncherOutcome::None);
        assert_eq!(state.chosen_dir, Some(expected), "親ではなく apple が開く");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn enter_on_file_opens_it_not_agent_mode() {
        // v0.6.0 で仕様変更：ファイル上の Enter は「そのファイルを開く」。
        // （以前は「今のフォルダでエージェント選択」だった）
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        state.selected = state
            .entries
            .iter()
            .position(|e| e.name == "readme.txt")
            .unwrap();

        let outcome = state.handle_key_parts(KeyCode::Enter, None);
        let canonical = std::fs::canonicalize(&base).unwrap();
        assert_eq!(
            outcome,
            LauncherOutcome::OpenFile {
                file: canonical.join("readme.txt"),
                dir: canonical,
            }
        );
        assert_eq!(state.mode, Mode::Browse);

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
