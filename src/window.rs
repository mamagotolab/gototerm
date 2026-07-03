use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::{CursorIcon, Window},
};

use crate::gt::GtMessage;
use crate::terminal::TerminalSize;
use crate::view::{Selection, TerminalView, Viewport};
use crate::vt::{ShellLocation, VtTerminal};
use crate::Display;

type CursorPosition = PhysicalPosition<f64>;

/// URL を OS 標準のブラウザで開く。Linux は xdg-open。Windows は
/// rundll32 の FileProtocolHandler を使う。explorer に URL を渡すと
/// 引数をパスと誤解してフォルダを開くことがあるため使わない。
pub(crate) fn open_url(url: &str) {
    use std::process::Command;
    #[cfg(not(windows))]
    let result = Command::new("xdg-open").arg(url).spawn();
    #[cfg(windows)]
    let result = Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    if let Err(e) = result {
        log::error!("URL を開けませんでした ({}): {}", url, e);
    }
}

fn is_link_token_char(c: char) -> bool {
    // 空白を含むパスは端末上のトークン境界が曖昧なので、Phase 4 では扱わない。
    !c.is_whitespace()
        && c != '\0'
        && !matches!(
            c,
            '"' | '\'' | '<' | '>' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '│'
        )
}

fn looks_like_path(token: &str) -> bool {
    token.contains('/') || token.starts_with("./") || token.starts_with("~/")
}

fn resolve_existing_file_token(token: &str, cwd: &Path) -> Option<PathBuf> {
    resolve_path_token(token, cwd).filter(|path| path.is_file())
}

pub(crate) fn resolve_path_token(token: &str, cwd: &Path) -> Option<PathBuf> {
    if token.is_empty() || token.starts_with("http://") || token.starts_with("https://") {
        return None;
    }

    let path = if let Some(rest) = token.strip_prefix("~/") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(rest))?
    } else {
        let raw = Path::new(token);
        if raw.is_absolute() {
            raw.to_path_buf()
        } else if cwd.is_absolute() {
            cwd.join(raw)
        } else {
            std::env::current_dir().ok()?.join(cwd).join(raw)
        }
    };

    Some(normalize_absolute_path(&path))
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }

    out
}

/// Wayland のクリップボードへ書き込む（wl-copy にパイプ）。
#[cfg(unix)]
fn set_clipboard(text: &str) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    match Command::new("wl-copy").stdin(Stdio::piped()).spawn() {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            // wl-copy は stdin を読み終えると選択を保持するため wait しない
        }
        Err(e) => log::error!("wl-copy の起動に失敗: {}", e),
    }
}

/// Wayland のクリップボードから読み出す（wl-paste）。
#[cfg(unix)]
fn get_clipboard() -> String {
    use std::process::Command;
    match Command::new("wl-paste").arg("--no-newline").output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(e) => {
            log::error!("wl-paste の起動に失敗: {}", e);
            String::new()
        }
    }
}

/// Windows のクリップボードへ書き込む（arboard）。
#[cfg(windows)]
fn set_clipboard(text: &str) {
    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text.to_owned())) {
        Ok(()) => {}
        Err(e) => log::error!("クリップボード書き込みに失敗: {}", e),
    }
}

/// Windows のクリップボードから読み出す（arboard）。
#[cfg(windows)]
fn get_clipboard() -> String {
    match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
        Ok(text) => text,
        Err(e) => {
            log::error!("クリップボード読み出しに失敗: {}", e);
            String::new()
        }
    }
}

pub struct TerminalWindow {
    window: Rc<Window>,
    terminal: VtTerminal,

    view: TerminalView,
    focused: bool,
    modifiers: ModifiersState,
    mouse: MouseState,
    /// 直近の IME 確定(Commit)時刻。確定 Enter を端末へ送らないため
    /// （Windows では確定 Enter が Commit とキー入力の両方で来る）。
    last_ime_commit: std::time::Instant,
    clicked_file: Option<PathBuf>,
}

/// viewport とフォントから端末セル数を求めて VtTerminal を作る。
fn make_terminal(
    view: &TerminalView,
    viewport: Viewport,
    cwd: &std::path::Path,
    command: Option<&[String]>,
) -> VtTerminal {
    let cell_size = view.cell_size();
    let scroll_bar_width = crate::TOYTERM_CONFIG.scroll_bar_width;
    let cols = ((viewport.w.saturating_sub(scroll_bar_width)) / cell_size.w).max(1) as usize;
    let lines = (viewport.h / cell_size.h).max(1) as usize;
    VtTerminal::new(
        cols,
        lines,
        cell_size.w as u16,
        cell_size.h as u16,
        cwd,
        command,
    )
}

struct MouseState {
    wheel_delta_x: f32,
    wheel_delta_y: f32,
    cursor_pos: CursorPosition,
    pressed_pos: Option<CursorPosition>,
    released_pos: Option<CursorPosition>,
    // 選択を絶対行に固定するため、押下/離した時点のスクロール量を覚えておく。
    pressed_offset: i64,
    released_offset: i64,
    // 押下時に矩形選択(Alt)だったか。
    block: bool,
    // 現在のドラッグがローカル選択か（押下時に確定）。途中で Shift を離しても
    // ボタンを離すまでローカル選択を続け、released_pos を確実に立てるため。
    selecting: bool,
    click_count: usize,
    last_clicked: std::time::Instant,
}

