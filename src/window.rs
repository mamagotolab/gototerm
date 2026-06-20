use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ControlFlow, EventLoopWindowTarget},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::{CursorIcon, Window},
};

use crate::terminal::{Mode, Terminal, TerminalSize};
use crate::view::{TerminalView, Viewport};
use crate::Display;

type Event = winit::event::Event<()>;
type CursorPosition = PhysicalPosition<f64>;

pub struct TerminalWindow {
    window: Window,
    display: Display,
    terminal: Terminal,
    clipboard: arboard::Clipboard,

    view: TerminalView,
    mode: Mode,
    history_head: isize,
    last_history_head: isize,
    focused: bool,
    modifiers: ModifiersState,
    mouse: MouseState,
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
    #[allow(unused)]
    pub fn new(window: Window, display: Display, cwd: Option<&std::path::Path>) -> Self {
        let size = window.inner_size();
        let full = Viewport {
            x: 0,
            y: 0,
            w: size.width,
            h: size.height,
        };
        Self::with_viewport(window, display, full, cwd)
    }

    pub fn with_viewport(
        window: Window,
        display: Display,
        viewport: Viewport,
        cwd: Option<&std::path::Path>,
    ) -> Self {
        let font_size = crate::TOYTERM_CONFIG.font_size;
        let view = TerminalView::with_viewport(
            display.clone(),
            viewport,
            font_size,
            Some((0, viewport.h)),
        );

        let terminal = {
            let cell_size = view.cell_size();
            let scroll_bar_width = crate::TOYTERM_CONFIG.scroll_bar_width;
            let size = TerminalSize {
                rows: (viewport.h / cell_size.h) as usize,
                cols: ((viewport.w - scroll_bar_width) / cell_size.w) as usize,
            };
            let parent_cwd = std::env::current_dir().expect("cwd");
            let child_cwd = cwd.unwrap_or(&parent_cwd);
            Terminal::new(size, cell_size, child_cwd)
        };

        // Use I-beam mouse cursor
        window.set_cursor_icon(CursorIcon::Text);

        // 日本語入力（IME）を有効化する。これを呼ばないと winit が
        // text-input-v3 を enable せず、fcitx5 等に入力が渡らない。
        window.set_ime_allowed(true);

        TerminalWindow {
            window,
            display,
            terminal,
            clipboard: arboard::Clipboard::new().expect("clipboard"),

            view,
            mode: Mode::default(),
            history_head: 0,
            last_history_head: 0,
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
        }
    }

    pub fn reset_pty(&mut self) -> Option<i32> {
        let last_status = self.terminal.exit_status();

        if last_status.is_none() {
            self.terminal.send_sigterm();
        }

        self.terminal = {
            let viewport = self.view.viewport();
            let cell_size = self.view.cell_size();
            let scroll_bar_width = crate::TOYTERM_CONFIG.scroll_bar_width;
            let size = TerminalSize {
                rows: (viewport.h / cell_size.h) as usize,
                cols: ((viewport.w - scroll_bar_width) / cell_size.w) as usize,
            };
            let cwd = std::env::current_dir().expect("cwd");
            Terminal::new(size, cell_size, &cwd)
        };

        // Invalidate rendering cache
        self.view.update_contents(|_| {});

        last_status
    }

    pub fn close_pty(&mut self) {
        self.terminal.send_sigterm();
    }

    // Change cursor icon according to the current mouse_track mode
    pub fn refresh_cursor_icon(&mut self) {
        let icon = if self.mode.mouse_track {
            CursorIcon::Default
        } else {
            CursorIcon::Text
        };
        self.window.set_cursor_icon(icon);
    }

