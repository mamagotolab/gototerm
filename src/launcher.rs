use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use unicode_normalization::UnicodeNormalization;
use unicode_width::UnicodeWidthChar;
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, Ime, KeyEvent},
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
    SaveWorkspace(String),
    OpenWorkspace(String),
    /// このディレクトリでターミナルを開く。
    OpenIn {
        dir: PathBuf,
        command: Option<Vec<String>>,
    },
    /// このファイルをエディタで開く（新タブ、cwd=親フォルダ）。
    OpenFile {
        file: PathBuf,
        dir: PathBuf,
    },
    /// OS の既定アプリで開く。ランチャーは開いたまま（続けて選べる）。
    OpenExternal {
        file: PathBuf,
    },
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
    pub(crate) fn workspace_notice(&mut self, notice: String) {
        self.state.workspace_notice = notice;
        self.state.mode = Mode::Workspaces;
        self.state.reload_workspaces();
        self.rebuild();
    }
    pub fn new(
        display: Display,
        viewport: Viewport,
        scale_factor: f64,
        recent: &[PathBuf],
    ) -> Self {
        let state = LauncherState::new(recent.to_vec());
        let mut launcher = Self {
            view: TerminalView::with_viewport(
                display,
                viewport,
                crate::TOYTERM_CONFIG.font_size,
                scale_factor,
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
        if self.view.increase_font_size(size_diff) {
            self.rebuild();
        }
    }

    pub fn set_scale_factor(&mut self, scale_factor: f64) -> bool {
        let changed = self.view.set_scale_factor(scale_factor);
        if changed {
            self.rebuild();
        }
        changed
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

    /// IME（日本語入力）のイベント。ランチャーが出ている間は裏のペインへ
    /// 流さず、ここで受けて入力欄へ入れる。
    pub fn handle_ime(&mut self, ime: &Ime) {
        self.state.handle_ime(ime);
        self.rebuild();
    }

    /// 変換候補ウィンドウを出す位置（入力欄のカーソルセル）。
    /// 文字を受け付けていない画面では None。
    pub fn ime_cursor_area(&self) -> Option<(PhysicalPosition<u32>, PhysicalSize<u32>)> {
        let (row, col) = self.state.ime_cursor?;
        let cell = self.view.cell_size();
        let vp = self.view.viewport();
        Some((
            PhysicalPosition::new(vp.x + col as u32 * cell.w, vp.y + row as u32 * cell.h),
            PhysicalSize::new(cell.w, cell.h),
        ))
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
    Workspaces,
    WorkspaceName,
    Browse,
    Recent,
    Bookmarks,
    Agent,
    /// Claude Code を選んだ直後の起動メニュー（名前入力＋別の始め方）。
    LaunchMenu,
    /// 過去セッションの一覧から選んで再開する画面。
    SessionHistory,
    /// フォルダ確定直後、gt hooks が未設定なら一度だけ挟む確認画面。
    HooksNudge,
}

#[derive(Clone, Debug)]
struct LauncherState {
    workspaces: Vec<crate::workspace_sets::WorkspaceSet>,
    workspace_selected: usize,
    workspace_name: String,
    workspace_notice: String,
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
    nudge_selected: usize,
    /// 起動メニューを出している間、起動予定のコマンドを預かっておく。
    pending_command: Option<Vec<String>>,
    /// None＝名前欄にカーソルがある（初期状態）。Some(i)＝別の始め方の i 行目。
    launch_row: Option<usize>,
    session_name: String,
    /// このフォルダの過去セッション（新しい順）。無ければ再開の行を出さない。
    sessions: Vec<crate::claude_sessions::Session>,
    history_selected: usize,
    /// IME で変換中の未確定文字列。確定すると入力欄へ移って空になる。
    preedit: String,
    /// 直前に IME が確定した時刻。確定の Enter を「決定」と取り違えないため。
    last_ime_commit: Instant,
    /// 変換候補ウィンドウを出すセル (row, col)。描画のたびに更新する。
    ime_cursor: Option<(usize, usize)>,
}

/// 起動メニューの「別の始め方」。名前入力の下に、使えるものだけ並べる。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaunchRow {
    /// 直前のセッションをそのまま続ける（`claude -c`）。
    Continue,
    /// 履歴から選んで再開する（`claude -r <id>`）。
    History,
}

impl LauncherState {
    fn reload_workspaces(&mut self) {
        match crate::workspace_sets::load_all() {
            Ok(sets) => self.workspaces = sets,
            Err(error) => {
                self.workspaces.clear();
                self.workspace_notice = error;
            }
        }
        self.workspace_selected = self
            .workspace_selected
            .min(self.workspaces.len().saturating_sub(1));
    }

    fn enter_workspaces(&mut self) {
        self.workspace_notice.clear();
        self.reload_workspaces();
        self.mode = Mode::Workspaces;
    }

    fn handle_workspace_key(&mut self, code: KeyCode, text: Option<&str>) -> LauncherOutcome {
        if self.mode == Mode::WorkspaceName {
            match code {
                KeyCode::Escape => self.mode = Mode::Workspaces,
                KeyCode::Backspace => {
                    self.workspace_name.pop();
                }
                KeyCode::Enter => {
                    let name = self.workspace_name.trim().to_owned();
                    match crate::workspace_sets::validate_name(&name) {
                        Ok(()) => return LauncherOutcome::SaveWorkspace(name),
                        Err(error) => self.workspace_notice = error,
                    }
                }
                _ => {
                    if let Some(text) = text {
                        self.workspace_name.push_str(text);
                    }
                }
            }
        } else {
            match code {
                KeyCode::Escape => self.mode = Mode::Browse,
                KeyCode::ArrowUp => {
                    self.workspace_selected = self.workspace_selected.saturating_sub(1)
                }
                KeyCode::ArrowDown => {
                    self.workspace_selected =
                        (self.workspace_selected + 1).min(self.workspaces.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(set) = self.workspaces.get(self.workspace_selected) {
                        return LauncherOutcome::OpenWorkspace(set.name.clone());
                    }
                }
                _ => match text {
                    Some("s") => {
                        self.workspace_name.clear();
                        self.workspace_notice.clear();
                        self.mode = Mode::WorkspaceName;
                    }
                    Some("d") => {
                        if let Some(set) = self.workspaces.get(self.workspace_selected) {
                            self.workspace_notice = match crate::workspace_sets::delete(&set.name) {
                                Ok(()) => "作業セットを削除しました".into(),
                                Err(error) => error,
                            };
                            self.reload_workspaces();
                        }
                    }
                    _ => {}
                },
            }
        }
        LauncherOutcome::None
    }

    fn render_workspaces(&mut self, cols: usize, rows: usize) -> Vec<Line> {
        let mut lines = vec![text_line(
            cols,
            "作業セット  Enter:新しいタブに復元  s:現在の配置を保存  d:削除  Esc:戻る",
            DIR_FG,
        )];
        if self.mode == Mode::WorkspaceName {
            let input = format!("名前: {}{}", self.workspace_name, self.preedit);
            lines.push(text_line(cols, &input, Color::White));
            self.ime_cursor = Some((
                1.min(rows.saturating_sub(1)),
                display_width(&input).min(cols.saturating_sub(1)),
            ));
            lines.push(text_line(cols, "Enter:保存  Esc:戻る", DIM));
        } else {
            let body = rows.saturating_sub(3).max(1);
            let offset = self.workspace_selected.saturating_sub(body - 1);
            for (i, set) in self.workspaces.iter().enumerate().skip(offset).take(body) {
                lines.push(bar_line(
                    cols,
                    &format!("  {}  ({}タブ)", set.name, set.tabs.len()),
                    Color::White,
                    i == self.workspace_selected,
                ));
            }
            if self.workspaces.is_empty() {
                lines.push(text_line(
                    cols,
                    "保存済みセットはありません。sで現在の配置を保存できます",
                    DIM,
                ));
            }
        }
        lines.push(text_line(cols, &self.workspace_notice, Color::Yellow));
        lines.resize_with(rows, || text_line(cols, "", Color::White));
        lines.truncate(rows);
        lines
    }
    fn new(recent: Vec<PathBuf>) -> Self {
        let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut state = Self {
            workspaces: Vec::new(),
            workspace_selected: 0,
            workspace_name: String::new(),
            workspace_notice: String::new(),
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
            nudge_selected: 0,
            pending_command: None,
            launch_row: None,
            session_name: String::new(),
            sessions: Vec::new(),
            history_selected: 0,
            preedit: String::new(),
            last_ime_commit: Instant::now() - Duration::from_secs(10),
            ime_cursor: None,
        };
        state.reload();
        state
    }

    #[cfg(test)]
    fn with_dir(recent: Vec<PathBuf>, dir: PathBuf) -> Self {
        let mut state = Self {
            workspaces: Vec::new(),
            workspace_selected: 0,
            workspace_name: String::new(),
            workspace_notice: String::new(),
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
            nudge_selected: 0,
            pending_command: None,
            launch_row: None,
            session_name: String::new(),
            sessions: Vec::new(),
            history_selected: 0,
            preedit: String::new(),
            last_ime_commit: Instant::now() - Duration::from_secs(10),
            ime_cursor: None,
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

    /// IME（日本語入力）のイベント。確定した文字だけが入力欄に入る。
    fn handle_ime(&mut self, ime: &Ime) {
        match ime {
            // 変換中の文字列。確定するまで入力欄に仮表示する。
            Ime::Preedit(text, _) => self.preedit = text.clone(),
            Ime::Commit(text) => {
                self.preedit.clear();
                self.last_ime_commit = Instant::now();
                self.insert_text(text);
            }
            Ime::Enabled | Ime::Disabled => self.preedit.clear(),
        }
    }

    /// 確定した文字列の行き先。文字を受け付ける画面だけが受け取る。
    fn insert_text(&mut self, text: &str) {
        match self.mode {
            Mode::WorkspaceName => self.workspace_name.push_str(text),
            // 名前欄にカーソルがあるときだけ（行を選んでいる間は入れない）。
            Mode::LaunchMenu if self.launch_row.is_none() => self.session_name.push_str(text),
            // 絞り込み中の `/` 検索。日本語のファイル名もここで打てる。
            Mode::Browse if self.filter.is_some() => self.push_filter_text(text),
            _ => {}
        }
    }

    fn handle_key_parts(&mut self, code: KeyCode, text: Option<&str>) -> LauncherOutcome {
        // 変換中のキーは IME のもの。ランチャーは手を出さない。
        if !self.preedit.is_empty() {
            return LauncherOutcome::None;
        }
        // 変換を確定した Enter が、確定(Commit)の直後にキーとしても届くこと
        // がある。そのまま決定に使うと、変換しただけで起動してしまう。
        if code == KeyCode::Enter && self.last_ime_commit.elapsed() < Duration::from_millis(50) {
            return LauncherOutcome::None;
        }
        match self.mode {
            Mode::Workspaces | Mode::WorkspaceName => self.handle_workspace_key(code, text),
            Mode::Browse => self.handle_browse_key(code, text),
            Mode::Recent => self.handle_recent_key(code, text),
            Mode::Bookmarks => self.handle_bookmark_key(code, text),
            Mode::Agent => self.handle_agent_key(code, text),
            Mode::LaunchMenu => self.handle_launch_menu_key(code, text),
            Mode::SessionHistory => self.handle_history_key(code, text),
            Mode::HooksNudge => self.handle_hooks_nudge_key(code, text),
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
                Some("w") => self.enter_workspaces(),
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
                        self.confirm_dir(dir);
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
        self.bookmarks
            .entries()
            .get(self.bookmark_selected)
            .cloned()
    }

    fn open_selected_bookmark(&mut self) {
        if let Some(dir) = self
            .selected_bookmark()
            .as_deref()
            .and_then(resolve_existing_dir)
        {
            self.confirm_dir(dir);
        }
    }

    fn browse_selected_bookmark(&mut self) {
        if let Some(dir) = self
            .selected_bookmark()
            .as_deref()
            .and_then(resolve_existing_dir)
        {
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
        self.bookmark_selected = (self.bookmark_selected as isize + delta).clamp(0, last) as usize;
    }

    /// 一覧の行頭に出す印。ブックマーク済みのフォルダなら "★"、それ以外は同じ幅の空白。
    fn bookmark_mark(&self, entry: &Entry) -> &'static str {
        if entry.name == ".." || !entry.is_dir {
            return " ";
        }
        if self
            .bookmarks
            .contains(&self.canonical_dir.join(&entry.name))
        {
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
                // Claude Code だけは起動メニュー（名前・再開）を挟む。
                // 他のエージェント・シェルは従来どおりそのまま起動。
                if command.as_deref().is_some_and(is_claude) {
                    self.enter_launch_menu(command, &dir);
                    return LauncherOutcome::None;
                }
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

    /// Claude Code の起動メニューを開く。カーソルは名前欄から始める
    /// （＝毎回まず「名前は？」と聞かれる。打たずに Enter なら名前なし）。
    fn enter_launch_menu(&mut self, command: Option<Vec<String>>, dir: &Path) {
        self.pending_command = command;
        self.session_name.clear();
        self.launch_row = None;
        self.history_selected = 0;
        self.sessions = crate::claude_sessions::recent_for_dir(dir, HISTORY_LIMIT);
        self.mode = Mode::LaunchMenu;
    }

    /// いま出せる「別の始め方」。過去セッションが無ければ空＝名前欄だけの画面になる。
    fn launch_rows(&self) -> Vec<LaunchRow> {
        match self.sessions.len() {
            0 => Vec::new(),
            // 1件しかないなら「続きから」と「履歴から選ぶ」は同じ意味。行を増やさない。
            1 => vec![LaunchRow::Continue],
            _ => vec![LaunchRow::Continue, LaunchRow::History],
        }
    }

    fn handle_launch_menu_key(&mut self, code: KeyCode, text: Option<&str>) -> LauncherOutcome {
        match code {
            KeyCode::Escape => self.back_to_agent_mode(),
            KeyCode::Enter => return self.activate_launch_selection(),
            KeyCode::ArrowDown => self.move_launch_row(1),
            KeyCode::ArrowUp => self.move_launch_row(-1),
            KeyCode::Backspace if self.launch_row.is_none() => {
                self.session_name.pop();
            }
            _ => {
                // 名前欄にカーソルがある間、文字はすべて名前（"c" で始まる名前も打てる）。
                // 行頭キーが効くのは ↓ で名前欄を出てから。
                match (self.launch_row, text) {
                    (None, Some(input)) if input.chars().all(|ch| !ch.is_control()) => {
                        self.session_name.push_str(input);
                    }
                    (Some(_), Some("j")) => self.move_launch_row(1),
                    (Some(_), Some("k")) => self.move_launch_row(-1),
                    (Some(_), Some("c")) => return self.activate_row(LaunchRow::Continue),
                    (Some(_), Some("r")) => return self.activate_row(LaunchRow::History),
                    _ => {}
                }
            }
        }
        LauncherOutcome::None
    }

    fn activate_launch_selection(&mut self) -> LauncherOutcome {
        match self.launch_row {
            // 名前欄で Enter。空なら名前なしで起動。
            None => {
                let name = self.session_name.trim().to_owned();
                let extra = if name.is_empty() {
                    Vec::new()
                } else {
                    vec!["-n".to_owned(), name]
                };
                self.launch_pending(extra)
            }
            Some(index) => match self.launch_rows().get(index).copied() {
                Some(row) => self.activate_row(row),
                None => LauncherOutcome::None,
            },
        }
    }

    fn activate_row(&mut self, row: LaunchRow) -> LauncherOutcome {
        if !self.launch_rows().contains(&row) {
            return LauncherOutcome::None;
        }
        match row {
            LaunchRow::Continue => self.launch_pending(vec!["-c".to_owned()]),
            LaunchRow::History => {
                self.history_selected = 0;
                self.mode = Mode::SessionHistory;
                LauncherOutcome::None
            }
        }
    }

    /// 名前欄（None）と行の間を上下する。行が無ければ名前欄から動かない。
    fn move_launch_row(&mut self, delta: isize) {
        let rows = self.launch_rows().len();
        if rows == 0 {
            return;
        }
        // 名前欄を -1 番目とみなして数える。
        let current = self.launch_row.map_or(-1, |i| i as isize);
        let next = (current + delta).clamp(-1, rows as isize - 1);
        self.launch_row = if next < 0 { None } else { Some(next as usize) };
    }

    fn handle_history_key(&mut self, code: KeyCode, text: Option<&str>) -> LauncherOutcome {
        match code {
            KeyCode::Escape => self.mode = Mode::LaunchMenu,
            KeyCode::Enter => {
                if let Some(session) = self.sessions.get(self.history_selected) {
                    let id = session.id.clone();
                    return self.launch_pending(vec!["-r".to_owned(), id]);
                }
            }
            KeyCode::ArrowDown => self.move_history(1),
            KeyCode::ArrowUp => self.move_history(-1),
            _ => match text {
                Some("j") => self.move_history(1),
                Some("k") => self.move_history(-1),
                _ => {}
            },
        }
        LauncherOutcome::None
    }

    fn move_history(&mut self, delta: isize) {
        if self.sessions.is_empty() {
            return;
        }
        let last = (self.sessions.len() - 1) as isize;
        self.history_selected = (self.history_selected as isize + delta).clamp(0, last) as usize;
    }

    /// 預けてあったコマンドに引数を足して起動する。
    fn launch_pending(&mut self, extra: Vec<String>) -> LauncherOutcome {
        let Some(dir) = self.chosen_dir.clone() else {
            self.mode = Mode::Browse;
            return LauncherOutcome::None;
        };
        let Some(mut command) = self.pending_command.take() else {
            self.mode = Mode::Browse;
            return LauncherOutcome::None;
        };
        command.extend(extra);
        LauncherOutcome::OpenIn {
            dir,
            command: Some(command),
        }
    }

    /// 起動メニューをやめてエージェント選択に戻る。
    fn back_to_agent_mode(&mut self) {
        self.pending_command = None;
        self.session_name.clear();
        self.launch_row = None;
        self.sessions = Vec::new();
        self.mode = Mode::Agent;
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
            self.confirm_dir(dir);
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

    /// フォルダを確定させた全経路（ブラウズ／最近使った／ブックマーク）が通る入口。
    /// Claude Code 連携が未設定なら一度だけ確認画面を挟み、そうでなければ
    /// 従来どおりそのままエージェント選択へ進む。
    fn confirm_dir(&mut self, dir: PathBuf) {
        if hooks_nudge_applicable(&dir) {
            self.chosen_dir = Some(dir);
            self.nudge_selected = 0;
            self.mode = Mode::HooksNudge;
        } else {
            self.enter_agent_mode(dir);
        }
    }

    fn handle_hooks_nudge_key(&mut self, code: KeyCode, text: Option<&str>) -> LauncherOutcome {
        match code {
            KeyCode::Escape => self.resolve_hooks_nudge(false),
            KeyCode::Enter => {
                let accept = self.nudge_selected == 0;
                self.resolve_hooks_nudge(accept);
            }
            KeyCode::ArrowDown => self.move_nudge_selection(1),
            KeyCode::ArrowUp => self.move_nudge_selection(-1),
            _ => match text {
                Some("j") => self.move_nudge_selection(1),
                Some("k") => self.move_nudge_selection(-1),
                _ => {}
            },
        }
        LauncherOutcome::None
    }

    fn move_nudge_selection(&mut self, delta: isize) {
        self.nudge_selected = (self.nudge_selected as isize + delta).clamp(0, 1) as usize;
    }

    fn resolve_hooks_nudge(&mut self, accept: bool) {
        let Some(dir) = self.chosen_dir.clone() else {
            self.mode = Mode::Browse;
            return;
        };
        if accept {
            // 書き込みに失敗しても（権限等）致命的ではないので、致命的扱いにせず進む。
            // gt init-hooks 同様、既存ファイルがあれば上書きしない判定は
            // hooks_nudge_applicable 側で既に済ませてある。
            let _ = write_hooks_config(&dir);
        }
        self.enter_agent_mode(dir);
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
        // 変換候補の位置は描くたびに置き直す（入力欄の無い画面では出さない）。
        self.ime_cursor = None;
        match self.mode {
            Mode::Workspaces | Mode::WorkspaceName => self.render_workspaces(cols, rows),
            Mode::Browse => self.render_browse(cols, rows),
            Mode::Recent => self.render_recent(cols, rows),
            Mode::Bookmarks => self.render_bookmarks(cols, rows),
            Mode::Agent => self.render_agent(cols, rows),
            Mode::LaunchMenu => self.render_launch_menu(cols, rows),
            Mode::SessionHistory => self.render_history(cols, rows),
            Mode::HooksNudge => self.render_hooks_nudge(cols, rows),
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
                "j/k:移動  l:入る  h:上へ  Enter:開く  o:既定アプリ  m:★登録  b:★一覧  w:作業セット  .:隠し  r:最近  Esc:閉じる"
                    .to_owned()
            }
        };
        lines.push(text_line(cols, &footer, DIM));
        // 絞り込み中は変換候補を検索欄の下に出す。
        if let Some(query) = self.filter.as_deref() {
            let col = display_width("検索: ") + display_width(query);
            self.ime_cursor = Some((lines.len() - 1, col));
        }
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

        overlay_list_popup(lines, cols, rows, &content);
    }

    fn render_launch_menu(&mut self, cols: usize, rows: usize) -> Vec<Line> {
        let mut lines = self.render_browse(cols, rows);
        self.overlay_launch_menu_popup(&mut lines, cols, rows);
        lines
    }

    fn overlay_launch_menu_popup(&mut self, lines: &mut [Line], cols: usize, rows: usize) {
        let subtitle = self
            .chosen_dir
            .as_deref()
            .map(display_path)
            .unwrap_or_else(|| display_path(&self.dir));

        let mut content: Vec<(String, Color, bool)> = Vec::new();
        content.push((pad_to("Claude Code で開く", MENU_W), ACCENT, false));
        content.push((fit_width(&subtitle, MENU_W), DIM, false));
        content.push((String::new(), Color::White, false));

        // 名前欄。カーソルがここにある間だけ末尾に "_" を出す。
        // 変換中の文字（preedit）は確定前でも見えるよう、そのまま欄に並べる。
        let on_name = self.launch_row.is_none();
        let cursor = if on_name { "_" } else { "" };
        let name_index = content.len();
        let typed = format!(" 名前: {}{}", self.session_name, self.preedit);
        content.push((
            pad_to(&format!("{typed}{cursor}"), MENU_W),
            Color::BrightWhite,
            on_name,
        ));
        let name_cursor = (name_index, display_width(&typed));

        let rows_list = self.launch_rows();
        if !rows_list.is_empty() {
            content.push((String::new(), Color::White, false));
            content.push((pad_to(" ↓ 別の始め方", MENU_W), DIM, false));
            for (i, row) in rows_list.iter().enumerate() {
                content.push((
                    pad_to(&self.launch_row_label(*row), MENU_W),
                    Color::BrightWhite,
                    self.launch_row == Some(i),
                ));
            }
        }

        content.push((String::new(), Color::White, false));
        let footer = if on_name {
            "Enter:開始（空なら名前なし）  ↓:別の始め方  Esc:戻る"
        } else {
            "Enter:決定  ↑:名前へ戻る  Esc:やめる"
        };
        content.push((fit_width(footer, MENU_W), DIM, false));

        let (x0, y0) = overlay_list_popup(lines, cols, rows, &content);
        // 変換候補ウィンドウは名前欄のカーソル位置に出す。
        // 中身の行は「枠+左パディング」の分だけ右にずれている。
        if on_name {
            let (row, col) = name_cursor;
            self.ime_cursor = Some((y0 + 1 + row, x0 + 2 + col));
        }
    }

    /// 「別の始め方」1行分の文字列。続きからの行には直前セッションの見出しを添える。
    fn launch_row_label(&self, row: LaunchRow) -> String {
        match row {
            LaunchRow::Continue => match self.sessions.first() {
                Some(session) => {
                    const PREFIX: &str = "  c  続きから   ";
                    let room = MENU_W.saturating_sub(display_width(PREFIX));
                    format!("{PREFIX}{}", session_row(session, room))
                }
                None => "  c  続きから".to_owned(),
            },
            LaunchRow::History => format!("  r  履歴から選ぶ…（{}件）", self.sessions.len()),
        }
    }

    fn render_history(&mut self, cols: usize, rows: usize) -> Vec<Line> {
        let mut lines = self.render_browse(cols, rows);
        self.overlay_history_popup(&mut lines, cols, rows);
        lines
    }

    fn overlay_history_popup(&self, lines: &mut [Line], cols: usize, rows: usize) {
        let mut content: Vec<(String, Color, bool)> = Vec::new();
        content.push((pad_to("続きから", MENU_W), ACCENT, false));
        content.push((String::new(), Color::White, false));

        for (i, session) in self.sessions.iter().enumerate() {
            // 名前を付けたセッションは明るく、名前なし（最初の発言で代用）は控えめに。
            let fg = if session.named {
                Color::BrightWhite
            } else {
                Color::White
            };
            content.push((
                pad_to(&format!(" {}", session_row(session, MENU_W - 1)), MENU_W),
                fg,
                i == self.history_selected,
            ));
        }

        content.push((String::new(), Color::White, false));
        content.push((
            fit_width("j/k:選択  Enter:再開  Esc:戻る", MENU_W),
            DIM,
            false,
        ));

        overlay_list_popup(lines, cols, rows, &content);
    }

    fn render_hooks_nudge(&mut self, cols: usize, rows: usize) -> Vec<Line> {
        let mut lines = self.render_browse(cols, rows);
        self.overlay_hooks_nudge_popup(&mut lines, cols, rows);
        lines
    }

    fn overlay_hooks_nudge_popup(&self, lines: &mut [Line], cols: usize, rows: usize) {
        let mut content: Vec<(String, Color, bool)> = Vec::new();
        content.push(("Claude Code 連携を設定しますか？".to_owned(), ACCENT, false));
        content.push((
            "変更ファイル・許可待ち・完了通知が使えるようになります".to_owned(),
            DIM,
            false,
        ));
        content.push((String::new(), Color::White, false));
        content.push((
            "設定する（.claude/settings.local.json に書き込み）".to_owned(),
            Color::BrightWhite,
            self.nudge_selected == 0,
        ));
        content.push((
            "今回はしない".to_owned(),
            Color::BrightWhite,
            self.nudge_selected == 1,
        ));
        content.push((String::new(), Color::White, false));
        content.push((
            "j/k:選択  Enter:決定  Esc:今回はしない".to_owned(),
            DIM,
            false,
        ));

        overlay_list_popup(lines, cols, rows, &content);
    }
}

/// 中央寄せの罫線ボックスとして、選択可能な項目リストを stamp する。
/// エージェント選択・hooks 連携確認など、複数のポップアップで共通の描画。
/// 画面中央にポップアップを重ねる。返り値は枠の左上セル (x0, y0)
/// （中の入力欄の位置を呼び出し側で求めるために使う）。
fn overlay_list_popup(
    lines: &mut [Line],
    cols: usize,
    rows: usize,
    content: &[(String, Color, bool)],
) -> (usize, usize) {
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
    (x0, y0)
}

pub(crate) fn display_width(s: &str) -> usize {
    s.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

/// ポップアップのボーダーセル（青のアクセント・不透明背景）。
pub(crate) fn border_cell(ch: char) -> Cell {
    let mut attr = GraphicAttribute::default();
    attr.fg = DIR_FG;
    attr.bg = crate::view::panel_bg_color();
    Cell::head(ch, 1, attr)
}

pub(crate) fn border_row(left: char, right: char, dash: usize) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(dash + 2);
    cells.push(border_cell(left));
    for _ in 0..dash {
        cells.push(border_cell('─'));
    }
    cells.push(border_cell(right));
    cells
}

/// base 行の列 x0 から、popup のセル列を上書きする。
pub(crate) fn stamp(lines: &mut [Line], y: usize, x0: usize, cells: Vec<Cell>) {
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

/// 起動メニューの内側の幅（半角換算）。全行をこの幅に揃えるので、
/// 名前を打っても履歴の件数が変わっても枠が伸び縮みしない。
const MENU_W: usize = 54;
/// 履歴に出す最大件数（枠の高さが下地のブラウザを潰さない範囲）。
const HISTORY_LIMIT: usize = 8;

/// 「2時間前」。format_age は "いま" だけ助詞が付くと変になるので分ける。
fn session_age(session: &crate::claude_sessions::Session) -> String {
    // 時計が巻き戻っている等で経過が取れなければ、時刻表示だけ諦める。
    let Ok(elapsed) = session.modified.elapsed() else {
        return String::new();
    };
    let age = crate::timeline::format_age(elapsed);
    if age == "いま" {
        age
    } else {
        format!("{age}前")
    }
}

/// 「見出し           2時間前」の1行。幅 `width` の中で経過時間を右端に寄せる。
fn session_row(session: &crate::claude_sessions::Session, width: usize) -> String {
    let age = session_age(session);
    let room = width.saturating_sub(display_width(&age) + 2);
    let title = fit_width(&session.label, room);
    let gap = room.saturating_sub(display_width(&title)) + 2;
    format!("{title}{}{age}", " ".repeat(gap))
}

/// 表示幅 w に収める。切ったら末尾に「…」を付ける。
fn fit_width(text: &str, w: usize) -> String {
    if display_width(text) <= w {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > w.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

/// 表示幅 w ちょうどに揃える（長ければ切る、短ければ空白で埋める）。
fn pad_to(text: &str, w: usize) -> String {
    let text = fit_width(text, w);
    let pad = w.saturating_sub(display_width(&text));
    format!("{text}{}", " ".repeat(pad))
}

/// Claude Code のコマンドか（`-n` や `-c` を足してよい相手か）。
/// `claude` / `claude.cmd` / フルパス指定のいずれでも拾う。
fn is_claude(command: &[String]) -> bool {
    let Some(program) = command.first() else {
        return false;
    };
    Path::new(program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_ascii_lowercase() == "claude")
        .unwrap_or(false)
}

/// gt hooks 連携を提案してよいか。Claude Code が入っていない環境では無意味な
/// 提案になるので出さない。既に `.claude/settings.local.json` があるなら
/// （gt hooks 済みでも、他の設定でも）触れない＝提案自体を出さない。
fn hooks_nudge_applicable(dir: &Path) -> bool {
    #[cfg(windows)]
    if !std::env::current_exe()
        .ok()
        .is_some_and(|exe| exe.with_file_name("gototerm-hook.exe").is_file())
    {
        return false;
    }
    crate::multiplexer::command_exists("claude")
        && !dir.join(".claude/settings.local.json").exists()
}

/// `assets/bin/gt` の `hook_snippet()` と同じ内容。gt が未導入の環境でも
/// 導線が成立するよう、シェルアウトせず直接書き込む（keep in sync with gt script）。
#[cfg(not(windows))]
const HOOK_SNIPPET: &str = r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write|MultiEdit|NotebookEdit",
        "hooks": [
          {
            "type": "command",
            "command": "gt hook"
          }
        ]
      }
    ],
    "Notification": [
      { "hooks": [ { "type": "command", "command": "gt hook" } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "command": "gt hook" } ] }
    ],
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "gt hook" } ] }
    ],
    "SessionEnd": [
      { "hooks": [ { "type": "command", "command": "gt hook" } ] }
    ]
  }
}
"#;

#[cfg(not(windows))]
fn write_hooks_config(dir: &Path) -> std::io::Result<()> {
    let claude_dir = dir.join(".claude");
    std::fs::create_dir_all(&claude_dir)?;
    std::fs::write(claude_dir.join("settings.local.json"), HOOK_SNIPPET)
}

#[cfg(windows)]
fn write_hooks_config(dir: &Path) -> std::io::Result<()> {
    let exe = std::env::current_exe()?.with_file_name("gototerm-hook.exe");
    crate::agent_hooks::setup(dir, &exe, &["claude"], false).map_err(std::io::Error::other)
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
pub(crate) fn column_cells(text: &str, fg: Color, bg: Color, width: usize) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut used = 0usize;
    for ch in text.nfc() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        // NFCで合成できない幅0文字には独立セルを割り当てられないが、
        // そこで打ち切らず後続文字の描画は続ける。
        if w == 0 {
            continue;
        }
        if used + w > width {
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

    #[test]
    fn workspace_name_accepts_ime_without_submitting_commit_enter() {
        let mut state = LauncherState::with_dir(vec![], std::env::temp_dir());
        state.mode = Mode::WorkspaceName;
        state.handle_ime(&Ime::Commit("開発用".into()));
        assert_eq!(
            state.handle_key_parts(KeyCode::Enter, None),
            LauncherOutcome::None
        );
        state.last_ime_commit = Instant::now() - Duration::from_secs(1);
        assert_eq!(
            state.handle_key_parts(KeyCode::Enter, None),
            LauncherOutcome::SaveWorkspace("開発用".into())
        );
        state.mode = Mode::Workspaces;
        state.workspaces = vec![crate::workspace_sets::WorkspaceSet {
            name: "開発用".into(),
            tabs: vec![],
        }];
        assert_eq!(
            state.handle_key_parts(KeyCode::Enter, None),
            LauncherOutcome::OpenWorkspace("開発用".into())
        );
        state.handle_key_parts(KeyCode::Escape, None);
        assert_eq!(state.mode, Mode::Browse);
    }

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
        // hooks 連携確認ポップアップは agent モード遷移のテストと無関係なので、
        // 設定済み扱いにして確実にスキップさせる（claude バイナリの有無という
        // 実行環境依存の条件でテストが揺れないように）。
        for dir in [&base, &base.join("apple"), &base.join("banana")] {
            std::fs::create_dir_all(dir.join(".claude")).unwrap();
            std::fs::write(dir.join(".claude").join("settings.local.json"), "{}").unwrap();
        }
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
        let idx = state
            .entries
            .iter()
            .position(|e| e.name == "apple")
            .unwrap();
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
        assert!(
            state.bookmarks.contains(&apple),
            "選択中フォルダが登録される"
        );
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
        assert!(
            state.bookmarks.contains(&apple),
            "親ではなく apple が登録される"
        );

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
        assert_eq!(
            state.entries[state.selected].name, "..",
            "入った直後は .. が選択"
        );

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
    fn hooks_nudge_not_applicable_when_settings_already_exist() {
        let base = temp_tree();
        // temp_tree() は既に .claude/settings.local.json を用意している
        // （claude バイナリの有無に関わらずスキップされるべき、が本題）。
        assert!(!hooks_nudge_applicable(&base));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn hooks_nudge_accept_writes_settings_and_enters_agent_mode() {
        let base = temp_tree();
        // このテストの主題は「未設定のフォルダ」なので、temp_tree() が用意した
        // settings.local.json を消してから、hooks 連携確認が出た体で駆動する
        // （routing 条件=claude バイナリの有無はここでは検証対象にしない）。
        let settings = base.join(".claude").join("settings.local.json");
        std::fs::remove_file(&settings).unwrap();

        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        state.chosen_dir = Some(base.clone());
        state.mode = Mode::HooksNudge;
        state.nudge_selected = 0; // 「設定する」

        state.handle_key_parts(KeyCode::Enter, None);

        assert_eq!(state.mode, Mode::Agent);
        assert_eq!(state.chosen_dir, Some(base.clone()));
        let written = std::fs::read_to_string(&settings).unwrap();
        #[cfg(not(windows))]
        assert!(written.contains("PostToolUse"));
        #[cfg(windows)]
        assert!(written.contains("gototerm-hook.exe"));
        assert!(written.contains("Notification"));
        assert!(written.contains("Stop"));
        assert!(written.contains("SessionStart"));
        assert!(written.contains("SessionEnd"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn hooks_nudge_decline_does_not_write_but_still_enters_agent_mode() {
        let base = temp_tree();
        let settings = base.join(".claude").join("settings.local.json");
        std::fs::remove_file(&settings).unwrap();

        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        state.chosen_dir = Some(base.clone());
        state.mode = Mode::HooksNudge;
        state.nudge_selected = 1; // 「今回はしない」

        state.handle_key_parts(KeyCode::Enter, None);

        assert_eq!(state.mode, Mode::Agent);
        assert!(!settings.exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn hooks_nudge_escape_also_declines() {
        let base = temp_tree();
        let settings = base.join(".claude").join("settings.local.json");
        std::fs::remove_file(&settings).unwrap();

        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        state.chosen_dir = Some(base.clone());
        state.mode = Mode::HooksNudge;
        state.nudge_selected = 0; // 選択が「設定する」でも Esc は今回はしない扱い

        state.handle_key_parts(KeyCode::Escape, None);

        assert_eq!(state.mode, Mode::Agent);
        assert!(!settings.exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn move_nudge_selection_clamps_to_two_options() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        state.nudge_selected = 0;
        state.move_nudge_selection(-1);
        assert_eq!(state.nudge_selected, 0);
        state.move_nudge_selection(1);
        assert_eq!(state.nudge_selected, 1);
        state.move_nudge_selection(1);
        assert_eq!(state.nudge_selected, 1);
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

    /// テスト用の過去セッション（新しい順に並べたつもりの2件）。
    fn fake_sessions(count: usize) -> Vec<crate::claude_sessions::Session> {
        (0..count)
            .map(|i| crate::claude_sessions::Session {
                id: format!("{i}{i}{i}{i}{i}{i}{i}{i}-1111-1111-1111-111111111111"),
                label: format!("過去の作業{i}"),
                named: i == 0,
                modified: std::time::SystemTime::now()
                    - std::time::Duration::from_secs(600 * (i as u64 + 1)),
            })
            .collect()
    }

    /// Claude Code を選ぶと起動メニューが出て、カーソルは名前欄にある（＝先に名前を聞かれる）。
    #[test]
    fn claude_opens_the_launch_menu_on_the_name_field() {
        let base = temp_tree();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        let expected = std::fs::canonicalize(&base).unwrap();
        state.enter_agent_mode(expected);
        state.handle_key_parts(KeyCode::ArrowDown, None);

        let outcome = state.handle_key_parts(KeyCode::Enter, None);
        assert_eq!(outcome, LauncherOutcome::None);
        assert_eq!(state.mode, Mode::LaunchMenu);
        assert_eq!(state.launch_row, None, "カーソルは名前欄");
        assert_eq!(state.pending_command, Some(vec!["claude".to_owned()]));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 名前欄に到達するまでの共通手順。
    fn open_launch_menu(base: &Path) -> (LauncherState, PathBuf) {
        let mut state = LauncherState::with_dir(Vec::new(), base.to_path_buf());
        let dir = std::fs::canonicalize(base).unwrap();
        state.enter_agent_mode(dir.clone());
        state.handle_key_parts(KeyCode::ArrowDown, None);
        state.handle_key_parts(KeyCode::Enter, None);
        (state, dir)
    }

    /// 空のまま Enter＝名前なしで起動（`-n ""` は渡さない）。
    #[test]
    fn empty_name_launches_plain_claude() {
        let base = temp_tree();
        let (mut state, dir) = open_launch_menu(&base);
        state.handle_key_parts(KeyCode::Space, Some(" "));

        let outcome = state.handle_key_parts(KeyCode::Enter, None);
        assert_eq!(
            outcome,
            LauncherOutcome::OpenIn {
                dir,
                command: Some(vec!["claude".to_owned()])
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 打った名前が `-n <名前>` になる（Backspace も効く）。
    #[test]
    fn typed_name_becomes_the_name_flag() {
        let base = temp_tree();
        let (mut state, dir) = open_launch_menu(&base);
        for ch in ["府", "中", " ", "改", "修"] {
            state.handle_key_parts(KeyCode::KeyA, Some(ch));
        }
        state.handle_key_parts(KeyCode::Backspace, None);

        let outcome = state.handle_key_parts(KeyCode::Enter, None);
        assert_eq!(
            outcome,
            LauncherOutcome::OpenIn {
                dir,
                command: Some(vec![
                    "claude".to_owned(),
                    "-n".to_owned(),
                    "府中 改".to_owned()
                ])
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 名前欄にいる間は、行頭キーと同じ文字も名前として打てる
    /// （"claude改修" のような名前が打てなくなるのを防ぐ）。
    #[test]
    fn letters_stay_in_the_name_field() {
        let base = temp_tree();
        let (mut state, _) = open_launch_menu(&base);
        state.sessions = fake_sessions(2);

        for ch in ["c", "r", "j"] {
            state.handle_key_parts(KeyCode::KeyA, Some(ch));
        }

        assert_eq!(state.session_name, "crj");
        assert_eq!(state.mode, Mode::LaunchMenu, "履歴画面へ飛んでいない");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 過去セッションが無いフォルダでは、名前欄だけ（↓を押しても動かない）。
    #[test]
    fn without_history_there_are_no_extra_rows() {
        let base = temp_tree();
        let (mut state, _) = open_launch_menu(&base);
        assert!(state.launch_rows().is_empty());

        state.handle_key_parts(KeyCode::ArrowDown, None);
        assert_eq!(state.launch_row, None);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 履歴が1件だけなら「続きから」だけ（「履歴から選ぶ」は同じ意味なので出さない）。
    #[test]
    fn a_single_session_shows_only_the_continue_row() {
        let base = temp_tree();
        let (mut state, _) = open_launch_menu(&base);
        state.sessions = fake_sessions(1);
        assert_eq!(state.launch_rows(), vec![LaunchRow::Continue]);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// ↓で「続きから」へ降りて Enter＝`claude -c`。
    #[test]
    fn continue_row_launches_with_the_continue_flag() {
        let base = temp_tree();
        let (mut state, dir) = open_launch_menu(&base);
        state.sessions = fake_sessions(2);

        state.handle_key_parts(KeyCode::ArrowDown, None);
        assert_eq!(state.launch_row, Some(0));
        let outcome = state.handle_key_parts(KeyCode::Enter, None);

        assert_eq!(
            outcome,
            LauncherOutcome::OpenIn {
                dir,
                command: Some(vec!["claude".to_owned(), "-c".to_owned()])
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 名前欄を出たあとは行頭キーが効く（r で履歴へ）。選んだセッションは `-r <id>`。
    #[test]
    fn history_resumes_the_selected_session_by_id() {
        let base = temp_tree();
        let (mut state, dir) = open_launch_menu(&base);
        state.sessions = fake_sessions(3);

        state.handle_key_parts(KeyCode::ArrowDown, None); // 名前欄を出る
        state.handle_key_parts(KeyCode::KeyR, Some("r")); // 行頭キー
        assert_eq!(state.mode, Mode::SessionHistory);

        state.handle_key_parts(KeyCode::KeyJ, Some("j")); // 2件目を選ぶ
        let outcome = state.handle_key_parts(KeyCode::Enter, None);

        assert_eq!(
            outcome,
            LauncherOutcome::OpenIn {
                dir,
                command: Some(vec![
                    "claude".to_owned(),
                    "-r".to_owned(),
                    fake_sessions(3)[1].id.clone()
                ])
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Esc は一段ずつ：履歴→起動メニュー→エージェント選択。
    #[test]
    fn escape_walks_back_one_step() {
        let base = temp_tree();
        let (mut state, _) = open_launch_menu(&base);
        state.sessions = fake_sessions(2);
        state.handle_key_parts(KeyCode::ArrowDown, None);
        state.handle_key_parts(KeyCode::KeyR, Some("r"));

        state.handle_key_parts(KeyCode::Escape, None);
        assert_eq!(state.mode, Mode::LaunchMenu);

        state.handle_key_parts(KeyCode::Escape, None);
        assert_eq!(state.mode, Mode::Agent);
        assert_eq!(state.pending_command, None);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// IME で確定した日本語が名前欄に入る（変換中は仮表示のまま）。
    #[test]
    fn ime_text_goes_into_the_name_field() {
        let base = temp_tree();
        let (mut state, dir) = open_launch_menu(&base);

        // 変換中（未確定）。欄には出るが、名前としてはまだ確定していない。
        state.handle_ime(&Ime::Preedit("ふちゅう".to_owned(), None));
        assert_eq!(state.preedit, "ふちゅう");
        assert_eq!(state.session_name, "");

        state.handle_ime(&Ime::Commit("府中".to_owned()));
        assert_eq!(state.preedit, "", "確定したら仮表示は消える");
        assert_eq!(state.session_name, "府中");

        // 確定 Enter と紛れないよう時間を空けてから決定する。
        state.last_ime_commit = Instant::now() - Duration::from_secs(1);
        let outcome = state.handle_key_parts(KeyCode::Enter, None);
        assert_eq!(
            outcome,
            LauncherOutcome::OpenIn {
                dir,
                command: Some(vec![
                    "claude".to_owned(),
                    "-n".to_owned(),
                    "府中".to_owned()
                ])
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 変換中のキーは IME のもの。ランチャーは反応しない
    /// （変換確定の Enter でそのまま起動してしまうのを防ぐ）。
    #[test]
    fn keys_during_conversion_do_not_launch() {
        let base = temp_tree();
        let (mut state, _) = open_launch_menu(&base);

        state.handle_ime(&Ime::Preedit("ふちゅう".to_owned(), None));
        assert_eq!(
            state.handle_key_parts(KeyCode::Enter, None),
            LauncherOutcome::None
        );
        assert_eq!(
            state.handle_key_parts(KeyCode::Escape, None),
            LauncherOutcome::None
        );
        assert_eq!(state.mode, Mode::LaunchMenu, "Esc でも画面は変わらない");

        // 確定の直後に届く Enter（Windows で二重に来る）も決定にしない。
        state.handle_ime(&Ime::Commit("府中".to_owned()));
        assert_eq!(
            state.handle_key_parts(KeyCode::Enter, None),
            LauncherOutcome::None
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 行を選んでいる間は、確定した文字を名前欄に入れない。
    #[test]
    fn ime_text_is_ignored_while_a_row_is_selected() {
        let base = temp_tree();
        let (mut state, _) = open_launch_menu(&base);
        state.sessions = fake_sessions(2);
        state.handle_key_parts(KeyCode::ArrowDown, None);

        state.handle_ime(&Ime::Commit("府中".to_owned()));
        assert_eq!(state.session_name, "");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `/` の絞り込みでも日本語で打てる（日本語のファイル名を探せる）。
    #[test]
    fn ime_text_goes_into_the_filter() {
        let base = temp_tree();
        std::fs::write(base.join("府中の資料.md"), "x").unwrap();
        let mut state = LauncherState::with_dir(Vec::new(), base.clone());
        state.handle_key_parts(KeyCode::Slash, Some("/"));

        state.handle_ime(&Ime::Commit("府中".to_owned()));
        assert_eq!(state.filter.as_deref(), Some("府中"));
        let hit = state.current_entry().map(|e| e.name.clone());
        assert_eq!(hit.as_deref(), Some("府中の資料.md"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 起動メニューを出すのは Claude Code だけ（Codex やシェルは即起動）。
    #[test]
    fn only_claude_gets_the_launch_menu() {
        assert!(is_claude(&["claude".to_owned()]));
        assert!(is_claude(&["/usr/bin/claude".to_owned()]));
        assert!(is_claude(&["claude.cmd".to_owned()]));
        assert!(!is_claude(&["codex".to_owned()]));
        assert!(!is_claude(&[]));
    }

    /// 枠が伸び縮みしないよう、全行を同じ表示幅に揃える（日本語は2セル）。
    #[test]
    fn rows_are_padded_to_the_same_display_width() {
        assert_eq!(display_width(&pad_to("府中", 10)), 10);
        assert_eq!(display_width(&pad_to("abc", 10)), 10);
        // 長すぎる見出しは「…」を付けて切る。
        let cut = fit_width("府中コンパスの改修作業", 10);
        assert!(cut.ends_with('…'));
        assert!(display_width(&cut) <= 10);
    }

    #[test]
    fn column_cells_preserve_combined_glyphs_and_keep_following_text() {
        let cells = column_cells(
            "e\u{301}か\u{3099}状態",
            Color::White,
            Color::Background,
            10,
        );
        let text: String = cells
            .iter()
            .filter(|cell| cell.width > 0)
            .map(|cell| cell.ch)
            .collect();
        assert!(text.contains("éが"));
        assert!(text.contains("状態"));
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
