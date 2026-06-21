use std::rc::Rc;

use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::{CursorIcon, Window},
};

use crate::terminal::TerminalSize;
use crate::vt::VtTerminal;
use crate::view::{TerminalView, Viewport};
use crate::Display;

type CursorPosition = PhysicalPosition<f64>;

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
}

/// viewport とフォントから端末セル数を求めて VtTerminal を作る。
fn make_terminal(view: &TerminalView, viewport: Viewport, cwd: &std::path::Path) -> VtTerminal {
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
        &crate::TOYTERM_CONFIG.shell,
    )
}

struct MouseState {
    wheel_delta_x: f32,
    wheel_delta_y: f32,
    cursor_pos: CursorPosition,
    pressed_pos: Option<CursorPosition>,
    released_pos: Option<CursorPosition>,
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
        let font_size = crate::TOYTERM_CONFIG.font_size;
        let view = TerminalView::with_viewport(
            display,
            viewport,
            font_size,
            Some((0, viewport.h)),
        );

        let terminal = {
            let parent_cwd = std::env::current_dir().expect("cwd");
            let child_cwd = cwd.unwrap_or(&parent_cwd);
            make_terminal(&view, viewport, child_cwd)
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
                click_count: 0,
                last_clicked: std::time::Instant::now() - std::time::Duration::from_secs(10),
            },
            last_ime_commit: std::time::Instant::now() - std::time::Duration::from_secs(10),
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

