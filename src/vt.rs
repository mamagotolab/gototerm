//! alacritty_terminal を VT エンジンとして駆動する新しい端末コア。
//!
//! 自作パーサ（control_function）の代わりに `vte::ansi::Processor` で解析し、
//! グリッド・モード・スクロールバック・応答シーケンスを `Term` に委ねる。
//! PTY は portable-pty（Unix=openpty / Windows=ConPTY）。

use std::io::{Read as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as AlacEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize};
use vte::ansi::Processor;

/// PTY master への書き込み口。入力と応答(DA/DSR等)の両方が使うため共有する。
pub type SharedWriter = Arc<Mutex<Box<dyn std::io::Write + Send>>>;

/// alacritty の Term が応答シーケンスを送るときに呼ばれるリスナー。
/// `PtyWrite` を PTY master に書き戻すことで DA/DSR 等を自動処理する。
#[derive(Clone)]
pub struct EventProxy {
    writer: SharedWriter,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacEvent) {
        if let AlacEvent::PtyWrite(text) = event {
            let _ = self.writer.lock().unwrap().write_all(text.as_bytes());
        }
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
        for (key, val) in std::env::vars() {
            cmd.env(key, val);
        }
        // alacritty_terminal は xterm 互換なので xterm-256color を名乗る
        cmd.env("TERM", "xterm-256color");
        cmd.cwd(cwd);

        let mut child = pair.slave.spawn_command(cmd).expect("spawn shell");
        let killer = child.clone_killer();
        let reader = pair.master.try_clone_reader().expect("pty reader");
        let writer: SharedWriter =
            Arc::new(Mutex::new(pair.master.take_writer().expect("pty writer")));
        let master = pair.master;
        drop(pair.slave); // slave を閉じ、子終了時に reader が EOF を受け取れるように

        let proxy = EventProxy {
            writer: writer.clone(),
        };
        let size = GridSize { cols, lines };
        let term = Arc::new(Mutex::new(Term::new(Config::default(), &size, proxy)));

        let exited = Arc::new(AtomicBool::new(false));

        // 読取スレッド：PTY 出力を Processor に流し込む
        {
            let term = term.clone();
            let exited = exited.clone();
            std::thread::spawn(move || {
                let mut processor: Processor = Processor::new();
                let mut reader = reader;
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF: 子プロセス終了
                        Ok(n) => {
                            let mut term = term.lock().unwrap();
                            processor.advance(&mut *term, &buf[..n]);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                exited.store(true, Ordering::SeqCst);
            });
        }

        // 子プロセスを回収するスレッド
        std::thread::spawn(move || {
            let _ = child.wait();
        });

        VtTerminal {
            term,
            writer,
            master,
            killer,
            exited,
        }
    }

    /// ユーザー入力などを PTY master に書く。
    pub fn write(&self, data: &[u8]) {
        let _ = self.writer.lock().unwrap().write_all(data);
    }

    /// 端末サイズを変更する（カーネル側＋グリッド側）。
    pub fn resize(&self, cols: usize, lines: usize, cell_w: u16, cell_h: u16) {
        let _ = self.master.resize(pty_size(cols, lines, cell_w, cell_h));
        self.term.lock().unwrap().resize(GridSize { cols, lines });
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
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::term::cell::Flags;
use vte::ansi::{Color as AColor, CursorShape, NamedColor};

/// 1フレーム分の描画スナップショット。
pub struct Snapshot {
    pub lines: Vec<TLine>,
    pub cursor: Option<TCursor>,
    pub columns: usize,
    pub screen_lines: usize,
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

        let content = term.renderable_content();

        for indexed in content.display_iter {
            let line = indexed.point.line.0; // 可視領域は 0..screen_lines
            let col = indexed.point.column.0;
            if line < 0 || line as usize >= screen_lines || col >= columns {
                continue;
            }
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
                let row = rc.point.line.0;
                if row < 0 {
                    None
                } else {
                    Some(TCursor::at(row as usize, rc.point.column.0, style))
                }
            }
        };

        Snapshot {
            lines,
            cursor,
            columns,
            screen_lines,
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
