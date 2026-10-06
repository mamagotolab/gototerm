use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::{CursorIcon, Window},
};

use crate::gt::GtMessage;
use crate::input::{
    backspace_bytes, cursor_key_bytes, cursor_key_sequence, enter_bytes, escape_bytes,
    function_key_bytes, space_bytes, tab_bytes, tilde_key_bytes, CursorKey, Mods, TildeKey,
};
use crate::keybindings::{self, ShortcutAction};
use crate::task_activity::{PaneActivity, PaneId};
use crate::terminal::TerminalSize;
use crate::view::{Selection, TerminalView, Viewport};
use crate::vt::{GridSelection, ShellLocation, VtTerminal};
use crate::Display;
use alacritty_terminal::selection::SelectionType;

type CursorPosition = PhysicalPosition<f64>;

pub(crate) fn visible_selection(
    selection: GridSelection,
    display_offset: usize,
    rows: usize,
    cols: usize,
) -> Option<Selection> {
    if rows == 0 || cols == 0 {
        return None;
    }

    let viewport_top = -(display_offset as i32);
    let viewport_bottom = viewport_top + rows as i32 - 1;

    if selection.block {
        let top = selection.start.line.0.min(selection.end.line.0);
        let bottom = selection.start.line.0.max(selection.end.line.0);
        if bottom < viewport_top || top > viewport_bottom {
            return None;
        }

        let left = selection.start.column.0.min(selection.end.column.0);
        let right = selection
            .start
            .column
            .0
            .max(selection.end.column.0)
            .min(cols - 1);
        if left >= cols {
            return None;
        }

        Some(Selection::Block {
            top: (top.max(viewport_top) - viewport_top) as usize,
            bottom: (bottom.min(viewport_bottom) - viewport_top) as usize,
            left,
            right,
        })
    } else {
        let (start, end) = if selection.start <= selection.end {
            (selection.start, selection.end)
        } else {
            (selection.end, selection.start)
        };
        if end.line.0 < viewport_top || start.line.0 > viewport_bottom {
            return None;
        }

        let (start_row, start_col) = if start.line.0 < viewport_top {
            (0, 0)
        } else {
            (
                (start.line.0 - viewport_top) as usize,
                start.column.0.min(cols - 1),
            )
        };
        let (end_row, end_col) = if end.line.0 > viewport_bottom {
            (rows - 1, cols - 1)
        } else {
            (
                (end.line.0 - viewport_top) as usize,
                end.column.0.min(cols - 1),
            )
        };

        Some(Selection::Linear {
            left: start_row * cols + start_col,
            right: end_row * cols + end_col,
        })
    }
}

fn selection_type_for_click(click_count: usize, block: bool) -> SelectionType {
    if block {
        SelectionType::Block
    } else {
        match click_count {
            1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        }
    }
}

/// AI（Claude Code）の Stop hook 受信時、ウィンドウが非フォーカスなら呼ぶ想定の
/// OS 通知。中身は固定文言のみ（動的な文字列を組み込まない）。
pub(crate) fn notify_completion() {
    const TITLE: &str = "gototerm";
    const BODY: &str = "AIの応答が完了しました";

    #[cfg(not(windows))]
    let result = std::process::Command::new("notify-send")
        .args([TITLE, BODY])
        .spawn();

    #[cfg(windows)]
    let result = show_toast(TITLE, BODY);

    if let Err(e) = result {
        log::debug!("OS 通知をスキップしました: {}", e);
    }
}

/// WinRT のトースト通知をプロセス内から直接出す。
///
/// 以前は同じ WinRT API を `powershell -Command` の非表示起動から叩いていたが、
/// 「隠しウィンドウで PowerShell を実行する」はマルウェアの常套手段そのもので、
/// Windows Defender が exe を隔離してしまった。子プロセスを起動しなければ
/// 検出理由そのものが無くなる。
#[cfg(windows)]
fn show_toast(title: &str, body: &str) -> windows::core::Result<()> {
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{
        ToastNotification, ToastNotificationManager, ToastTemplateType,
    };

    // AppId は Explorer のものを借用する。未パッケージのアプリが追加インストール
    // 無しで通知を出すための一般的な回避策。
    const APP_ID: &str = "Microsoft.Windows.Explorer";

    // ToastText02 は「太字の1行目＋本文」のテンプレート。text 要素が2つある。
    let xml: XmlDocument =
        ToastNotificationManager::GetTemplateContent(ToastTemplateType::ToastText02)?;
    let texts = xml.GetElementsByTagName(&HSTRING::from("text"))?;
    texts
        .Item(0)?
        .AppendChild(&xml.CreateTextNode(&HSTRING::from(title))?)?;
    texts
        .Item(1)?
        .AppendChild(&xml.CreateTextNode(&HSTRING::from(body))?)?;

    let toast = ToastNotification::CreateToastNotification(&xml)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?.Show(&toast)
}

