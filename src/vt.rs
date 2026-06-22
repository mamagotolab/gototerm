//! alacritty_terminal を VT エンジンとして駆動する新しい端末コア。
//!
//! 自作パーサ（control_function）の代わりに `vte::ansi::Processor` で解析し、
//! グリッド・モード・スクロールバック・応答シーケンスを `Term` に委ねる。
//! PTY は portable-pty（Unix=openpty / Windows=ConPTY）。

use std::io::{Read as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Rgb;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize};
use vte::ansi::Processor;

use crate::sixel;
use crate::terminal::PositionedImage;

/// PTY master への書き込み口。入力と応答(DA/DSR等)の両方が使うため共有する。
pub type SharedWriter = Arc<Mutex<Box<dyn std::io::Write + Send>>>;

/// alacritty の Term が応答シーケンスを送るときに呼ばれるリスナー。
/// `PtyWrite`(DA/DSR等)・`TextAreaSizeRequest`(CSI14t/16t=ピクセル寸法)・
/// `ColorRequest`(OSC色問い合わせ)に応答する。これらを返さないと、
/// yazi 等の画像オーバーレイが配置寸法を決められず画面がガタつく。
#[derive(Clone)]
pub struct EventProxy {
    writer: SharedWriter,
    winsize: Arc<Mutex<WindowSize>>,
}

impl EventProxy {
    fn reply(&self, text: &str) {
        let _ = self.writer.lock().unwrap().write_all(text.as_bytes());
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        match event {
            AlacEvent::PtyWrite(text) => {
                // alacritty の Primary DA 応答(VT102=`?6c`)に Sixel(4) を足し、
                // 画像対応を申告する。これが無いと yazi 等が Sixel を送らず、
                // Wayland オーバーレイ描画に落ちて画面がガタつく。
                if text == "\x1b[?6c" {
                    self.reply("\x1b[?62;4c"); // VT220 + Sixel
                } else {
                    self.reply(&text);
                }
            }
            AlacEvent::TextAreaSizeRequest(format) => {
                let ws = *self.winsize.lock().unwrap();
                self.reply(&format(ws));
            }
            AlacEvent::ColorRequest(index, format) => {
                self.reply(&format(color_index_to_rgb(index)));
            }
            _ => {}
        }
    }
}

/// alacritty の色インデックスを、ユーザ設定パレットの RGB に解決する。
/// 0..=15=ANSI16色 / 16..=231=6x6x6キューブ / 232..=255=グレースケール /
/// 256=前景 / 257=背景 / 258=カーソル。
fn color_index_to_rgb(index: usize) -> Rgb {
    let cfg = &crate::TOYTERM_CONFIG;
    let split = |rgba: u32| Rgb {
        r: ((rgba >> 24) & 0xff) as u8,
        g: ((rgba >> 16) & 0xff) as u8,
        b: ((rgba >> 8) & 0xff) as u8,
    };
    match index {
        0 => split(cfg.color_black),
        1 => split(cfg.color_red),
        2 => split(cfg.color_green),
        3 => split(cfg.color_yellow),
        4 => split(cfg.color_blue),
        5 => split(cfg.color_magenta),
        6 => split(cfg.color_cyan),
        7 => split(cfg.color_white),
        8 => split(cfg.color_bright_black),
        9 => split(cfg.color_bright_red),
        10 => split(cfg.color_bright_green),
        11 => split(cfg.color_bright_yellow),
        12 => split(cfg.color_bright_blue),
        13 => split(cfg.color_bright_magenta),
        14 => split(cfg.color_bright_cyan),
        15 => split(cfg.color_bright_white),
        16..=231 => {
            let i = index as u8 - 16;
            let to = |v: u8| -> u8 {
                if v == 0 {
                    0
                } else {
                    55 + 40 * v
                }
            };
            Rgb {
                r: to((i / 36) % 6),
                g: to((i / 6) % 6),
                b: to(i % 6),
            }
        }
        232..=255 => {
            let v = 8 + 10 * (index as u8 - 232);
            Rgb { r: v, g: v, b: v }
        }
        257 => split(cfg.color_background),
        _ => split(cfg.color_foreground), // 256=前景, 258=カーソル, その他
    }
}