impl TerminalWindow {
    pub fn with_viewport(
        window: Rc<Window>,
        display: Display,
        viewport: Viewport,
        cwd: Option<&std::path::Path>,
    ) -> Self {
        Self::with_viewport_command(window, display, viewport, cwd, None)
    }

    pub fn with_viewport_command(
        window: Rc<Window>,
        display: Display,
        viewport: Viewport,
        cwd: Option<&std::path::Path>,
        command: Option<&[String]>,
    ) -> Self {
        let font_size = crate::TOYTERM_CONFIG.font_size;
        let view = TerminalView::with_viewport(display, viewport, font_size, Some((0, viewport.h)));

        let terminal = {
            let parent_cwd = std::env::current_dir().expect("cwd");
            let child_cwd = cwd.unwrap_or(&parent_cwd);
            make_terminal(&view, viewport, child_cwd, command)
        };

        // Use I-beam mouse cursor
        window.set_cursor_icon(CursorIcon::Text);

        // 日本語入力（IME）を有効化する。これを呼ばないと winit が
        // text-input-v3 を enable せず、fcitx5 等に入力が渡らない。
        window.set_ime_allowed(true);

        TerminalWindow {
            window,
            terminal,

            view,
            focused: true,
            modifiers: ModifiersState::empty(),
            mouse: MouseState {
                wheel_delta_x: 0.0,
                wheel_delta_y: 0.0,
                cursor_pos: CursorPosition::default(),
                pressed_pos: None,
                released_pos: None,
                pressed_offset: 0,
                released_offset: 0,
                block: false,
                selecting: false,
                click_count: 0,
                last_clicked: std::time::Instant::now() - std::time::Duration::from_secs(10),
            },
            last_ime_commit: std::time::Instant::now() - std::time::Duration::from_secs(10),
            clicked_file: None,
        }
    }

    pub fn close_pty(&mut self) {
        self.terminal.kill();
    }

    // Change cursor icon according to the current mouse_track mode
    pub fn refresh_cursor_icon(&mut self) {
        let icon = if self.terminal.mouse_mode() {
            CursorIcon::Default
        } else {
            CursorIcon::Text
        };
        self.window.set_cursor_icon(icon);
    }

    /// このペインの再描画が必要か（前回 draw 以降に内容が変わったか）。
    pub fn needs_redraw(&self) -> bool {
        self.view.needs_redraw()
    }

    /// カーソル点滅の表示フェーズを設定する。
    pub fn set_cursor_blink(&mut self, on: bool) {
        self.view.set_cursor_blink(on);
    }