/// URL やファイルを OS 標準のアプリで開く。Linux は xdg-open。Windows は
/// ShellExecuteW を直接呼ぶ。explorer に URL を渡すと引数をパスと誤解して
/// フォルダを開くことがあるため使わない。
pub(crate) fn open_url(url: &str) {
    #[cfg(not(windows))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(windows)]
    let result = shell_open(url);
    if let Err(e) = result {
        log::error!("URL を開けませんでした ({}): {}", url, e);
    }
}

/// URL やファイルを既定のアプリで開く。
///
/// 以前は `rundll32 url.dll,FileProtocolHandler` を起動していたが、rundll32 は
/// 正規ツールを悪用する手口(LOLBin)として Defender の監視対象。同じことは
/// ShellExecuteW を直接呼べば子プロセス無しで済む。
#[cfg(windows)]
fn shell_open(target: &str) -> windows::core::Result<()> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb = HSTRING::from("open");
    let target = HSTRING::from(target);
    let result = unsafe {
        ShellExecuteW(
            None,
            &verb,
            &target,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // 成功すると 32 より大きい擬似ハンドルが返る。32 以下はエラーコード。
    if result.0 as isize > 32 {
        Ok(())
    } else {
        // from_thread は GetLastError を読む（旧 from_win32 の後継）。
        Err(windows::core::Error::from_thread())
    }
}

/// Alt を押しながらの文字入力は ESC を前置して送る（xterm の metaSendsEscape 相当）。
///
/// winit は Alt+e の text を "e" として渡すので、そのまま送ると Alt が消える。
/// TUI アプリのキー割り当ては ESC 前置を前提にしているため、Alt が効かなくなる。
/// 実例: mutt の `<esc>e`（resend-message＝本文をデコードして編集）が、素の `e`
/// （edit-message＝生のメールソース）として届き、ISO-2022-JP のまま開いてしまう。
///
/// Ctrl+Alt は前置しない。Windows の AltGr が Ctrl+Alt として報告され、配列が
/// 文字そのものを生んでいるケースと区別できないため。
fn meta_prefixed(text: &str, alt: bool, ctrl: bool) -> Vec<u8> {
    if alt && !ctrl && !text.is_empty() {
        let mut out = Vec::with_capacity(text.len() + 1);
        out.push(0x1b);
        out.extend_from_slice(text.as_bytes());
        out
    } else {
        text.as_bytes().to_vec()
    }
}

fn report_mouse_to_app(mouse_mode: bool, shift: bool, ctrl_url: bool) -> bool {
    mouse_mode && !shift && !ctrl_url
}

fn known_local_cwd(location: Option<ShellLocation>) -> Option<PathBuf> {
    match location {
        Some(ShellLocation::Local(cwd)) => Some(cwd),
        _ => None,
    }
}

fn resolve_hint_file(token: &str, location: Option<ShellLocation>) -> Option<PathBuf> {
    resolve_existing_file_token(token, &known_local_cwd(location)?)
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
    link_hints: Option<crate::link_hints::LinkHints>,
    window: Rc<Window>,
    terminal: VtTerminal,
    pane_id: PaneId,
    activity: PaneActivity,

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
    // Pixel location is kept only for distinguishing link clicks from drags.
    pressed_pos: Option<CursorPosition>,
    // 現在のドラッグがローカル選択か（押下時に確定）。途中で Shift を離しても
    // ボタンを離すまでローカル選択を続けるため。
    selecting: bool,
    click_count: usize,
    last_clicked: std::time::Instant,
}

impl TerminalWindow {
    pub fn with_viewport(
        window: Rc<Window>,
        display: Display,
        viewport: Viewport,
        scale_factor: f64,
        cwd: Option<&std::path::Path>,
    ) -> Self {
        Self::with_viewport_command(window, display, viewport, scale_factor, cwd, None)
    }

    pub fn with_viewport_command(
        window: Rc<Window>,
        display: Display,
        viewport: Viewport,
        scale_factor: f64,
        cwd: Option<&std::path::Path>,
        command: Option<&[String]>,
    ) -> Self {
        let font_size = crate::TOYTERM_CONFIG.font_size;
        let view = TerminalView::with_viewport(
            display,
            viewport,
            font_size,
            scale_factor,
            Some((0, viewport.h)),
        );

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
            pane_id: PaneId::allocate(),
            activity: PaneActivity::default(),

            view,
            focused: true,
            modifiers: ModifiersState::empty(),
            mouse: MouseState {
                wheel_delta_x: 0.0,
                wheel_delta_y: 0.0,
                cursor_pos: CursorPosition::default(),
                pressed_pos: None,
                selecting: false,
                click_count: 0,
                last_clicked: std::time::Instant::now() - std::time::Duration::from_secs(10),
            },
            last_ime_commit: std::time::Instant::now() - std::time::Duration::from_secs(10),
            clicked_file: None,
            link_hints: None,
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

        // 画面が変わったときだけ alacritty のグリッドを取り込んで描画を更新する。
        if self.terminal.take_dirty() {
            self.cancel_link_hints();
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

        let new_selection_range = self.terminal.grid_selection().and_then(|selection| {
            visible_selection(selection, self.terminal.display_offset(), rows, cols)
        });
        if self.view.selection_range != new_selection_range {
            self.view.update_contents(|view| {
                view.selection_range = new_selection_range;
            });
        }

        false
    }

    fn update_mouse_selection(&mut self) {
        let CursorPosition { x, y } = self.mouse.cursor_pos;
        let cell_size = self.view.cell_size();
        self.terminal
            .update_selection_at_pixel(x, y, cell_size.w, cell_size.h);
    }

    fn clear_mouse_selection(&mut self) {
        self.mouse.pressed_pos = None;
        self.terminal.clear_selection();
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

    pub(crate) fn pane_id(&self) -> PaneId {
        self.pane_id
    }

    pub(crate) fn activity(&self) -> &PaneActivity {
        &self.activity
    }

    pub(crate) fn observed_location(&self) -> Option<ShellLocation> {
        self.terminal
            .location()
            .or_else(|| self.terminal.cwd().map(ShellLocation::Local))
    }

    pub fn take_clicked_file(&mut self) -> Option<PathBuf> {
        self.clicked_file.take()
    }

    pub fn take_gt_messages(&mut self) -> Vec<GtMessage> {
        let messages = self.terminal.take_gt_messages();
        let now = std::time::Instant::now();
        for message in &messages {
            self.activity.apply(message, now);
        }
        messages
    }

    pub fn set_viewport(&mut self, new_viewport: Viewport) {
        log::debug!("viewport changed: {:?}", new_viewport);
        self.view.set_viewport(new_viewport);
        self.resize_buffer();
        self.update_ime_position();
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
        if self.terminal.url_at(row, col).is_some() {
            return true;
        }
        self.modifiers.control_key()
            && self
                .token_at(row, col)
                .is_some_and(|token| looks_like_path(&token))
    }

    fn mouse_cell(&self) -> Option<(usize, usize)> {
        let CursorPosition { x, y } = self.mouse.cursor_pos;
        let viewport = self.viewport();
        if x < 0.0 || y < 0.0 || x >= viewport.w as f64 || y >= viewport.h as f64 {
            return None;
        }
        let cs = self.view.cell_size();
        Some((
            (y / cs.h.max(1) as f64) as usize,
            (x / cs.w.max(1) as f64) as usize,
        ))
    }

    fn ctrl_url_under_mouse(&self) -> bool {
        self.modifiers.control_key()
            && self
                .mouse_cell()
                .is_some_and(|(row, col)| self.terminal.url_at(row, col).is_some())
    }

    pub(crate) fn update_link_cursor(&self) {
        // 全ペインへ座標が届くため、ポインタがあるペインだけがカーソルを更新する。
        let Some((row, col)) = self.mouse_cell() else {
            return;
        };
        let local = !report_mouse_to_app(
            self.terminal.mouse_mode(),
            self.modifiers.shift_key(),
            self.ctrl_url_under_mouse(),
        );
        self.window
            .set_cursor_icon(if local && self.should_show_link_pointer(row, col) {
                CursorIcon::Pointer
            } else {
                CursorIcon::Text
            });
    }

    /// クリックでリンクを開く。URL は素のクリックで、ファイルは Ctrl+クリックのとき
    /// だけ（画面上のパスを普通にクリックしてプレビューが誤爆で開くのを防ぐ）。
    /// URL 判定を先にして、素のクリックでは stat しない。
    fn handle_link_click(&mut self, row: usize, col: usize, ctrl: bool) {
        if let Some(url) = self.terminal.url_at(row, col) {
            open_url(&url);
            return;
        }
        if ctrl {
            let Some(token) = self.token_at(row, col) else {
                return;
            };
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

    /// フォントサイズを差分だけ変える。セルが変わるので PTY のグリッドを組み直す。
    pub fn change_font_size(&mut self, size_diff: i32) {
        if self.view.increase_font_size(size_diff) {
            self.resize_buffer();
            self.update_ime_position();
        }
    }

    pub fn set_scale_factor(&mut self, scale_factor: f64) -> bool {
        self.view.set_scale_factor(scale_factor)
    }

    fn resize_buffer(&mut self) {
        self.clear_mouse_selection();

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
        if !gain {
            self.cancel_link_hints();
        }
        self.focused = gain;

        // Update cursor
        self.view.update_contents(|view| {
            view.view_focused = self.focused;
        });

        if gain {
            self.window.set_ime_allowed(!self.local_input_mode());
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
        if self.local_input_mode() && matches!(event, WindowEvent::Ime(_)) {
            self.view.update_contents(|view| view.preedit.clear());
            return;
        }
        if self.terminal.copy_mode_active()
            && matches!(
                event,
                WindowEvent::MouseInput { .. } | WindowEvent::CursorMoved { .. }
            )
        {
            return;
        }
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
                if self.mouse.selecting {
                    self.update_mouse_selection();
                }

                self.update_link_cursor();
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let is_inner = {
                    let viewport = self.viewport();
                    let (w, h) = (viewport.w as f64, viewport.h as f64);
                    let CursorPosition { x, y } = self.mouse.cursor_pos;
                    0.0 <= x && x < w && 0.0 <= y && y < h
                };

                if !is_inner {
                    self.clear_mouse_selection();
                    self.mouse.selecting = false;
                    return;
                }

                // Shift 押下中はマウス報告を無視してローカル選択に回す
                // （xterm の作法）。これで Claude Code 等のマウス報告アプリでも
                // Shift+ドラッグで画面の文字を選択 → Ctrl+Shift+C でコピーできる。
                // Released は「ドラッグ開始時にローカル選択だったか(selecting)」も見る。
                // 途中で Shift を離してもボタンを離すまでローカル選択を続け、
                // 選択範囲を固定する。
                let report_to_app = report_mouse_to_app(
                    self.terminal.mouse_mode(),
                    self.modifiers.shift_key(),
                    *button == MouseButton::Left && self.ctrl_url_under_mouse(),
                );
                let report = match state {
                    ElementState::Pressed => report_to_app,
                    // 押下をアプリへ送った後にCtrlを押しても、解放は同じ相手へ送る。
                    ElementState::Released => self.terminal.mouse_mode() && !self.mouse.selecting,
                };
                if report {
                    self.mouse.selecting = false;
                    self.mouse.pressed_pos = None;
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
                            // Ctrl を押しながらの開始は矩形選択。
                            let block = self.modifiers.control_key();
                            let CursorPosition { x, y } = self.mouse.cursor_pos;
                            let cell_size = self.view.cell_size();
                            self.terminal.start_selection_at_pixel(
                                selection_type_for_click(self.mouse.click_count, block),
                                x,
                                y,
                                cell_size.w,
                                cell_size.h,
                            );
                            // このドラッグはローカル選択。離すまで継続する。
                            self.mouse.selecting = true;
                            self.update_mouse_selection();
                        }
                        ElementState::Released => {
                            self.update_mouse_selection();
                            self.mouse.selecting = false;

                            // ドラッグ（選択）でない単純な左クリック。URL は素のクリックで
                            // 開き、ファイルは Ctrl+クリックのときだけ開く（handle_link_click
                            // 内で判定）。マウス対応アプリでも Ctrl+URL はローカル処理する。
                            if *button == MouseButton::Left {
                                if let Some(press) = self.mouse.pressed_pos.take() {
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
                if self.modifiers.shift_key() || self.terminal.copy_mode_active() {
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
                    let application = self.terminal.application_cursor_mode();
                    let vk = if vertical > 0 {
                        CursorKey::Up
                    } else {
                        CursorKey::Down
                    };
                    for _ in 0..vertical.abs() {
                        self.terminal.write(cursor_key_sequence(vk, application));
                    }
                    let hk = if horizontal > 0 {
                        CursorKey::Right
                    } else {
                        CursorKey::Left
                    };
                    for _ in 0..horizontal.abs() {
                        self.terminal.write(cursor_key_sequence(hk, application));
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
                if self.mouse.selecting {
                    self.update_mouse_selection();
                }
            }

            _ => {}
        }
    }

    /// カーソル系キー（矢印・Home・End）を、修飾キーとカーソルモードに応じて送る。
    fn write_cursor_key(&mut self, key: CursorKey, mods: Mods) {
        let bytes = cursor_key_bytes(key, self.terminal.application_cursor_mode(), mods);
        self.terminal.write(&bytes);
    }

    fn write_function_key(&mut self, n: u8, mods: Mods) {
        if let Some(bytes) = function_key_bytes(n, mods) {
            self.terminal.write(&bytes);
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

        if self.link_hints.is_some() {
            self.handle_link_hint_key(key_event);
            return;
        }
        if keybindings::lookup(self.modifiers, keycode) == Some(ShortcutAction::LinkHints) {
            self.start_link_hints();
            return;
        }

        if self.terminal.copy_mode_active() {
            self.handle_copy_mode_key(keycode);
            return;
        }
        if keybindings::lookup(self.modifiers, keycode) == Some(ShortcutAction::CopyMode) {
            self.terminal.toggle_copy_mode();
            self.view.update_contents(|view| view.preedit.clear());
            self.window.set_ime_allowed(false);
            return;
        }

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
        let window_shortcut = match keybindings::lookup(self.modifiers, keycode) {
            // フォント変更はマルチプレクサが全ペイン一括で処理する（ここでは扱わない）。
            Some(ShortcutAction::Copy) => {
                clear = false;
                self.copy_clipboard();
                true
            }
            Some(ShortcutAction::Paste) => {
                self.paste_clipboard();
                true
            }
            Some(ShortcutAction::ClearHistory) => {
                self.terminal.clear_history();
                true
            }
            _ => false,
        };
        if window_shortcut {
            handled = true;
        } else {
            // 特殊キーは修飾キーの有無で分岐させない。以前は (false, _, ...) の形で
            // Ctrl を弾いており、Ctrl+Backspace や Ctrl+PageUp などがどの腕にも
            // 一致せず、フォールバック先の text も None なので無反応だった。
            // 修飾キーの反映は src/input.rs に集約する（xterm 準拠）。
            let mods = Mods::new(shift, self.modifiers.alt_key(), ctrl);
            match keycode {
                KeyCode::Escape => {
                    self.clear_mouse_selection();
                    self.terminal.write(&escape_bytes(mods));
                }

                // Backspace は BS ではなく DEL を送る（Ctrl は BS、Alt は ESC 前置）。
                KeyCode::Backspace => self.terminal.write(&backspace_bytes(mods)),

                KeyCode::Enter => self.terminal.write(&enter_bytes(mods)),
                KeyCode::Tab => self.terminal.write(&tab_bytes(mods)),

                // Space は明示的に送る。IME 有効時に winit が text=None で Space を
                // 渡してくることがあり、text 経由だと何も送られず Claude Code の
                // 選択(スペースでトグル)等が効かなくなるため。ここに来る時点で
                // preedit は空（上でガード済み）なので変換中は影響しない。
                KeyCode::Space => match space_bytes(mods) {
                    Some(bytes) => self.terminal.write(&bytes),
                    // Ctrl+Space は送らない（IME 切り替えを邪魔しない）
                    None => handled = false,
                },

                KeyCode::ArrowUp => self.write_cursor_key(CursorKey::Up, mods),
                KeyCode::ArrowDown => self.write_cursor_key(CursorKey::Down, mods),
                KeyCode::ArrowRight => self.write_cursor_key(CursorKey::Right, mods),
                KeyCode::ArrowLeft => self.write_cursor_key(CursorKey::Left, mods),
                KeyCode::Home => self.write_cursor_key(CursorKey::Home, mods),
                KeyCode::End => self.write_cursor_key(CursorKey::End, mods),

                KeyCode::Insert => self
                    .terminal
                    .write(&tilde_key_bytes(TildeKey::Insert, mods)),
                KeyCode::Delete => self
                    .terminal
                    .write(&tilde_key_bytes(TildeKey::Delete, mods)),
                KeyCode::PageUp => self
                    .terminal
                    .write(&tilde_key_bytes(TildeKey::PageUp, mods)),
                KeyCode::PageDown => self
                    .terminal
                    .write(&tilde_key_bytes(TildeKey::PageDown, mods)),

                KeyCode::F1 => self.write_function_key(1, mods),
                KeyCode::F2 => self.write_function_key(2, mods),
                KeyCode::F3 => self.write_function_key(3, mods),
                KeyCode::F4 => self.write_function_key(4, mods),
                KeyCode::F5 => self.write_function_key(5, mods),
                KeyCode::F6 => self.write_function_key(6, mods),
                KeyCode::F7 => self.write_function_key(7, mods),
                KeyCode::F8 => self.write_function_key(8, mods),
                KeyCode::F9 => self.write_function_key(9, mods),
                KeyCode::F10 => self.write_function_key(10, mods),
                KeyCode::F11 => self.write_function_key(11, mods),
                KeyCode::F12 => self.write_function_key(12, mods),

                // Ctrl+英字（Shiftなし）は制御コードへ。Ctrl+C/L/V もここで処理。
                code if ctrl && !shift => match ctrl_letter_code(code) {
                    Some(b) => self.terminal.write(&[b]),
                    None => handled = false,
                },

                _ => handled = false,
            }
        }

        if !handled {
            match &key_event.text {
                // 通常文字（英数字・記号・全角等の非IME入力）を送る。
                // Alt 押下時は ESC を前置する（後述の meta_prefixed 参照）。
                Some(text) => {
                    let bytes =
                        meta_prefixed(text, self.modifiers.alt_key(), self.modifiers.control_key());
                    self.terminal.write(&bytes);
                }
                // 修飾キー単体などテキストを生まないキーでは選択を消さない
                None => clear = false,
            }
        }

        if clear {
            self.view.update_contents(|view| {
                view.selection_range = None;
            });

            self.clear_mouse_selection();

            // 実際の入力をしたらスクロールバックを最下部に戻す
            // （履歴を見たまま打って迷子になるのを防ぐ）。コピー等の
            // clear=false のキーでは戻さないので、履歴からのコピーは可能。
            self.terminal.scroll_to_bottom();
        }
    }

    fn copy_clipboard(&mut self) {
        let mut text = String::new();

        if let Some((selection, selected_text)) = self.terminal.tracked_selection_text() {
            text = selected_text;
            if !selection.block {
                text = dedent_common_indent(&text);
            }
        }

        // 末尾行の余分な空白も落とす（改行は残す）。
        let n = text.trim_end_matches([' ', '\t']).len();
        text.truncate(n);

        log::info!("copy: {:?}", text);
        set_clipboard(&text);
    }

    pub(crate) fn copy_mode_active(&self) -> bool {
        self.terminal.copy_mode_active()
    }

    pub(crate) fn local_input_mode(&self) -> bool {
        self.copy_mode_active() || self.link_hints.is_some()
    }

    fn cancel_link_hints(&mut self) {
        if self.link_hints.take().is_some() {
            self.view.update_contents(|view| view.hint_labels.clear());
            if self.focused {
                self.window.set_ime_allowed(!self.copy_mode_active());
            }
        }
    }

    fn start_link_hints(&mut self) {
        use crate::link_hints::{LinkHints, Target};
        self.check_update();
        let location = self.observed_location();
        let mut targets = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for row in 0..self.view.lines.len() {
            for col in 0..self.view.lines[row].columns() {
                if let Some(url) = self.terminal.url_at(row, col) {
                    if seen.insert(format!("url:{url}")) {
                        targets.push((row, col, Target::Url(url)));
                    }
                } else if let Some(token) = self.token_at(row, col) {
                    if looks_like_path(&token) && seen.insert(format!("file:{token}")) {
                        if let Some(file) = resolve_hint_file(&token, location.clone()) {
                            targets.push((row, col, Target::File(file)));
                        }
                    }
                }
            }
        }
        self.link_hints = LinkHints::new(targets);
        if let Some(state) = &self.link_hints {
            let labels = state
                .hints
                .iter()
                .map(|hint| {
                    (
                        hint.row,
                        hint.col.saturating_sub(
                            (hint.col + hint.label.len())
                                .saturating_sub(self.view.lines[hint.row].columns()),
                        ),
                        hint.label.clone(),
                    )
                })
                .collect();
            self.view.update_contents(|view| view.hint_labels = labels);
            self.window.set_ime_allowed(false);
        }
    }

    fn handle_link_hint_key(&mut self, key: &KeyEvent) {
        // Cancel before execution when output changed between the last frame and this key.
        if self.terminal.take_dirty() {
            self.cancel_link_hints();
            self.terminal.mark_dirty();
            return;
        }
        if key.physical_key == PhysicalKey::Code(KeyCode::Escape) {
            self.cancel_link_hints();
            return;
        }
        if key.repeat {
            return;
        }
        let Some(ch) = key
            .text
            .as_ref()
            .and_then(|text| text.chars().next())
            .filter(|ch| ch.is_ascii_alphabetic())
        else {
            self.cancel_link_hints();
            return;
        };
        match self.link_hints.as_mut().unwrap().input(ch) {
            Ok(Some(target)) => {
                self.cancel_link_hints();
                match target {
                    crate::link_hints::Target::Url(url) => open_url(&url),
                    crate::link_hints::Target::File(file) => self.clicked_file = Some(file),
                }
            }
            Ok(None) => {}
            Err(()) => self.cancel_link_hints(),
        }
    }

    fn handle_copy_mode_key(&mut self, key: KeyCode) {
        use alacritty_terminal::vi_mode::ViMotion;
        use KeyCode::*;
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        match (ctrl, key) {
            (_, Escape) | (true, Space) => {
                self.terminal.toggle_copy_mode();
                self.window.set_ime_allowed(true);
            }
            (false, KeyY) => {
                if let Some((_, text)) = self.terminal.tracked_selection_text() {
                    set_clipboard(&text);
                }
                self.terminal.toggle_copy_mode();
                self.window.set_ime_allowed(true);
            }
            (true, KeyU) => self.terminal.copy_mode_page(true),
            (true, KeyD) => self.terminal.copy_mode_page(false),
            (false, KeyG) => self.terminal.copy_mode_edge(!shift),
            (_, KeyV) => self.terminal.copy_mode_select(if ctrl {
                SelectionType::Block
            } else if shift {
                SelectionType::Lines
            } else {
                SelectionType::Simple
            }),
            (false, KeyH | ArrowLeft) => self.terminal.copy_mode_motion(ViMotion::Left),
            (false, KeyJ | ArrowDown) => self.terminal.copy_mode_motion(ViMotion::Down),
            (false, KeyK | ArrowUp) => self.terminal.copy_mode_motion(ViMotion::Up),
            (false, KeyL | ArrowRight) => self.terminal.copy_mode_motion(ViMotion::Right),
            _ => {}
        }
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
    #[test]
    fn file_hints_require_observed_local_cwd() {
        let dir = std::env::temp_dir().join(format!("gototerm-hint-cwd-{}", std::process::id()));
        let first = dir.join("first");
        let second = dir.join("second");
        for path in [&first, &second] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::write(path.join("same.txt"), "test").unwrap();
        }
        assert!(super::resolve_hint_file("same.txt", None).is_none());
        assert!(super::resolve_hint_file(
            "same.txt",
            Some(crate::vt::ShellLocation::Remote {
                host: "server".into(),
                path: first.clone()
            })
        )
        .is_none());
        let a = super::resolve_hint_file(
            "same.txt",
            Some(crate::vt::ShellLocation::Local(first.clone())),
        )
        .unwrap();
        let b = super::resolve_hint_file(
            "same.txt",
            Some(crate::vt::ShellLocation::Local(second.clone())),
        )
        .unwrap();
        assert_eq!(
            std::fs::canonicalize(&a).unwrap(),
            std::fs::canonicalize(first.join("same.txt")).unwrap()
        );
        assert_eq!(
            std::fs::canonicalize(&b).unwrap(),
            std::fs::canonicalize(second.join("same.txt")).unwrap()
        );
        assert_ne!(a, b);
        std::fs::remove_dir_all(dir).unwrap();
    }
    use super::{
        dedent_common_indent, meta_prefixed, report_mouse_to_app, resolve_existing_file_token,
        resolve_path_token, selection_type_for_click, visible_selection,
    };
    use crate::view::Selection;
    use crate::vt::GridSelection;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::selection::SelectionType;
    use std::path::{Path, PathBuf};

    #[test]
    fn ctrl_url_click_overrides_app_mouse_reporting() {
        assert!(!report_mouse_to_app(true, false, true));
        assert!(report_mouse_to_app(true, false, false));
        assert!(!report_mouse_to_app(true, true, false));
        assert!(!report_mouse_to_app(false, false, false));
    }

    fn linear_grid_selection(start: (i32, usize), end: (i32, usize)) -> GridSelection {
        GridSelection {
            start: Point::new(Line(start.0), Column(start.1)),
            end: Point::new(Line(end.0), Column(end.1)),
            block: false,
        }
    }

    #[test]
    fn alt_key_sends_escape_prefix() {
        // mutt の <esc>e（resend-message）が効くために必要。
        assert_eq!(meta_prefixed("e", true, false), b"\x1be".to_vec());
        assert_eq!(meta_prefixed("f", true, false), b"\x1bf".to_vec());
    }

    #[test]
    fn plain_key_is_sent_unchanged() {
        assert_eq!(meta_prefixed("e", false, false), b"e".to_vec());
        assert_eq!(meta_prefixed("日", false, false), "日".as_bytes().to_vec());
    }

    #[test]
    fn ctrl_alt_is_not_prefixed_because_of_altgr() {
        // Windows の AltGr は Ctrl+Alt として報告され、配列が文字を生んでいる。
        // ここで ESC を前置すると AltGr で入力できる記号が壊れる。
        assert_eq!(meta_prefixed("@", true, true), b"@".to_vec());
    }

    #[test]
    fn empty_text_stays_empty_even_with_alt() {
        assert!(meta_prefixed("", true, false).is_empty());
    }

    #[test]
    fn selection_fully_above_viewport_is_not_painted() {
        let selection = linear_grid_selection((-8, 1), (-6, 4));
        assert_eq!(visible_selection(selection, 3, 4, 10), None);
    }

    #[test]
    fn selection_crossing_viewport_is_clipped_to_visible_rows() {
        let selection = linear_grid_selection((-5, 2), (1, 4));
        assert_eq!(
            visible_selection(selection, 3, 4, 10),
            Some(Selection::Linear { left: 0, right: 39 })
        );
    }

    #[test]
    fn block_selection_clips_rows_without_linearizing_columns() {
        let selection = GridSelection {
            start: Point::new(Line(-5), Column(7)),
            end: Point::new(Line(-1), Column(2)),
            block: true,
        };
        assert_eq!(
            visible_selection(selection, 3, 4, 10),
            Some(Selection::Block {
                top: 0,
                bottom: 2,
                left: 2,
                right: 7,
            })
        );
    }

    #[test]
    fn click_count_selects_tracked_selection_kind() {
        assert_eq!(selection_type_for_click(1, false), SelectionType::Simple);
        assert_eq!(selection_type_for_click(2, false), SelectionType::Semantic);
        assert_eq!(selection_type_for_click(3, false), SelectionType::Lines);
        assert_eq!(selection_type_for_click(2, true), SelectionType::Block);
    }

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