/// alacritty に渡すグリッドサイズ（Dimensions 実装）。
#[derive(Clone, Copy)]
pub struct GridSize {
    pub cols: usize,
    pub lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// alacritty_terminal ベースの端末。`term` を描画側と共有する。
pub struct VtTerminal {
    pub term: Arc<Mutex<Term<EventProxy>>>,
    writer: SharedWriter,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    exited: Arc<AtomicBool>,
    dirty: Arc<AtomicBool>,
    /// テキスト領域の現在寸法。`TextAreaSizeRequest` 応答に使う（resize で更新）。
    winsize: Arc<Mutex<WindowSize>>,
    /// Sixel で描かれた画像。グリッドとは別に保持し、描画時に重ねる。
    images: Arc<Mutex<Vec<PositionedImage>>>,
    /// 直近の代替画面(Alt Screen)状態。切替時に画像を消すため。
    last_alt: Arc<AtomicBool>,
}

fn window_size(cols: usize, lines: usize, cell_w: u16, cell_h: u16) -> WindowSize {
    WindowSize {
        num_cols: cols as u16,
        num_lines: lines as u16,
        cell_width: cell_w,
        cell_height: cell_h,
    }
}

/// PTY バイト列を分割した断片。
enum Seg {
    /// 通常の VT 列。alacritty の Processor へ流す。
    Pass(Vec<u8>),
    /// Sixel の本体（`ESC P …q` と ST を除いた中身）。自前で描画する。
    Sixel(Vec<u8>),
}

#[derive(Default, Clone, Copy, PartialEq)]
enum SplitState {
    #[default]
    Normal,
    Esc,
    DcsIntro,
    SixelData,
    SixelEsc,
    DcsPass,
    DcsPassEsc,
}

/// PTY バイト列から Sixel(DCS) を抜き出す状態機械。チャンクをまたいで状態を保つ。
/// Sixel 以外の DCS（DECRQSS 等）はそのまま Pass に通す。
#[derive(Default)]
struct SixelSplitter {
    state: SplitState,
    intro: Vec<u8>,
    not_sixel: bool,
    payload: Vec<u8>,
}

impl SixelSplitter {
    fn feed(&mut self, input: &[u8]) -> Vec<Seg> {
        let mut segs: Vec<Seg> = Vec::new();
        let mut pass: Vec<u8> = Vec::new();

        for &b in input {
            match self.state {
                SplitState::Normal => {
                    if b == 0x1b {
                        self.state = SplitState::Esc;
                    } else {
                        pass.push(b);
                    }
                }
                SplitState::Esc => {
                    if b == b'P' {
                        self.state = SplitState::DcsIntro;
                        self.intro.clear();
                        self.not_sixel = false;
                    } else {
                        pass.push(0x1b);
                        if b == 0x1b {
                            // ESC ESC: 2つ目を新たな ESC として扱う
                        } else {
                            pass.push(b);
                            self.state = SplitState::Normal;
                        }
                    }
                }
                SplitState::DcsIntro => {
                    if (0x40..=0x7e).contains(&b) {
                        if b == b'q' && !self.not_sixel {
                            self.state = SplitState::SixelData;
                            self.payload.clear();
                        } else {
                            // 非Sixel DCS: ここまでを pass に出し ST まで素通し
                            pass.push(0x1b);
                            pass.push(b'P');
                            pass.extend_from_slice(&self.intro);
                            pass.push(b);
                            self.state = SplitState::DcsPass;
                        }
                    } else {
                        if (0x20..=0x2f).contains(&b) {
                            self.not_sixel = true; // 中間バイト($ +等) → DECRQSS等
                        }
                        self.intro.push(b);
                    }
                }
                SplitState::SixelData => {
                    if b == 0x07 {
                        if !pass.is_empty() {
                            segs.push(Seg::Pass(std::mem::take(&mut pass)));
                        }
                        segs.push(Seg::Sixel(std::mem::take(&mut self.payload)));
                        self.state = SplitState::Normal;
                    } else if b == 0x1b {
                        self.state = SplitState::SixelEsc;
                    } else {
                        self.payload.push(b);
                    }
                }
                SplitState::SixelEsc => {
                    // ESC '\' = ST。いずれにせよ Sixel は終了。
                    if !pass.is_empty() {
                        segs.push(Seg::Pass(std::mem::take(&mut pass)));
                    }
                    segs.push(Seg::Sixel(std::mem::take(&mut self.payload)));
                    self.state = SplitState::Normal;
                    if b == 0x1b {
                        self.state = SplitState::Esc;
                    } else if b != b'\\' {
                        pass.push(b);
                    }
                }
                SplitState::DcsPass => {
                    pass.push(b);
                    if b == 0x07 {
                        self.state = SplitState::Normal;
                    } else if b == 0x1b {
                        self.state = SplitState::DcsPassEsc;
                    }
                }
                SplitState::DcsPassEsc => {
                    pass.push(b);
                    self.state = if b == b'\\' {
                        SplitState::Normal
                    } else {
                        SplitState::DcsPass
                    };
                }
            }
        }
        if !pass.is_empty() {
            segs.push(Seg::Pass(pass));
        }
        segs
    }
}

/// Sixel をデコードし、現在のカーソル位置に画像として置く。
/// 画像の高さ分だけカーソルを下げて後続出力と重ならないようにする。
fn place_sixel(
    payload: &[u8],
    term: &Arc<Mutex<Term<EventProxy>>>,
    images: &Arc<Mutex<Vec<PositionedImage>>>,
    winsize: &Arc<Mutex<WindowSize>>,
    processor: &mut Processor,
) {
    let img = sixel::Parser::new().decode(&mut payload.iter().map(|&b| b as char));
    if img.width == 0 || img.height == 0 {
        return;
    }

    let cell_h = winsize.lock().unwrap().cell_height.max(1) as u64;

    let (row, col) = {
        let term = term.lock().unwrap();
        let p = term.grid().cursor.point;
        (p.line.0 as isize, p.column.0 as isize)
    };

    images.lock().unwrap().push(PositionedImage {
        row,
        col,
        width: img.width,
        height: img.height,
        data: img.data,
    });

    let rows = ((img.height + cell_h - 1) / cell_h) as usize;
    let nl = vec![b'\n'; rows];
    let mut term = term.lock().unwrap();
    processor.advance(&mut *term, &nl);
}

impl VtTerminal {
    pub fn new(
        cols: usize,
        lines: usize,
        cell_w: u16,
        cell_h: u16,
        cwd: &std::path::Path,
        shell: &[String],
    ) -> Self {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(pty_size(cols, lines, cell_w, cell_h))
            .expect("openpty");

        // シェルを起動
        let mut cmd = CommandBuilder::new(&shell[0]);
        for arg in &shell[1..] {
            cmd.arg(arg);
        }
        // 親の環境を引き継ぐが、「別端末の正体」を示す変数は落とす。
        // これらが残ると yazi 等が「kitty/ghostty だから画像を出せる」と誤検出し、
        // 画像プロトコル非対応の gototerm で preview のたびに画面が乱れる。
        // （例：Ghostty のシェルから gototerm を起動すると TERM_PROGRAM=ghostty が漏れる）
        const STRIP_ENV: &[&str] = &[
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "KITTY_WINDOW_ID",
            "KITTY_PID",
            "KITTY_INSTALLATION_DIR",
            "GHOSTTY_RESOURCES_DIR",
            "GHOSTTY_BIN_DIR",
            "GHOSTTY_SHELL_FEATURES",
            "GHOSTTY_SHELL_INTEGRATION_XDG_DIR",
            "KONSOLE_VERSION",
            "KONSOLE_DBUS_SESSION",
            "KONSOLE_DBUS_SERVICE",
            "KONSOLE_DBUS_WINDOW",
            "VTE_VERSION",
            "WEZTERM_EXECUTABLE",
            "WEZTERM_PANE",
            "WEZTERM_UNIX_SOCKET",
            "WEZTERM_CONFIG_FILE",
        ];
        for (key, val) in std::env::vars() {
            if STRIP_ENV.contains(&key.as_str()) {
                continue;
            }
            cmd.env(key, val);
        }
        // alacritty_terminal は xterm 互換なので xterm-256color を名乗る
        cmd.env("TERM", "xterm-256color");
        // 自分の正体を伝える（画像対応端末と誤認させない）
        cmd.env("TERM_PROGRAM", "gototerm");
        cmd.cwd(cwd);

        let mut child = pair.slave.spawn_command(cmd).expect("spawn shell");
        let killer = child.clone_killer();
        let reader = pair.master.try_clone_reader().expect("pty reader");
        let writer: SharedWriter =
            Arc::new(Mutex::new(pair.master.take_writer().expect("pty writer")));
        let master = pair.master;
        drop(pair.slave); // slave を閉じ、子終了時に reader が EOF を受け取れるように

        let winsize = Arc::new(Mutex::new(window_size(cols, lines, cell_w, cell_h)));
        let proxy = EventProxy {
            writer: writer.clone(),
            winsize: winsize.clone(),
        };
        let size = GridSize { cols, lines };
        let term = Arc::new(Mutex::new(Term::new(Config::default(), &size, proxy)));

        let exited = Arc::new(AtomicBool::new(false));
        let dirty = Arc::new(AtomicBool::new(true));
        let images: Arc<Mutex<Vec<PositionedImage>>> = Arc::new(Mutex::new(Vec::new()));
        let last_alt = Arc::new(AtomicBool::new(false));

        // 読取スレッド：PTY 出力を Sixel と通常VTに分け、後者を Processor に流す
        {
            let term = term.clone();
            let exited = exited.clone();
            let dirty = dirty.clone();
            let images = images.clone();
            let winsize = winsize.clone();
            std::thread::spawn(move || {
                let mut processor: Processor = Processor::new();
                let mut splitter = SixelSplitter::default();
                let mut reader = reader;
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF: 子プロセス終了
                        Ok(n) => {
                            for seg in splitter.feed(&buf[..n]) {
                                match seg {
                                    Seg::Pass(bytes) => {
                                        let mut term = term.lock().unwrap();
                                        processor.advance(&mut *term, &bytes);
                                    }
                                    Seg::Sixel(payload) => {
                                        place_sixel(&payload, &term, &images, &winsize, &mut processor);
                                    }
                                }
                            }
                            dirty.store(true, Ordering::SeqCst);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                exited.store(true, Ordering::SeqCst);
            });
        }

        // 子プロセスを回収するスレッド。子の終了を確実な終了シグナルとして使う。
        // Windows の ConPTY では子が終了しても master 読み取りが EOF を返さない
        // ことがあり、reader 側だけに頼ると `exit` で閉じない。ここで終了フラグを立てる。
        {
            let exited = exited.clone();
            let dirty = dirty.clone();
            std::thread::spawn(move || {
                let _ = child.wait();
                exited.store(true, Ordering::SeqCst);
                dirty.store(true, Ordering::SeqCst);
            });
        }

        VtTerminal {
            term,
            writer,
            master,
            killer,
            exited,
            dirty,
            winsize,
            images,
            last_alt,
        }
    }

    /// 前回以降に画面内容が変わったか（変わっていれば true を返し、フラグを下げる）。
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::SeqCst)
    }