    // Returns true if the PTY is closed, false otherwise
    pub fn check_update(&mut self) -> bool {
        let cell_size = self.view.cell_size();

        if self.terminal.has_exited() {
            return true;
        }

        let (cols, rows) = self.terminal.size();
        let terminal_size = TerminalSize { rows, cols };

        // 画面が変わったときだけ alacritty のグリッドを取り込んで描画を更新する。
        if self.terminal.take_dirty() {
            let snapshot = self.terminal.snapshot();

            if let Some(cursor) = snapshot.cursor.filter(|_| self.focused) {
                // 変換候補ウィンドウをカーソルのセル位置に出す（over-the-spot）。
                // フォーカス中のペインだけが IME 位置を更新する。
                self.window.set_ime_cursor_area(
                    PhysicalPosition::new(
                        self.viewport().x + cursor.col as u32 * cell_size.w,
                        self.viewport().y + cursor.row as u32 * cell_size.h,
                    ),
                    PhysicalSize::new(cell_size.w, cell_size.h),
                );
            }

            self.view.update_contents(|view| {
                view.lines = snapshot.lines;
                view.cursor = snapshot.cursor;
                view.images = snapshot.images;
                view.scroll_bar = None;
                view.view_focused = self.focused;
            });
        }

        // Update text selection（スクロール追従＋矩形対応）
        if let Some(CursorPosition { x: sx, y: sy }) = self.mouse.pressed_pos {
            let CursorPosition { x: ex, y: ey } =
                self.mouse.released_pos.unwrap_or(self.mouse.cursor_pos);

            let lines = &self.view.lines;
            let rows = terminal_size.rows;
            let cols = terminal_size.cols;
            let rows_i = rows as i64;

            // 列はスクロールの影響を受けないのでピクセルから直接。
            let x_max = cell_size.w as f64 * cols as f64;
            let sx = sx.clamp(0.0, x_max - 0.1);
            let ex = ex.clamp(0.0, x_max - 0.1);
            let mut s_col = (sx / cell_size.w as f64).round() as usize;
            let mut e_col = (ex / cell_size.w as f64).round() as usize;

            // 行は「押下/離した時点のスクロール量」で絶対行に正規化し、現在の
            // スクロール量で画面行へ戻す。これで選択が中身に貼り付き、スクロール
            // しても付いていく（以前は画面位置に固定されていてズレた）。
            let cur_off = self.terminal.display_offset() as i64;
            let end_off = if self.mouse.released_pos.is_some() {
                self.mouse.released_offset
            } else {
                cur_off
            };
            let s_row_cap = ((sy / cell_size.h as f64).floor() as i64).clamp(0, rows_i - 1);
            let e_row_cap = ((ey / cell_size.h as f64).floor() as i64).clamp(0, rows_i - 1);
            let s_row_now = s_row_cap - self.mouse.pressed_offset + cur_off;
            let e_row_now = e_row_cap - end_off + cur_off;

            let new_selection_range = if (s_row_now < 0 && e_row_now < 0)
                || (s_row_now >= rows_i && e_row_now >= rows_i)
            {
                // スクロールで選択が完全に画面外へ出た → 何も塗らない。
                None
            } else {
                let mut s_row = s_row_now.clamp(0, rows_i - 1) as usize;
                let mut e_row = e_row_now.clamp(0, rows_i - 1) as usize;

                if self.mouse.block {
                    // 矩形選択：行範囲 × 列範囲（各軸 min/max の閉区間）。
                    let top = s_row.min(e_row);
                    let bottom = s_row.max(e_row);
                    let left = s_col.min(e_col);
                    let right = s_col.max(e_col).saturating_sub(1);
                    if left <= right {
                        Some(Selection::Block {
                            top,
                            bottom,
                            left,
                            right,
                        })
                    } else {
                        None
                    }
                } else {
                    if (e_row, e_col) < (s_row, s_col) {
                        std::mem::swap(&mut s_row, &mut e_row);
                        std::mem::swap(&mut s_col, &mut e_col);
                    }

                    // NOTE: selecton is closed range [s, e]
                    e_col = e_col.saturating_sub(1);

                    match self.mouse.click_count {
                        // single click: character selection
                        1 => {}

                        // double click: word selection
                        2 => {
                            fn delimiter(ch: char) -> bool {
                                ch.is_ascii_punctuation() || ch.is_ascii_whitespace()
                            }
                            fn on_different_word(a: char, b: char) -> bool {
                                delimiter(a) || delimiter(b)
                            }

                            while 0 < s_col && s_col < cols {
                                let prev = lines[s_row].get(s_col - 1).unwrap().ch;
                                let curr = lines[s_row].get(s_col).unwrap().ch;
                                if on_different_word(prev, curr) {
                                    break;
                                }
                                s_col -= 1;
                            }
                            while e_col < cols - 1 {
                                let prev = lines[e_row].get(e_col).unwrap().ch;
                                let curr = lines[e_row].get(e_col + 1).unwrap().ch;
                                if on_different_word(prev, curr) {
                                    break;
                                }
                                e_col += 1;
                            }
                        }

                        // triple click (or more): line selection
                        _ => {
                            s_col = 0;
                            e_col = cols - 1;
                        }
                    }

                    let l = s_row * cols + s_col;
                    let r = e_row * cols + e_col;
                    if l <= r {
                        Some(Selection::Linear { left: l, right: r })
                    } else {
                        None
                    }
                }
            };

            if self.view.selection_range != new_selection_range {
                self.view.update_contents(|view| {
                    view.selection_range = new_selection_range;
                });
            }
        } else if self.view.selection_range.is_some() {
            self.view.update_contents(|view| {
                view.selection_range = None;
            });
        }

        false
    }

    pub fn draw(&mut self, surface: &mut glium::Frame) {
        self.view.draw(surface);
    }

    pub fn viewport(&self) -> Viewport {
        self.view.viewport()
    }

    /// このペインのシェルの現在の作業ディレクトリ（取れない環境では None）。
    pub fn pane_cwd(&self) -> Option<std::path::PathBuf> {
        self.terminal.cwd()
    }

    pub fn pane_location(&self) -> ShellLocation {
        self.terminal
            .location()
            .or_else(|| self.terminal.cwd().map(ShellLocation::Local))
            .or_else(|| std::env::current_dir().ok().map(ShellLocation::Local))
            .unwrap_or_else(|| ShellLocation::Local(PathBuf::from(".")))
    }

    pub fn take_clicked_file(&mut self) -> Option<PathBuf> {
        self.clicked_file.take()
    }

    pub fn take_gt_messages(&mut self) -> Vec<GtMessage> {
        self.terminal.take_gt_messages()
    }

    pub fn set_viewport(&mut self, new_viewport: Viewport) {
        log::debug!("viewport changed: {:?}", new_viewport);
        self.view.set_viewport(new_viewport);
        self.resize_buffer();
    }