        // Update text selection
        if let Some(CursorPosition { x: sx, y: sy }) = self.mouse.pressed_pos {
            let CursorPosition { x: ex, y: ey } =
                self.mouse.released_pos.unwrap_or(self.mouse.cursor_pos);

            let lines = &self.view.lines;

            let x_max = cell_size.w as f64 * terminal_size.cols as f64;
            let y_max = cell_size.h as f64 * terminal_size.rows as f64;
            let sx = sx.clamp(0.0, x_max - 0.1);
            let sy = sy.clamp(0.0, y_max - 0.1);
            let ex = ex.clamp(0.0, x_max - 0.1);
            let ey = ey.clamp(0.0, y_max - 0.1);

            let mut s_row = (sy / cell_size.h as f64).floor() as usize;
            let mut s_col = (sx / cell_size.w as f64).round() as usize;
            let mut e_row = (ey / cell_size.h as f64).floor() as usize;
            let mut e_col = (ex / cell_size.w as f64).round() as usize;

            if (e_row, e_col) < (s_row, s_col) {
                std::mem::swap(&mut s_row, &mut e_row);
                std::mem::swap(&mut s_col, &mut e_col);
            }

            // NOTE: selecton is closed range [s, e]
            e_col = e_col.saturating_sub(1);

            match self.mouse.click_count {
                // single click: character selection
                1 => {
                    // nothing to do
                }

                // double click: word selection
                2 => {
                    fn delimiter(ch: char) -> bool {
                        ch.is_ascii_punctuation() || ch.is_ascii_whitespace()
                    }
                    fn on_different_word(a: char, b: char) -> bool {
                        delimiter(a) || delimiter(b)
                    }

                    while 0 < s_col && s_col < terminal_size.cols {
                        let prev = lines[s_row].get(s_col - 1).unwrap().ch;
                        let curr = lines[s_row].get(s_col).unwrap().ch;
                        if on_different_word(prev, curr) {
                            break;
                        }
                        s_col -= 1;
                    }
                    while e_col < terminal_size.cols - 1 {
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
                    e_col = terminal_size.cols - 1;
                }
            }

            let l = s_row * terminal_size.cols + s_col;
            let r = e_row * terminal_size.cols + e_col;
            let new_selection_range = if l <= r { Some((l, r)) } else { None };

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

    pub fn set_viewport(&mut self, new_viewport: Viewport) {
        log::debug!("viewport changed: {:?}", new_viewport);
        self.view.set_viewport(new_viewport);
        self.resize_buffer();
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

                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed =>
                {
                    self.on_key_press(event);
                }

                WindowEvent::CursorMoved { position, .. } => {
                    let viewport = self.viewport();
                    let x = position.x - viewport.x as f64;
                    let y = position.y - viewport.y as f64;
                    self.mouse.cursor_pos = CursorPosition { x, y };
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
                        return;
                    }

                    if self.terminal.mouse_mode() {
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
                            }
                            ElementState::Released => {
                                self.mouse.released_pos = Some(self.mouse.cursor_pos);
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

                    // 通常画面ではホイールで履歴スクロール（直感的）。
                    // 代替画面(nvim/less 等の TUI)では矢印キーを送って中身をスクロール。
                    // Shift 押下時は常に履歴スクロール。
                    if self.modifiers.shift_key() || !self.terminal.alt_screen() {
                        self.terminal.scroll(vertical as i32);
                    } else {
                        // Send Up/Down key
                        if vertical > 0 {
                            for _ in 0..vertical.abs() {
                                self.terminal.write(b"\x1b[\x41"); // Up
                            }
                        } else {
                            for _ in 0..vertical.abs() {
                                self.terminal.write(b"\x1b[\x42"); // Down
                            }
                        }
                    }

                    if horizontal > 0 {
                        for _ in 0..horizontal.abs() {
                            self.terminal.write(b"\x1b[\x43"); // Right
                        }
                    } else {
                        for _ in 0..horizontal.abs() {
                            self.terminal.write(b"\x1b[\x44"); // Left
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
                KeyA => 1, KeyB => 2, KeyC => 3, KeyD => 4, KeyE => 5,
                KeyF => 6, KeyG => 7, KeyH => 8, KeyI => 9, KeyJ => 10,
                KeyK => 11, KeyL => 12, KeyM => 13, KeyN => 14, KeyO => 15,
                KeyP => 16, KeyQ => 17, KeyR => 18, KeyS => 19, KeyT => 20,
                KeyU => 21, KeyV => 22, KeyW => 23, KeyX => 24, KeyY => 25,
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

            (false, _, KeyCode::ArrowUp) => self.terminal.write(b"\x1b[\x41"),
            (false, _, KeyCode::ArrowDown) => self.terminal.write(b"\x1b[\x42"),
            (false, _, KeyCode::ArrowRight) => self.terminal.write(b"\x1b[\x43"),
            (false, _, KeyCode::ArrowLeft) => self.terminal.write(b"\x1b[\x44"),

            (false, _, KeyCode::PageUp) => self.terminal.write(b"\x1b[5~"),
            (false, _, KeyCode::PageDown) => self.terminal.write(b"\x1b[6~"),

            // Ctrl+Shift+C/V/L: コピー・ペースト・履歴クリア
            (true, true, KeyCode::KeyC) => {
                clear = false;
                self.copy_clipboard();
            }
            (true, true, KeyCode::KeyV) => self.paste_clipboard(),
            (true, true, KeyCode::KeyL) => {
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

        let selection_range = self.view.selection_range;

        'row: for (i, row) in self.view.lines.iter().enumerate() {
            let cols = row.columns();

            for (j, cell) in row.iter().enumerate() {
                if cell.width == 0 {
                    continue;
                }

                let is_selected = match selection_range {
                    Some((left, right)) => {
                        let offset = i * cols + j;
                        let center = offset + (cell.width / 2) as usize;
                        left <= center && center <= right
                    }
                    None => false,
                };

                if is_selected {
                    text.push(cell.ch);
                }

                if cell.ch == '\n' {
                    continue 'row;
                }
            }

            if !row.linewrap() {
                let is_selected = match selection_range {
                    Some((left, right)) => {
                        let offset = (i + 1) * cols;
                        left < offset && offset <= right
                    }
                    None => false,
                };
                if is_selected {
                    text.push('\n');
                }
            }
        }

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