    /// 現在の端末サイズ (columns, screen_lines)。
    pub fn size(&self) -> (usize, usize) {
        use alacritty_terminal::grid::Dimensions as _;
        let term = self.term.lock().unwrap();
        (term.columns(), term.screen_lines())
    }

    pub fn mouse_mode(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.lock().unwrap().mode().intersects(TermMode::MOUSE_MODE)
    }

    /// 代替画面(Alt Screen)中か。nvim/less 等の全画面 TUI で true。
    pub fn alt_screen(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.lock().unwrap().mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn sgr_mouse(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.lock().unwrap().mode().contains(TermMode::SGR_MOUSE)
    }

    pub fn bracketed_paste(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.lock().unwrap().mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// スクロールバック（履歴）を消去する。
    pub fn clear_history(&self) {
        self.term.lock().unwrap().grid_mut().clear_history();
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// スクロールバック表示を delta 行ぶん動かす（正で過去方向＝上）。
    pub fn scroll(&self, delta: i32) {
        use alacritty_terminal::grid::Scroll;
        self.term.lock().unwrap().scroll_display(Scroll::Delta(delta));
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// スクロールバックを最下部（現在）に戻す。キー入力時に呼ぶ。
    pub fn scroll_to_bottom(&self) {
        use alacritty_terminal::grid::Scroll;
        self.term.lock().unwrap().scroll_display(Scroll::Bottom);
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// ユーザー入力などを PTY master に書く。
    pub fn write(&self, data: &[u8]) {
        let _ = self.writer.lock().unwrap().write_all(data);
    }

    /// 端末サイズを変更する（カーネル側＋グリッド側）。
    pub fn resize(&self, cols: usize, lines: usize, cell_w: u16, cell_h: u16) {
        let _ = self.master.resize(pty_size(cols, lines, cell_w, cell_h));
        self.term.lock().unwrap().resize(GridSize { cols, lines });
        *self.winsize.lock().unwrap() = window_size(cols, lines, cell_w, cell_h);
    }

    pub fn kill(&mut self) {
        let _ = self.killer.kill();
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }
}

// ============================================================================
// 描画アダプタ：alacritty のグリッドを既存の描画形式(Line/Cell)へ変換する
// ============================================================================

use crate::terminal::{Cell as TCell, Color as TColor, Cursor as TCursor, CursorStyle, GraphicAttribute, Line as TLine};
use alacritty_terminal::term::cell::Flags;
use vte::ansi::{Color as AColor, CursorShape, NamedColor};

/// 1フレーム分の描画スナップショット。
pub struct Snapshot {
    pub lines: Vec<TLine>,
    pub cursor: Option<TCursor>,
    pub images: Vec<PositionedImage>,
}

impl VtTerminal {
    /// 現在の画面内容を既存描画形式に変換して取り出す。
    pub fn snapshot(&self) -> Snapshot {
        let term = self.term.lock().unwrap();
        let columns = term.columns();
        let screen_lines = term.screen_lines();

        // 空白セルで初期化した行バッファ
        let blank = TCell::head(' ', 1, GraphicAttribute::default());
        let mut rows: Vec<Vec<TCell>> = vec![vec![blank; columns]; screen_lines];

        // 履歴スクロール量。スクロール中、display_iter は履歴を「負の行番号」で
        // 返すため、表示行 = グリッド行 + display_offset で 0..screen_lines に直す。
        let display_offset = term.grid().display_offset() as i32;

        let content = term.renderable_content();

        for indexed in content.display_iter {
            let row = indexed.point.line.0 + display_offset; // 0..screen_lines に正規化
            let col = indexed.point.column.0;
            if row < 0 || row as usize >= screen_lines || col >= columns {
                continue;
            }
            let line = row;
            let cell = &indexed;

            // 全角・スペーサ
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                rows[line as usize][col] = TCell::spacer(1);
                continue;
            }
            let width: u16 = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };

            let bold: i8 = if cell.flags.contains(Flags::DIM) {
                -1
            } else if cell.flags.intersects(Flags::BOLD | Flags::BOLD_ITALIC) {
                1
            } else {
                0
            };

            let attr = GraphicAttribute {
                fg: map_color(cell.fg),
                bg: map_color(cell.bg),
                bold,
                inversed: cell.flags.contains(Flags::INVERSE),
                blinking: 0,
                concealed: cell.flags.contains(Flags::HIDDEN),
            };

            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            rows[line as usize][col] = TCell::head(ch, width, attr);
        }

        // Sixel 画像の間引き：画面外・テキストで上書きされた・Alt画面切替で消す。
        let images = {
            use alacritty_terminal::term::TermMode;
            let alt = term.mode().contains(TermMode::ALT_SCREEN);
            let prev_alt = self.last_alt.swap(alt, Ordering::SeqCst);

            let (cell_w, cell_h) = {
                let ws = self.winsize.lock().unwrap();
                (ws.cell_width.max(1) as usize, ws.cell_height.max(1) as usize)
            };

            let mut imgs = self.images.lock().unwrap();
            if alt != prev_alt {
                imgs.clear();
            }
            imgs.retain(|im| {
                if im.row < 0 || im.row as usize >= screen_lines {
                    return false;
                }
                let r0 = im.row as usize;
                let c0 = im.col.max(0) as usize;
                let nrows = (im.height as usize + cell_h - 1) / cell_h;
                let ncols = (im.width as usize + cell_w - 1) / cell_w;
                // 画像が覆うセルにテキストが書かれていたら（＝上書き）画像を捨てる
                for r in r0..(r0 + nrows).min(screen_lines) {
                    for c in c0..(c0 + ncols).min(columns) {
                        if rows[r][c].ch != ' ' {
                            return false;
                        }
                    }
                }
                true
            });
            imgs.clone()
        };

        let lines: Vec<TLine> = rows
            .into_iter()
            .map(|cells| TLine::from_cells(cells, false))
            .collect();

        // カーソル
        let rc = content.cursor;
        let cursor = match rc.shape {
            CursorShape::Hidden => None,
            shape => {
                let style = match shape {
                    CursorShape::Underline => CursorStyle::Underline,
                    CursorShape::Beam => CursorStyle::Bar,
                    _ => CursorStyle::Block,
                };
                // カーソルもスクロール量で正規化。履歴を遡って画面外に出たら隠す。
                let row = rc.point.line.0 + display_offset;
                if row < 0 || row as usize >= screen_lines {
                    None
                } else {
                    Some(TCursor::at(row as usize, rc.point.column.0, style))
                }
            }
        };

        Snapshot {
            lines,
            cursor,
            images,
        }
    }
}

/// alacritty の色を既存の Color へ変換する。
/// 標準16色・前景・背景は名前付きのまま（ユーザ設定パレットが効く）、
/// それ以外は RGB に解決する。
fn map_color(c: AColor) -> TColor {
    match c {
        AColor::Named(n) => match n {
            NamedColor::Foreground => TColor::Foreground,
            NamedColor::Background => TColor::Background,
            NamedColor::Black => TColor::Black,
            NamedColor::Red => TColor::Red,
            NamedColor::Green => TColor::Green,
            NamedColor::Yellow => TColor::Yellow,
            NamedColor::Blue => TColor::Blue,
            NamedColor::Magenta => TColor::Magenta,
            NamedColor::Cyan => TColor::Cyan,
            NamedColor::White => TColor::White,
            NamedColor::BrightBlack => TColor::BrightBlack,
            NamedColor::BrightRed => TColor::BrightRed,
            NamedColor::BrightGreen => TColor::BrightGreen,
            NamedColor::BrightYellow => TColor::BrightYellow,
            NamedColor::BrightBlue => TColor::BrightBlue,
            NamedColor::BrightMagenta => TColor::BrightMagenta,
            NamedColor::BrightCyan => TColor::BrightCyan,
            NamedColor::BrightWhite => TColor::BrightWhite,
            _ => TColor::Foreground,
        },
        AColor::Spec(rgb) => TColor::Rgb {
            rgba: rgba_u32(rgb.r, rgb.g, rgb.b),
        },
        AColor::Indexed(i) => match i {
            0 => TColor::Black,
            1 => TColor::Red,
            2 => TColor::Green,
            3 => TColor::Yellow,
            4 => TColor::Blue,
            5 => TColor::Magenta,
            6 => TColor::Cyan,
            7 => TColor::White,
            8 => TColor::BrightBlack,
            9 => TColor::BrightRed,
            10 => TColor::BrightGreen,
            11 => TColor::BrightYellow,
            12 => TColor::BrightBlue,
            13 => TColor::BrightMagenta,
            14 => TColor::BrightCyan,
            15 => TColor::BrightWhite,
            16..=231 => {
                // 6x6x6 カラーキューブ
                let i = i - 16;
                let to = |v: u8| -> u8 {
                    if v == 0 {
                        0
                    } else {
                        55 + 40 * v
                    }
                };
                let r = to((i / 36) % 6);
                let g = to((i / 6) % 6);
                let b = to(i % 6);
                TColor::Rgb { rgba: rgba_u32(r, g, b) }
            }
            _ => {
                // グレースケール 232..=255
                let v = 8 + 10 * (i - 232);
                TColor::Rgb { rgba: rgba_u32(v, v, v) }
            }
        },
    }
}

fn rgba_u32(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | 0xff
}

fn pty_size(cols: usize, lines: usize, cell_w: u16, cell_h: u16) -> PtySize {
    PtySize {
        rows: lines as u16,
        cols: cols as u16,
        pixel_width: cols as u16 * cell_w,
        pixel_height: lines as u16 * cell_h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 共有 Vec に書き出すテスト用 Writer。
    struct VecWriter(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for VecWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn dummy_writer() -> (SharedWriter, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer: SharedWriter = Arc::new(Mutex::new(Box::new(VecWriter(buf.clone()))));
        (writer, buf)
    }

    #[test]
    fn window_size_carries_cells_and_pixels() {
        let ws = window_size(80, 24, 9, 18);
        assert_eq!(ws.num_cols, 80);
        assert_eq!(ws.num_lines, 24);
        assert_eq!(ws.cell_width, 9);
        assert_eq!(ws.cell_height, 18);
    }

    fn segs_to_debug(segs: &[Seg]) -> Vec<(char, String)> {
        segs.iter()
            .map(|s| match s {
                Seg::Pass(b) => ('P', String::from_utf8_lossy(b).into_owned()),
                Seg::Sixel(b) => ('S', String::from_utf8_lossy(b).into_owned()),
            })
            .collect()
    }

    #[test]
    fn splitter_extracts_sixel_between_text() {
        let mut sp = SixelSplitter::default();
        let segs = sp.feed(b"hi\x1bP0;1;0q#0;2;100;0;0~~~\x1b\\bye");
        assert_eq!(
            segs_to_debug(&segs),
            vec![
                ('P', "hi".to_string()),
                ('S', "#0;2;100;0;0~~~".to_string()),
                ('P', "bye".to_string()),
            ]
        );
    }

    #[test]
    fn splitter_passes_non_sixel_dcs_through() {
        // DECRQSS ($q 中間バイトつき) は Sixel ではないのでそのまま素通し
        let mut sp = SixelSplitter::default();
        let segs = sp.feed(b"\x1bP$qm\x1b\\");
        assert_eq!(
            segs_to_debug(&segs),
            vec![('P', "\x1bP$qm\x1b\\".to_string())]
        );
    }

    #[test]
    fn splitter_handles_sixel_split_across_chunks() {
        let mut sp = SixelSplitter::default();
        let mut got = Vec::new();
        got.extend(segs_to_debug(&sp.feed(b"\x1bPq#0~~")));
        got.extend(segs_to_debug(&sp.feed(b"~-?\x1b\\done")));
        assert_eq!(
            got,
            vec![('S', "#0~~~-?".to_string()), ('P', "done".to_string())]
        );
    }

    #[test]
    fn responds_to_text_area_pixel_size_query() {
        use alacritty_terminal::term::{Config, Term};
        use vte::ansi::Processor;

        let (writer, buf) = dummy_writer();
        let winsize = Arc::new(Mutex::new(window_size(80, 24, 9, 18)));
        let proxy = EventProxy { writer, winsize };

        let size = GridSize { cols: 80, lines: 24 };
        let mut term = Term::new(Config::default(), &size, proxy);
        let mut processor: Processor = Processor::new();

        // CSI 14 t = テキスト領域をピクセルで報告させる → 応答 CSI 4 ; H ; W t
        processor.advance(&mut term, b"\x1b[14t");
        let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        // 幅=80*9=720, 高さ=24*18=432
        assert_eq!(out, "\x1b[4;432;720t", "CSI 14t の応答が想定と違う");
    }

    #[test]
    fn place_sixel_stores_image_and_advances_cursor() {
        use alacritty_terminal::term::{Config, Term};
        use vte::ansi::Processor;

        let (writer, _buf) = dummy_writer();
        let winsize = Arc::new(Mutex::new(window_size(80, 24, 10, 20)));
        let proxy = EventProxy {
            writer,
            winsize: winsize.clone(),
        };
        let term = Arc::new(Mutex::new(Term::new(
            Config::default(),
            &GridSize { cols: 80, lines: 24 },
            proxy,
        )));
        let images: Arc<Mutex<Vec<PositionedImage>>> = Arc::new(Mutex::new(Vec::new()));
        let mut processor = Processor::new();

        // chafa 風のペイロード（ラスタ属性 20x12, 赤を3画素）
        let payload = b"\"1;1;20;12#0;2;100;0;0#0~~~";
        place_sixel(payload, &term, &images, &winsize, &mut processor);

        let imgs = images.lock().unwrap();
        assert_eq!(imgs.len(), 1, "画像が1枚登録される");
        assert_eq!(imgs[0].width, 20);
        assert!(imgs[0].height >= 12);
        // RGB(3byte) × width × height
        assert_eq!(
            imgs[0].data.len(),
            (imgs[0].width * imgs[0].height * 3) as usize
        );
        // カーソルは画像の高さ分(12px/20px=1行)だけ下がっている
        let row = term.lock().unwrap().grid().cursor.point.line.0;
        assert!(row >= 1, "カーソルが画像の下へ送られている (row={row})");
    }

    #[test]
    fn scrollback_shows_history_after_scroll() {
        use alacritty_terminal::grid::Scroll;
        use alacritty_terminal::term::{Config, Term};
        use vte::ansi::Processor;

        let (writer, _buf) = dummy_writer();
        let winsize = Arc::new(Mutex::new(window_size(20, 5, 10, 20)));
        let proxy = EventProxy { writer, winsize };
        let mut term = Term::new(Config::default(), &GridSize { cols: 20, lines: 5 }, proxy);
        let mut processor: Processor = Processor::new();

        // 画面(5行)より多い 20 行を出力 → 履歴に積まれる
        let mut data = Vec::new();
        for n in 0..20 {
            data.extend_from_slice(format!("L{n}\r\n").as_bytes());
        }
        processor.advance(&mut term, &data);

        // snapshot と同じく「表示行 = グリッド行 + display_offset」で画面トップ(行0)を読む。
        // スクロールした履歴は display_iter 上では負の行番号で来るため、offset を足す。
        let top_line = |term: &Term<EventProxy>| -> String {
            let offset = term.grid().display_offset() as i32;
            let content = term.renderable_content();
            let mut s = String::new();
            for ind in content.display_iter {
                if ind.point.line.0 + offset == 0 {
                    s.push(ind.c);
                }
            }
            s.trim_end().to_string()
        };

        // Delta(正) = 過去(上)方向。3行さかのぼると画面トップが変わる。
        let before = top_line(&term);
        term.scroll_display(Scroll::Delta(3));
        let after = top_line(&term);
        assert_ne!(before, after, "スクロールで画面トップ行が変わるはず");

        // 最上部まで遡ると先頭行 L0 が画面トップに来る。
        term.scroll_display(Scroll::Delta(1000));
        assert_eq!(top_line(&term), "L0", "最上部で先頭行 L0 が画面トップ");
    }

    #[test]
    fn color_index_cube_and_grayscale_are_config_independent() {
        // 6x6x6 キューブ: 16=黒, 231=白
        assert_eq!(color_index_to_rgb(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(color_index_to_rgb(231), Rgb { r: 255, g: 255, b: 255 });
        // 196 = 赤(5,0,0) -> (255,0,0)
        assert_eq!(color_index_to_rgb(196), Rgb { r: 255, g: 0, b: 0 });
        // グレースケール: 232=8, 255=238（v=8+10*23）
        assert_eq!(color_index_to_rgb(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(color_index_to_rgb(255), Rgb { r: 238, g: 238, b: 238 });
    }
}