    fn token_at(&self, row: usize, col: usize) -> Option<String> {
        let line = self.view.lines.get(row)?;

        // 列ごとの文字を作る（幅2の全角は2列ぶん占有、幅0は前のセルの続き）。
        let mut chars: Vec<char> = Vec::new();
        for cell in line.iter() {
            match cell.width {
                0 => {}
                w => {
                    chars.push(cell.ch);
                    for _ in 1..w {
                        chars.push('\0');
                    }
                }
            }
        }

        let clicked = *chars.get(col)?;
        if !is_link_token_char(clicked) {
            return None;
        }

        let mut start = col;
        while start > 0 && is_link_token_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = col;
        while end + 1 < chars.len() && is_link_token_char(chars[end + 1]) {
            end += 1;
        }

        let token: String = chars[start..=end].iter().filter(|c| **c != '\0').collect();
        // 末尾の句読点はリンク本体ではないことが多いので URL/パス共通で除く。
        Some(
            token
                .trim_end_matches(|c| matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '。' | '、'))
                .to_string(),
        )
    }

    /// ホバー時に手カーソルを出すか（stat しない軽い判定）。
    /// URL は常に。ファイルパスは Ctrl+クリックで開くので Ctrl 押下時だけ。
    fn should_show_link_pointer(&self, row: usize, col: usize) -> bool {
        let Some(token) = self.token_at(row, col) else {
            return false;
        };
        if token.starts_with("http://") || token.starts_with("https://") {
            return true;
        }
        looks_like_path(&token) && self.modifiers.control_key()
    }

    /// クリックでリンクを開く。URL は素のクリックで、ファイルは Ctrl+クリックのとき
    /// だけ（画面上のパスを普通にクリックしてプレビューが誤爆で開くのを防ぐ）。
    /// URL 判定を先にして、素のクリックでは stat しない。
    fn handle_link_click(&mut self, row: usize, col: usize, ctrl: bool) {
        let Some(token) = self.token_at(row, col) else {
            return;
        };
        if token.starts_with("http://") || token.starts_with("https://") {
            open_url(&token);
            return;
        }
        if ctrl {
            let cwd = self
                .terminal
                .cwd()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            if let Some(path) = resolve_existing_file_token(&token, &cwd) {
                self.clicked_file = Some(path);
            }
        }
    }

    fn increase_font_size(&mut self, size_diff: i32) {
        self.view.increase_font_size(size_diff);
        self.resize_buffer();
    }

    fn resize_buffer(&mut self) {
        self.mouse.pressed_pos = None;
        self.mouse.released_pos = None;

        let viewport = self.view.viewport();

        let scroll_bar_width = crate::TOYTERM_CONFIG.scroll_bar_width;
        let width = viewport.w.saturating_sub(scroll_bar_width);

        let cell_size = self.view.cell_size();
        let rows = (viewport.h / cell_size.h) as usize;
        let cols = (width / cell_size.w) as usize;
        let buff_size = TerminalSize {
            rows: rows.max(1),
            cols: cols.max(1),
        };
        self.terminal.resize(
            buff_size.cols,
            buff_size.rows,
            cell_size.w as u16,
            cell_size.h as u16,
        );
    }

    pub fn focus_changed(&mut self, gain: bool) {
        self.focused = gain;

        // Update cursor
        self.view.update_contents(|view| {
            view.view_focused = self.focused;
        });

        if gain {
            self.refresh_cursor_icon();
        }
    }

    /// IME 候補ウィンドウの表示位置を、現在のカーソルセルに合わせて更新する。
    fn update_ime_position(&self) {
        if let Some(cursor) = self.view.cursor {
            let cell_size = self.view.cell_size();
            let vp = self.viewport();
            self.window.set_ime_cursor_area(
                PhysicalPosition::new(
                    vp.x + cursor.col as u32 * cell_size.w,
                    vp.y + cursor.row as u32 * cell_size.h,
                ),
                PhysicalSize::new(cell_size.w, cell_size.h),
            );
        }
    }

    /// マネージャから渡されるウィンドウイベントを処理する。
    /// CloseRequested / Resized / RedrawRequested / AboutToWait といった
    /// ウィンドウ全体の制御はマネージャ側が持ち、ここでは扱わない。
    pub fn process_window_event(&mut self, event: &WindowEvent) {
        match event {
            &WindowEvent::Focused(gain) => self.focus_changed(gain),

            WindowEvent::ModifiersChanged(new_states) => {
                self.modifiers = new_states.state();
            }

            // IME（日本語入力など）の状態を処理する。
            WindowEvent::Ime(ime) => match ime {
                // 変換中の未確定文字列。カーソル位置にインライン表示する。
                Ime::Preedit(text, _) => {
                    let text = text.clone();
                    self.view.update_contents(|view| view.preedit = text);
                    // 変換中は内容更新が起きないので、ここで候補位置を更新する
                    self.update_ime_position();
                }
                // 確定した文字列を PTY に流し、変換中表示を消す。
                Ime::Commit(text) => {
                    self.terminal.write(text.as_bytes());
                    self.view.update_contents(|view| view.preedit.clear());
                    // 確定に使った Enter がこの直後にキー入力として来ても
                    // 改行を送らないよう、確定時刻を記録しておく。
                    self.last_ime_commit = std::time::Instant::now();
                }
                Ime::Enabled | Ime::Disabled => {
                    self.view.update_contents(|view| view.preedit.clear());
                    self.update_ime_position();
                }
            },

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.on_key_press(event);
            }

            WindowEvent::CursorMoved { position, .. } => {
                let viewport = self.viewport();
                let x = position.x - viewport.x as f64;
                let y = position.y - viewport.y as f64;
                self.mouse.cursor_pos = CursorPosition { x, y };

                // リンクの上ではポインタ（手）カーソルにして「クリックできる」と
                // 分かるようにする。URL は常に、ファイルパスは Ctrl 押下時のみ。
                // mouse_mode のアプリにはマウスを渡すので変えない。
                if !self.terminal.mouse_mode() {
                    let cs = self.view.cell_size();
                    let col = (x / cs.w.max(1) as f64) as i64;
                    let row = (y / cs.h.max(1) as f64) as i64;
                    let on_link = col >= 0
                        && row >= 0
                        && self.should_show_link_pointer(row as usize, col as usize);
                    self.window.set_cursor_icon(if on_link {
                        CursorIcon::Pointer
                    } else {
                        CursorIcon::Text
                    });
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let is_inner = {
                    let viewport = self.viewport();
                    let (w, h) = (viewport.w as f64, viewport.h as f64);
                    let CursorPosition { x, y } = self.mouse.cursor_pos;
                    0.0 <= x && x < w && 0.0 <= y && y < h
                };

                if !is_inner {
                    self.mouse.pressed_pos = None;
                    self.mouse.released_pos = None;
                    self.mouse.selecting = false;
                    return;
                }

                // Shift 押下中はマウス報告を無視してローカル選択に回す
                // （xterm の作法）。これで Claude Code 等のマウス報告アプリでも
                // Shift+ドラッグで画面の文字を選択 → Ctrl+Shift+C でコピーできる。
                // Released は「ドラッグ開始時にローカル選択だったか(selecting)」も見る。
                // 途中で Shift を離してもボタンを離すまでローカル選択を続け、
                // released_pos を必ず立てる（立たないと離した後も選択がマウスに
                // 追従して固定できない）。
                let report_to_app = self.terminal.mouse_mode() && !self.modifiers.shift_key();
                let report = match state {
                    ElementState::Pressed => report_to_app,
                    ElementState::Released => report_to_app && !self.mouse.selecting,
                };
                if report {
                    self.mouse.selecting = false;
                    let button = match state {
                        ElementState::Released if !self.terminal.sgr_mouse() => 3,
                        _ => match button {
                            MouseButton::Left => 0,
                            MouseButton::Middle => 1,
                            MouseButton::Right => 2,
                            MouseButton::Back | MouseButton::Forward => 0,
                            MouseButton::Other(button_id) => {
                                // FIXME : Support multi button mouse?
                                log::warn!("unknown mouse button : {}", button_id);
                                0
                            }
                        },
                    };

                    #[rustfmt::skip]
                        let mods =
                            if self.modifiers.shift_key()   { 0b00000100 } else { 0 }
                        |   if self.modifiers.alt_key()     { 0b00001000 } else { 0 }
                        |   if self.modifiers.control_key() { 0b00010000 } else { 0 };

                    let CursorPosition { x, y } = self.mouse.cursor_pos;
                    let cell_size = self.view.cell_size();
                    let col = x.round() as u32 / cell_size.w + 1;
                    let row = y.round() as u32 / cell_size.h + 1;

                    if self.terminal.sgr_mouse() {
                        self.sgr_ext_mouse_report(button + mods, col, row, state);
                    } else {
                        self.normal_mouse_report(button + mods, col, row);
                    }
                } else {
                    match state {
                        ElementState::Pressed => {
                            const CLICK_INTERVAL: std::time::Duration =
                                std::time::Duration::from_millis(400);
                            if self.mouse.last_clicked.elapsed() > CLICK_INTERVAL {
                                self.mouse.click_count = 0;
                            }

                            self.mouse.click_count += 1;
                            self.mouse.last_clicked = std::time::Instant::now();
                            log::debug!("clicked {} times", self.mouse.click_count);

                            self.mouse.pressed_pos = Some(self.mouse.cursor_pos);
                            self.mouse.released_pos = None;
                            // 選択をスクロールに追従させるため押下時のスクロール量を記録。
                            self.mouse.pressed_offset = self.terminal.display_offset() as i64;
                            self.mouse.released_offset = self.mouse.pressed_offset;
                            // Ctrl を押しながらの開始は矩形選択。
                            self.mouse.block = self.modifiers.control_key();
                            // このドラッグはローカル選択。離すまで継続する。
                            self.mouse.selecting = true;
                        }
                        ElementState::Released => {
                            self.mouse.selecting = false;
                            self.mouse.released_pos = Some(self.mouse.cursor_pos);
                            self.mouse.released_offset = self.terminal.display_offset() as i64;

                            // ドラッグ（選択）でない単純な左クリック。URL は素のクリックで
                            // 開き、ファイルは Ctrl+クリックのときだけ開く（handle_link_click
                            // 内で判定）。mouse_mode が ON のアプリ（nvim 等）はここに来ない。
                            if *button == MouseButton::Left {
                                if let Some(press) = self.mouse.pressed_pos {
                                    let cs = self.view.cell_size();
                                    let to_cell = |p: CursorPosition| {
                                        (
                                            (p.x / cs.w.max(1) as f64) as i64,
                                            (p.y / cs.h.max(1) as f64) as i64,
                                        )
                                    };
                                    let here = self.mouse.cursor_pos;
                                    if to_cell(press) == to_cell(here) {
                                        let (col, row) = to_cell(here);
                                        if col >= 0 && row >= 0 {
                                            let ctrl = self.modifiers.control_key();
                                            self.handle_link_click(
                                                row as usize,
                                                col as usize,
                                                ctrl,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // マウスホイールは行単位(LineDelta)、ノートPCのタッチパッドは
                // ピクセル単位(PixelDelta)で来る。後者はセルサイズで行数に換算する。
                let cell_size = self.view.cell_size();
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (*x * 1.5, *y * 1.5),
                    // タッチパッド等のピクセル単位スクロールはセルサイズで行数に換算。
                    // winit は LineDelta も PixelDelta も同じ符号規約(正=上)なので、
                    // 反転せずマウスホイールと同じ向きに揃える。
                    MouseScrollDelta::PixelDelta(pos) => (
                        pos.x as f32 / cell_size.w.max(1) as f32,
                        pos.y as f32 / cell_size.h.max(1) as f32,
                    ),
                };

                self.mouse.wheel_delta_x += dx;
                self.mouse.wheel_delta_y += dy;

                let horizontal = self.mouse.wheel_delta_x.trunc() as isize;
                let vertical = self.mouse.wheel_delta_y.trunc() as isize;

                self.mouse.wheel_delta_x %= 1.0;
                self.mouse.wheel_delta_y %= 1.0;

                // スクロールの振り分け：
                //  ・Shift 押下 → 常に履歴スクロール（どんな画面でも遡れる保険）
                //  ・アプリがマウス報告中(Claude Code 等の TUI) → ホイールを
                //    マウスホイールイベントとして送り、アプリ自身にスクロールさせる。
                //    （以前は矢印キーを送っていたため、Claude Code が入力履歴↑↓と
                //     誤解してページが動かなかった）
                //  ・代替画面(マウス非対応の less 等) → 矢印キー
                //  ・通常画面 → ローカル履歴スクロール
                if self.modifiers.shift_key() {
                    self.terminal.scroll(vertical as i32);
                } else if self.terminal.mouse_mode() {
                    let cell_size = self.view.cell_size();
                    let CursorPosition { x, y } = self.mouse.cursor_pos;
                    let col = x.round().max(0.0) as u32 / cell_size.w.max(1) + 1;
                    let row = y.round().max(0.0) as u32 / cell_size.h.max(1) + 1;
                    let sgr = self.terminal.sgr_mouse();
                    // 64=ホイール上, 65=下, 66=左, 67=右
                    let v_btn: u8 = if vertical > 0 { 64 } else { 65 };
                    for _ in 0..vertical.abs() {
                        if sgr {
                            self.sgr_ext_mouse_report(v_btn, col, row, &ElementState::Pressed);
                        } else {
                            self.normal_mouse_report(v_btn, col, row);
                        }
                    }
                    let h_btn: u8 = if horizontal > 0 { 67 } else { 66 };
                    for _ in 0..horizontal.abs() {
                        if sgr {
                            self.sgr_ext_mouse_report(h_btn, col, row, &ElementState::Pressed);
                        } else {
                            self.normal_mouse_report(h_btn, col, row);
                        }
                    }
                } else if self.terminal.alt_screen() {
                    let vk: &[u8] = if vertical > 0 {
                        b"\x1b[\x41"
                    } else {
                        b"\x1b[\x42"
                    };
                    for _ in 0..vertical.abs() {
                        self.terminal.write(vk);
                    }
                    let hk: &[u8] = if horizontal > 0 {
                        b"\x1b[\x43"
                    } else {
                        b"\x1b[\x44"
                    };
                    for _ in 0..horizontal.abs() {
                        self.terminal.write(hk);
                    }
                } else {
                    self.terminal.scroll(vertical as i32);
                    let hk: &[u8] = if horizontal > 0 {
                        b"\x1b[\x43"
                    } else {
                        b"\x1b[\x44"
                    };
                    for _ in 0..horizontal.abs() {
                        self.terminal.write(hk);
                    }
                }
            }

            _ => {}
        }
    }

    fn on_key_press(&mut self, key_event: &KeyEvent) {
        // Ctrl+英字を制御コード(0x01..=0x1A)へ。ReceivedCharacter 廃止の代替。
        fn ctrl_letter_code(code: KeyCode) -> Option<u8> {
            use KeyCode::*;
            let n: u8 = match code {
                KeyA => 1,
                KeyB => 2,
                KeyC => 3,
                KeyD => 4,
                KeyE => 5,
                KeyF => 6,
                KeyG => 7,
                KeyH => 8,
                KeyI => 9,
                KeyJ => 10,
                KeyK => 11,
                KeyL => 12,
                KeyM => 13,
                KeyN => 14,
                KeyO => 15,
                KeyP => 16,
                KeyQ => 17,
                KeyR => 18,
                KeyS => 19,
                KeyT => 20,
                KeyU => 21,
                KeyV => 22,
                KeyW => 23,
                KeyX => 24,
                KeyY => 25,
                KeyZ => 26,
                _ => return None,
            };
            Some(n)
        }

        let keycode = match key_event.physical_key {
            PhysicalKey::Code(code) => code,
            PhysicalKey::Unidentified(_) => return,
        };

        // IME 変換中のキーは IME に任せ、端末へ送らない。
        if !self.view.preedit.is_empty() {
            return;
        }
        // IME 確定(Commit)とほぼ同時に来る確定 Enter は端末へ送らない
        // （Windows で確定 Enter が Commit とキー入力の両方で来るため。
        // ユーザが改めて押す本物の Enter は時間が空くので影響しない）。
        if keycode == KeyCode::Enter
            && self.last_ime_commit.elapsed() < std::time::Duration::from_millis(50)
        {
            return;
        }

        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();

        // normally text selection is cleared when user types something,
        // but there are some exceptions. history_head is cleared too.
        let mut clear = true;

        // 制御シーケンスを送る特殊キーを先に処理。handled=false なら
        // 通常文字として KeyEvent.text をそのまま PTY に流す。
        let mut handled = true;
        match (ctrl, shift, keycode) {
            (false, _, KeyCode::Escape) => {
                self.mouse.pressed_pos = None;
                self.mouse.released_pos = None;
                self.terminal.write(b"\x1B");
            }

            (true, false, KeyCode::Minus) => self.increase_font_size(-1),
            (true, false, KeyCode::Equal) => self.increase_font_size(1),

            // Backspace: send DEL instead of BS
            (false, _, KeyCode::Backspace) => self.terminal.write(b"\x7f"),
            (false, _, KeyCode::Delete) => self.terminal.write(b"\x1b[3~"),

            // Shift+Enter は ESC+CR を送る。Claude Code 等の TUI はこれを
            // 「送信せず改行」として扱う（/terminal-setup が設定するのと同じ）。
            (false, true, KeyCode::Enter) => self.terminal.write(b"\x1b\r"),
            (false, false, KeyCode::Enter) => self.terminal.write(b"\r"),
            (false, _, KeyCode::Tab) => self.terminal.write(b"\t"),

            // Space は明示的に空白を送る。IME 有効時に winit が text=None で
            // Space を渡してくることがあり、その場合 text 経由だと何も送られず、
            // Claude Code の選択(スペースでトグル)等が効かなくなるため。
            // ここに来る時点で preedit は空（上でガード済み）なので変換中は影響しない。
            (false, _, KeyCode::Space) => self.terminal.write(b" "),

            (false, _, KeyCode::ArrowUp) => self.terminal.write(b"\x1b[\x41"),
            (false, _, KeyCode::ArrowDown) => self.terminal.write(b"\x1b[\x42"),
            (false, _, KeyCode::ArrowRight) => self.terminal.write(b"\x1b[\x43"),
            (false, _, KeyCode::ArrowLeft) => self.terminal.write(b"\x1b[\x44"),

            (false, _, KeyCode::PageUp) => self.terminal.write(b"\x1b[5~"),
            (false, _, KeyCode::PageDown) => self.terminal.write(b"\x1b[6~"),

            // Ctrl+Shift+C/V: コピー・ペースト。履歴クリアは Ctrl+Shift+Delete
            //（Ctrl+Shift+L はペインのフォーカス移動＝右 に使うため移設した）。
            (true, true, KeyCode::KeyC) => {
                clear = false;
                self.copy_clipboard();
            }
            (true, true, KeyCode::KeyV) => self.paste_clipboard(),
            (true, true, KeyCode::Delete) => {
                self.terminal.clear_history();
            }

            (false, _, KeyCode::F1) => self.terminal.write(b"\x1BOP"),
            (false, _, KeyCode::F2) => self.terminal.write(b"\x1BOQ"),
            (false, _, KeyCode::F3) => self.terminal.write(b"\x1BOR"),
            (false, _, KeyCode::F4) => self.terminal.write(b"\x1BOS"),
            (false, _, KeyCode::F5) => self.terminal.write(b"\x1B[15~"),
            (false, _, KeyCode::F6) => self.terminal.write(b"\x1B[17~"),
            (false, _, KeyCode::F7) => self.terminal.write(b"\x1B[18~"),
            (false, _, KeyCode::F8) => self.terminal.write(b"\x1B[19~"),
            (false, _, KeyCode::F9) => self.terminal.write(b"\x1B[20~"),
            (false, _, KeyCode::F10) => self.terminal.write(b"\x1B[21~"),
            (false, _, KeyCode::F11) => self.terminal.write(b"\x1B[23~"),
            (false, _, KeyCode::F12) => self.terminal.write(b"\x1B[24~"),

            // Ctrl+英字（Shiftなし）は制御コードへ。Ctrl+C/L/V もここで処理。
            (true, false, code) => match ctrl_letter_code(code) {
                Some(b) => self.terminal.write(&[b]),
                None => handled = false,
            },

            _ => handled = false,
        }

        if !handled {
            match &key_event.text {
                // 通常文字（英数字・記号・全角等の非IME入力）をそのまま送る
                Some(text) => self.terminal.write(text.as_bytes()),
                // 修飾キー単体などテキストを生まないキーでは選択を消さない
                None => clear = false,
            }
        }

        if clear {
            self.view.update_contents(|view| {
                view.selection_range = None;
            });

            self.mouse.pressed_pos = None;
            self.mouse.released_pos = None;

            // 実際の入力をしたらスクロールバックを最下部に戻す
            // （履歴を見たまま打って迷子になるのを防ぐ）。コピー等の
            // clear=false のキーでは戻さないので、履歴からのコピーは可能。
            self.terminal.scroll_to_bottom();
        }
    }

    fn copy_clipboard(&mut self) {
        let mut text = String::new();

        match self.view.selection_range {
            None => {}

            // 通常選択：行方向の連続範囲。
            Some(Selection::Linear { left, right }) => {
                'row: for (i, row) in self.view.lines.iter().enumerate() {
                    let cols = row.columns();
                    for (j, cell) in row.iter().enumerate() {
                        if cell.width == 0 {
                            continue;
                        }
                        let offset = i * cols + j;
                        let center = offset + (cell.width / 2) as usize;
                        if left <= center && center <= right {
                            text.push(cell.ch);
                        }
                        if cell.ch == '\n' {
                            continue 'row;
                        }
                    }
                    if !row.linewrap() {
                        let offset = (i + 1) * cols;
                        if left < offset && offset <= right {
                            // 行末の余分な空白は貼り付け先で邪魔になるので落とす。
                            let n = text.trim_end_matches([' ', '\t']).len();
                            text.truncate(n);
                            text.push('\n');
                        }
                    }
                }
                text = dedent_common_indent(&text);
            }

            // 矩形選択：各行の列範囲 [left, right] を取り、行間に改行を入れる。
            Some(Selection::Block {
                top,
                bottom,
                left,
                right,
            }) => {
                for (i, row) in self.view.lines.iter().enumerate() {
                    if i < top || i > bottom {
                        continue;
                    }
                    let mut line = String::new();
                    for (j, cell) in row.iter().enumerate() {
                        if cell.width == 0 {
                            continue;
                        }
                        if left <= j && j <= right {
                            line.push(cell.ch);
                        }
                    }
                    // 矩形の右側にできる余分な空白を行ごとに落とす。
                    text.push_str(line.trim_end_matches([' ', '\t']));
                    if i != bottom {
                        text.push('\n');
                    }
                }
            }
        }

        // 末尾行の余分な空白も落とす（改行は残す）。
        let n = text.trim_end_matches([' ', '\t']).len();
        text.truncate(n);

        log::info!("copy: {:?}", text);
        set_clipboard(&text);
    }

    fn paste_clipboard(&mut self) {
        let text = get_clipboard();
        log::debug!("paste: {:?}", text);
        if self.terminal.bracketed_paste() {
            self.terminal.write(b"\x1b[200~");
            self.terminal.write(text.as_bytes());
            self.terminal.write(b"\x1b[201~");
        } else {
            self.terminal.write(text.as_bytes());
        }
    }

    fn normal_mouse_report(&mut self, button: u8, col: u32, row: u32) {
        let col = if 0 < col && col < 224 { col + 32 } else { 0 } as u8;
        let row = if 0 < row && row < 224 { row + 32 } else { 0 } as u8;

        let msg = [b'\x1b', b'[', b'M', 32 + button, col, row];

        self.terminal.write(&msg);
    }

    fn sgr_ext_mouse_report(&mut self, button: u8, col: u32, row: u32, state: &ElementState) {
        let m = match state {
            ElementState::Pressed => 'M',
            ElementState::Released => 'm',
        };

        self.terminal
            .write(format!("\x1b[<{button};{col};{row}{m}").as_bytes());
    }
}

fn dedent_common_indent(text: &str) -> String {
    fn is_blank(line: &str) -> bool {
        line.chars().all(|ch| matches!(ch, ' ' | '\t'))
    }

    fn leading_indent(line: &str) -> &str {
        let end = line
            .char_indices()
            .find_map(|(i, ch)| (!matches!(ch, ' ' | '\t')).then_some(i))
            .unwrap_or(line.len());
        &line[..end]
    }

    fn common_prefix(left: &str, right: &str) -> String {
        let mut end = 0;
        for ((i, a), b) in left.char_indices().zip(right.chars()) {
            if a != b {
                break;
            }
            end = i + a.len_utf8();
        }
        left[..end].to_string()
    }

    let mut common: Option<String> = None;
    for line in text.split('\n').filter(|line| !is_blank(line)) {
        let indent = leading_indent(line);
        common = Some(match common {
            Some(ref current) => common_prefix(current, indent),
            None => indent.to_string(),
        });
    }

    let common = common.unwrap_or_default();
    text.split('\n')
        .map(|line| {
            if is_blank(line) {
                ""
            } else {
                line.strip_prefix(&common).unwrap_or(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{dedent_common_indent, resolve_existing_file_token, resolve_path_token};
    use std::path::{Path, PathBuf};

    #[test]
    fn dedent_common_indent_removes_shared_prefix() {
        let cases = [
            ("  a\n  b", "a\nb"),
            ("  a\n    b", "a\n  b"),
            ("  a\n\n  b", "a\n\nb"),
            ("  a\n   \n  b", "a\n\nb"),
            ("a\n  b", "a\n  b"),
            ("   hello", "hello"),
            ("\tx\n\ty", "x\ny"),
            ("  a\n  b\n", "a\nb\n"),
            ("", ""),
        ];

        for (input, expected) in cases {
            assert_eq!(dedent_common_indent(input), expected);
        }
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("toyterm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn resolves_absolute_path_token() {
        let dir = test_dir("abs");
        let file = dir.join("note.txt");
        std::fs::write(&file, "hello").expect("write file");

        assert_eq!(
            resolve_path_token(file.to_str().unwrap(), Path::new("/tmp")),
            Some(file)
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolves_home_path_token() {
        let dir = test_dir("home");
        let file = dir.join("note.txt");
        std::fs::write(&file, "hello").expect("write file");
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &dir);

        assert_eq!(
            resolve_path_token("~/note.txt", Path::new("/tmp")),
            Some(file)
        );

        if let Some(home) = old_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolves_relative_path_token_against_cwd() {
        let dir = test_dir("rel");
        std::fs::create_dir_all(dir.join("src")).expect("create src");
        let file = dir.join("src/main.rs");
        std::fs::write(&file, "fn main() {}").expect("write file");

        assert_eq!(resolve_path_token("src/./main.rs", &dir), Some(file));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn existing_file_resolution_ignores_missing_token() {
        let dir = test_dir("missing");

        assert_eq!(resolve_existing_file_token("missing.txt", &dir), None);

        let _ = std::fs::remove_dir_all(dir);
    }
}