    // Returns true if the PTY is closed, false otherwise
    fn check_update(&mut self) -> bool {
        let cell_size = self.view.cell_size();

        let contents_updated: bool;
        let mouse_track_mode_changed: bool;
        let terminal_size: TerminalSize;
        {
            // hold the lock while copying states
            let mut state = self.terminal.state.lock().unwrap();

            if state.exit_status.is_some() {
                return true;
            }

            mouse_track_mode_changed = self.mode.mouse_track != state.mode().mouse_track;
            self.mode = state.mode();

            contents_updated = state.updated || self.last_history_head != self.history_head;
            self.last_history_head = self.history_head;

            terminal_size = state.size();

            if contents_updated {
                // update scroll bar
                let scroll_bar_position = {
                    let hist_rows = state.history_size();
                    let rows = state.size().rows;
                    let viewport_height = self.viewport().h;

                    let total = hist_rows + rows;
                    let r = (hist_rows as isize + self.history_head) as f64 / total as f64;
                    let origin = (viewport_height as f64 * r) as u32;
                    let length = ((viewport_height as f64) * rows as f64 / total as f64) as u32;
                    Some((origin, length))
                };

                let mut lines = Vec::new();
                self.view
                    .update_contents(|view| std::mem::swap(&mut view.lines, &mut lines));

                {
                    let top = self.history_head;
                    let bot = top + terminal_size.rows as isize;

                    if lines.len() == terminal_size.rows {
                        // Copy lines w/o heap allocation
                        for (src, dst) in state.range(top, bot).zip(lines.iter_mut()) {
                            dst.copy_from(src);
                        }
                    } else {
                        // Copy lines w/ heap allocation
                        lines.clear();
                        lines.extend(state.range(top, bot).cloned());
                    }
                }

                let images = state
                    .images()
                    .cloned()
                    .map(|mut img| {
                        img.row -= self.history_head;
                        img
                    })
                    .collect();

                let cursor = if self.history_head >= 0 && state.mode().cursor_visible {
                    let cursor = state.cursor();

                    // 変換候補ウィンドウをカーソルのセル位置に出すための矩形。
                    // これにより候補が入力中の文字を隠さない（over-the-spot）。
                    self.window.set_ime_cursor_area(
                        PhysicalPosition::new(
                            self.viewport().x + cursor.col as u32 * cell_size.w,
                            self.viewport().y + cursor.row as u32 * cell_size.h,
                        ),
                        PhysicalSize::new(cell_size.w, cell_size.h),
                    );

                    Some(cursor)
                } else {
                    None
                };

                self.view.update_contents(|view| {
                    view.lines = lines;
                    view.images = images;
                    view.cursor = cursor;
                    view.scroll_bar = scroll_bar_position;
                    view.view_focused = self.focused;
                });
            }

            state.updated = false;
        }

        if mouse_track_mode_changed {
            self.refresh_cursor_icon();
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
        self.terminal.request_resize(buff_size, cell_size);
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

    pub fn on_event(&mut self, event: &Event, elwt: &EventLoopWindowTarget<()>) {
        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    elwt.exit();
                }

                &WindowEvent::Focused(gain) => self.focus_changed(gain),

                &WindowEvent::Resized(new_size) => {
                    // glium 0.34 の手書きサーフェスは自動リサイズされない。
                    // ここで明示的に合わせないと、描画が初期サイズのまま潰れる。
                    self.display.resize((new_size.width, new_size.height));
                    let mut viewport = self.viewport();
                    viewport.w = new_size.width;
                    viewport.h = new_size.height;
                    self.set_viewport(viewport);
                }

                WindowEvent::ModifiersChanged(new_states) => {
                    self.modifiers = new_states.state();
                }

                // IME（日本語入力など）の状態を処理する。
                WindowEvent::Ime(ime) => match ime {
                    // 変換中の未確定文字列。カーソル位置にインライン表示する。
                    Ime::Preedit(text, _) => {
                        let text = text.clone();
                        self.view.update_contents(|view| view.preedit = text);
                    }
                    // 確定した文字列を PTY に流し、変換中表示を消す。
                    Ime::Commit(text) => {
                        self.terminal.pty_write(text.as_bytes());
                        self.view.update_contents(|view| view.preedit.clear());
                    }
                    Ime::Enabled | Ime::Disabled => {
                        self.view.update_contents(|view| view.preedit.clear());
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

                    if self.mode.mouse_track {
                        let button = match state {
                            ElementState::Released if !self.mode.sgr_ext_mouse_track => 3,
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

                        if self.mode.sgr_ext_mouse_track {
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

                WindowEvent::MouseWheel {
                    delta: MouseScrollDelta::LineDelta(dx, dy),
                    ..
                } => {
                    let mouse = &mut self.mouse;

                    mouse.wheel_delta_x += dx * 1.5;
                    mouse.wheel_delta_y += dy * 1.5;

                    let horizontal = mouse.wheel_delta_x.trunc() as isize;
                    let vertical = mouse.wheel_delta_y.trunc() as isize;

                    mouse.wheel_delta_x %= 1.0;
                    mouse.wheel_delta_y %= 1.0;

                    if self.modifiers.shift_key() {
                        // Scroll up history
                        let state = self.terminal.state.lock().unwrap();
                        let min = -(state.history_size() as isize);
                        self.history_head = (self.history_head - vertical).clamp(min, 0);
                    } else {
                        // Send Up/Down key
                        if vertical > 0 {
                            for _ in 0..vertical.abs() {
                                self.terminal.pty_write(b"\x1b[\x41"); // Up
                            }
                        } else {
                            for _ in 0..vertical.abs() {
                                self.terminal.pty_write(b"\x1b[\x42"); // Down
                            }
                        }
                    }

                    if horizontal > 0 {
                        for _ in 0..horizontal.abs() {
                            self.terminal.pty_write(b"\x1b[\x43"); // Right
                        }
                    } else {
                        for _ in 0..horizontal.abs() {
                            self.terminal.pty_write(b"\x1b[\x44"); // Left
                        }
                    }
                }

                WindowEvent::RedrawRequested => {
                    let mut surface = self.display.draw();
                    self.draw(&mut surface);
                    surface.finish().expect("finish");
                }

                _ => {}
            },

            Event::AboutToWait => {
                if self.check_update() {
                    elwt.exit();
                    return;
                }
                // 内容が変わったフレームだけ再描画を要求する。アイドル時は
                // スワップせず、ウィンドウが隠れていてもループが固まらない。
                if self.view.needs_redraw() {
                    self.window.request_redraw();
                }
                // 約16ms(=60fps)後に再びポーリングする。Poll の全力空転を避けつつ
                // PTY 出力を遅延なく拾い、ping にも定期的に応答できる。
                elwt.set_control_flow(ControlFlow::WaitUntil(
                    std::time::Instant::now() + std::time::Duration::from_millis(16),
                ));
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
                self.history_head = 0;
                self.mouse.pressed_pos = None;
                self.mouse.released_pos = None;
                self.terminal.pty_write(b"\x1B");
            }

            (true, false, KeyCode::Minus) => self.increase_font_size(-1),
            (true, false, KeyCode::Equal) => self.increase_font_size(1),

            // Backspace: send DEL instead of BS
            (false, _, KeyCode::Backspace) => self.terminal.pty_write(b"\x7f"),
            (false, _, KeyCode::Delete) => self.terminal.pty_write(b"\x1b[3~"),

            (false, _, KeyCode::Enter) => self.terminal.pty_write(b"\r"),
            (false, _, KeyCode::Tab) => self.terminal.pty_write(b"\t"),

            (false, _, KeyCode::ArrowUp) => self.terminal.pty_write(b"\x1b[\x41"),
            (false, _, KeyCode::ArrowDown) => self.terminal.pty_write(b"\x1b[\x42"),
            (false, _, KeyCode::ArrowRight) => self.terminal.pty_write(b"\x1b[\x43"),
            (false, _, KeyCode::ArrowLeft) => self.terminal.pty_write(b"\x1b[\x44"),

            (false, _, KeyCode::PageUp) => self.terminal.pty_write(b"\x1b[5~"),
            (false, _, KeyCode::PageDown) => self.terminal.pty_write(b"\x1b[6~"),

            // Ctrl+Shift+C/V/L: コピー・ペースト・履歴クリア
            (true, true, KeyCode::KeyC) => {
                clear = false;
                self.copy_clipboard();
            }
            (true, true, KeyCode::KeyV) => self.paste_clipboard(),
            (true, true, KeyCode::KeyL) => {
                self.history_head = 0;
                let mut state = self.terminal.state.lock().unwrap();
                state.clear_history();
            }

            (false, _, KeyCode::F1) => self.terminal.pty_write(b"\x1BOP"),
            (false, _, KeyCode::F2) => self.terminal.pty_write(b"\x1BOQ"),
            (false, _, KeyCode::F3) => self.terminal.pty_write(b"\x1BOR"),
            (false, _, KeyCode::F4) => self.terminal.pty_write(b"\x1BOS"),
            (false, _, KeyCode::F5) => self.terminal.pty_write(b"\x1B[15~"),
            (false, _, KeyCode::F6) => self.terminal.pty_write(b"\x1B[17~"),
            (false, _, KeyCode::F7) => self.terminal.pty_write(b"\x1B[18~"),
            (false, _, KeyCode::F8) => self.terminal.pty_write(b"\x1B[19~"),
            (false, _, KeyCode::F9) => self.terminal.pty_write(b"\x1B[20~"),
            (false, _, KeyCode::F10) => self.terminal.pty_write(b"\x1B[21~"),
            (false, _, KeyCode::F11) => self.terminal.pty_write(b"\x1B[23~"),
            (false, _, KeyCode::F12) => self.terminal.pty_write(b"\x1B[24~"),

            // Ctrl+英字（Shiftなし）は制御コードへ。Ctrl+C/L/V もここで処理。
            (true, false, code) => match ctrl_letter_code(code) {
                Some(b) => self.terminal.pty_write(&[b]),
                None => handled = false,
            },

            _ => handled = false,
        }

        if !handled {
            match &key_event.text {
                // 通常文字（英数字・記号・全角等の非IME入力）をそのまま送る
                Some(text) => self.terminal.pty_write(text.as_bytes()),
                // 修飾キー単体などテキストを生まないキーでは選択を消さない
                None => clear = false,
            }
        }

        if clear {
            self.view.update_contents(|view| {
                view.selection_range = None;
            });

            self.history_head = 0;
            self.mouse.pressed_pos = None;
            self.mouse.released_pos = None;
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
        let _ = self.clipboard.set_text(text);
    }

    fn paste_clipboard(&mut self) {
        match self.clipboard.get_text() {
            Ok(text) => {
                log::debug!("paste: {:?}", text);
                if self.mode.bracketed_paste {
                    self.terminal.pty_write(b"\x1b[200~");
                    self.terminal.pty_write(text.as_bytes());
                    self.terminal.pty_write(b"\x1b[201~");
                } else {
                    self.terminal.pty_write(text.as_bytes());
                }
            }
            Err(_) => {
                log::error!("Failed to paste something from clipboard");
            }
        }
    }

    fn normal_mouse_report(&mut self, button: u8, col: u32, row: u32) {
        let col = if 0 < col && col < 224 { col + 32 } else { 0 } as u8;
        let row = if 0 < row && row < 224 { row + 32 } else { 0 } as u8;

        let msg = [b'\x1b', b'[', b'M', 32 + button, col, row];

        self.terminal.pty_write(&msg);
    }

    fn sgr_ext_mouse_report(&mut self, button: u8, col: u32, row: u32, state: &ElementState) {
        let m = match state {
            ElementState::Pressed => 'M',
            ElementState::Released => 'm',
        };

        self.terminal
            .pty_write(format!("\x1b[<{button};{col};{row}{m}").as_bytes());
    }
}

#[cfg(feature = "multiplex")]
impl TerminalWindow {
    pub fn get_foreground_process_name(&self) -> String {
        let pgid = self.terminal.get_pgid();
        match std::fs::read(format!("/proc/{pgid}/cmdline")) {
            Ok(cmdline) => {
                let argv0 = cmdline.split(|b| *b == b'\0').next().unwrap();
                String::from_utf8_lossy(argv0).into()
            }
            Err(err) => {
                // A process group doesn't need to have a leader (PID=PGID).
                log::debug!("Failed to read /proc/{pgid}/cmdline: {}", err);
                "(unknown)".to_owned()
            }
        }
    }

    pub fn get_foreground_process_cwd(&self) -> std::path::PathBuf {
        let pgid = self.terminal.get_pgid();
        match std::fs::read_link(format!("/proc/{pgid}/cwd")) {
            Ok(cwd) => cwd,
            Err(err) => {
                // A process group doesn't need to have a leader (PID=PGID).
                log::debug!("Failed to read_link /proc/{pgid}/cwd: {}", err);

                // FIXME
                std::env::current_dir().unwrap()
            }
        }
    }
}
