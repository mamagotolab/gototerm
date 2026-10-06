//! alacritty_terminal を VT エンジンとして駆動する新しい端末コア。
//!
//! 自作パーサ（control_function）の代わりに `vte::ansi::Processor` で解析し、
//! グリッド・モード・スクロールバック・応答シーケンスを `Term` に委ねる。
//! PTY は portable-pty（Unix=openpty / Windows=ConPTY）。

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection as AlacSelection, SelectionType};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Rgb;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize};
use vte::ansi::Processor;

use crate::gt::{parse_gt_message, GtMessage};
use crate::sixel;
use crate::terminal::PositionedImage;

/// PTY master への書き込み口。入力と応答(DA/DSR等)の両方が使うため共有する。
pub type SharedWriter = Arc<Mutex<Box<dyn std::io::Write + Send>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellLocation {
    Local(PathBuf),
    Remote { host: String, path: PathBuf },
}

/// Scrollback-grid selection endpoints. Lines are absolute grid coordinates:
/// history is negative and the live screen starts at line zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GridSelection {
    pub(crate) start: Point,
    pub(crate) end: Point,
    pub(crate) block: bool,
}

#[derive(Clone, Copy, Debug)]
struct ExpandedSelection {
    selection_type: SelectionType,
    anchor_left: Column,
    anchor_right: Column,
    forward: Option<bool>,
}

fn selection_point_is_valid<T>(term: &Term<T>, point: Point) -> bool {
    point.line >= term.topmost_line()
        && point.line <= term.bottommost_line()
        && point.column.0 < term.columns()
}

fn pixel_selection_point<T>(
    term: &Term<T>,
    x: f64,
    y: f64,
    cell_width: u32,
    cell_height: u32,
) -> Option<(Point, Side)> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }

    let rows = term.screen_lines();
    let columns = term.columns();
    if rows == 0 || columns == 0 {
        return None;
    }

    let width = cell_width.max(1) as f64;
    let height = cell_height.max(1) as f64;
    let x = x.clamp(0.0, width * columns as f64 - 0.1);
    let screen_line = (y / height).floor().clamp(0.0, (rows - 1) as f64) as usize;
    let cell_x = x / width;
    let column = (cell_x.floor() as usize).min(columns - 1);
    let side = if cell_x.fract() < 0.5 {
        Side::Left
    } else {
        Side::Right
    };
    let line = i32::try_from(screen_line)
        .ok()?
        .checked_sub(i32::try_from(term.grid().display_offset()).ok()?)?;
    let point = Point::new(Line(line), Column(column));

    selection_point_is_valid(term, point).then_some((point, side))
}

fn physical_selection_bounds<T>(
    term: &Term<T>,
    selection_type: SelectionType,
    point: Point,
) -> Option<(Point, Point)> {
    if !selection_point_is_valid(term, point) {
        return None;
    }

    let last_column = term.last_column();
    if selection_type == SelectionType::Lines {
        return Some((
            Point::new(point.line, Column(0)),
            Point::new(point.line, last_column),
        ));
    }

    let row = &term.grid()[point.line];
    let delimiter = |column: Column| {
        let ch = row[column].c;
        ch.is_ascii_punctuation() || ch.is_ascii_whitespace()
    };
    if delimiter(point.column) {
        return Some((point, point));
    }

    let mut left = point.column;
    while left > 0 && !delimiter(left - 1) {
        left -= 1;
    }

    let mut right = point.column;
    while right < last_column && !delimiter(right + 1) {
        right += 1;
    }

    Some((Point::new(point.line, left), Point::new(point.line, right)))
}

fn start_selection_locked<T>(
    term: &mut Term<T>,
    drag: &mut Option<ExpandedSelection>,
    selection_type: SelectionType,
    point: Point,
    side: Side,
) -> bool {
    if !selection_point_is_valid(term, point) {
        term.selection = None;
        *drag = None;
        return false;
    }

    if matches!(
        selection_type,
        SelectionType::Semantic | SelectionType::Lines
    ) {
        let Some((start, end)) = physical_selection_bounds(term, selection_type, point) else {
            term.selection = None;
            *drag = None;
            return false;
        };
        let mut selection = AlacSelection::new(SelectionType::Simple, start, Side::Left);
        selection.update(end, Side::Right);
        term.selection = Some(selection);
        *drag = Some(ExpandedSelection {
            selection_type,
            anchor_left: start.column,
            anchor_right: end.column,
            forward: None,
        });
    } else {
        term.selection = Some(AlacSelection::new(selection_type, point, side));
        *drag = None;
    }

    true
}

fn update_selection_locked<T>(
    term: &mut Term<T>,
    drag: &mut Option<ExpandedSelection>,
    point: Point,
    side: Side,
) -> bool {
    if !selection_point_is_valid(term, point) {
        *drag = None;
        return false;
    }

    let Some(mut expanded) = *drag else {
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
            return true;
        }
        return false;
    };
    let Some(range) = term
        .selection
        .as_ref()
        .and_then(|selection| selection.to_range(term))
    else {
        *drag = None;
        return false;
    };

    let anchor_line = match expanded.forward {
        Some(false) => range.end.line,
        Some(true) | None => range.start.line,
    };
    let Some((current_start, current_end)) =
        physical_selection_bounds(term, expanded.selection_type, point)
    else {
        *drag = None;
        return false;
    };
    let forward = current_start.line > anchor_line
        || (current_start.line == anchor_line && current_start.column >= expanded.anchor_left);

    let mut selection = if forward {
        AlacSelection::new(
            SelectionType::Simple,
            Point::new(anchor_line, expanded.anchor_left),
            Side::Left,
        )
    } else {
        AlacSelection::new(
            SelectionType::Simple,
            Point::new(anchor_line, expanded.anchor_right),
            Side::Right,
        )
    };
    if forward {
        selection.update(current_end, Side::Right);
    } else {
        selection.update(current_start, Side::Left);
    }
    term.selection = Some(selection);
    expanded.forward = Some(forward);
    *drag = Some(expanded);

    true
}

fn selection_text_from_term<T>(term: &Term<T>, selection: GridSelection) -> String {
    let in_bounds = |point: Point| {
        point.line >= term.topmost_line()
            && point.line <= term.bottommost_line()
            && point.column.0 < term.columns()
    };
    if !in_bounds(selection.start) || !in_bounds(selection.end) {
        return String::new();
    }

    if selection.block {
        let top = selection.start.line.0.min(selection.end.line.0);
        let bottom = selection.start.line.0.max(selection.end.line.0);
        let left = selection.start.column.0.min(selection.end.column.0);
        let right = selection.start.column.0.max(selection.end.column.0);

        (top..=bottom)
            .map(|line| {
                term.bounds_to_string(
                    Point::new(Line(line), Column(left)),
                    Point::new(Line(line), Column(right)),
                )
                .trim_end_matches([' ', '\t'])
                .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        let (start, end) = if selection.start <= selection.end {
            (selection.start, selection.end)
        } else {
            (selection.end, selection.start)
        };
        term.bounds_to_string(start, end)
    }
}

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
    #[cfg(windows)]
    _state_pipe: Option<crate::state_pipe::StatePipe>,
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
    gt_messages: Arc<Mutex<Vec<GtMessage>>>,
    /// 直近の代替画面(Alt Screen)状態。切替時に画像を消すため。
    last_alt: Arc<AtomicBool>,
    /// シェル（PTY 子プロセス）の PID。`cwd()` で /proc を引くために保持。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    child_pid: Option<u32>,
    shell_location: Arc<Mutex<Option<ShellLocation>>>,
    selection_drag: Mutex<Option<ExpandedSelection>>,
}

fn window_size(cols: usize, lines: usize, cell_w: u16, cell_h: u16) -> WindowSize {
    WindowSize {
        num_cols: cols as u16,
        num_lines: lines as u16,
        cell_width: cell_w,
        cell_height: cell_h,
    }
}

fn selection_delimiters() -> String {
    (0u8..=127)
        .map(char::from)
        .filter(|ch| ch.is_ascii_punctuation() || ch.is_ascii_whitespace())
        .collect()
}

/// PTY バイト列を分割した断片。
enum Seg {
    /// 通常の VT 列。alacritty の Processor へ流す。
    Pass { source_offset: u64, bytes: Vec<u8> },
    /// Sixel の本体（`ESC P …q` と ST を除いた中身）。自前で描画する。
    Sixel(Vec<u8>),
    /// gototerm が読む OSC。OSC 7717 は Phase 8a では抽出だけ行う。
    Osc { code: OscCode, payload: String },
    /// 内容を破棄した OSC。診断上のストリーム境界だけを残す。
    Boundary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OscCode {
    Cwd,
    Gt,
}

const OSC_MAX_BYTES: usize = 1024 * 1024;

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
    OscCode,
    OscData,
    OscEsc,
    OscPass,
    OscPassEsc,
}

/// PTY バイト列から Sixel(DCS) を抜き出す状態機械。チャンクをまたいで状態を保つ。
/// Sixel 以外の DCS（DECRQSS 等）はそのまま Pass に通す。
#[derive(Default)]
struct SixelSplitter {
    state: SplitState,
    intro: Vec<u8>,
    not_sixel: bool,
    payload: Vec<u8>,
    osc_code: Vec<u8>,
    osc_kind: Option<OscCode>,
    osc_discard: bool,
    raw_offset: u64,
}

impl SixelSplitter {
    fn push_pass_byte(
        pass: &mut Vec<u8>,
        pass_offset: &mut Option<u64>,
        source_offset: u64,
        byte: u8,
    ) {
        if pass.is_empty() {
            *pass_offset = Some(source_offset);
        }
        pass.push(byte);
    }

    fn push_pass_slice(
        pass: &mut Vec<u8>,
        pass_offset: &mut Option<u64>,
        source_offset: u64,
        bytes: &[u8],
    ) {
        if bytes.is_empty() {
            return;
        }
        if pass.is_empty() {
            *pass_offset = Some(source_offset);
        }
        pass.extend_from_slice(bytes);
    }

    fn flush_pass(segs: &mut Vec<Seg>, pass: &mut Vec<u8>, pass_offset: &mut Option<u64>) {
        if pass.is_empty() {
            return;
        }
        segs.push(Seg::Pass {
            source_offset: pass_offset.take().expect("pass bytes have a source offset"),
            bytes: std::mem::take(pass),
        });
    }

    fn feed(&mut self, input: &[u8]) -> Vec<Seg> {
        let mut segs: Vec<Seg> = Vec::new();
        let mut pass: Vec<u8> = Vec::new();
        let mut pass_offset = None;

        for &b in input {
            let source_offset = self.raw_offset;
            self.raw_offset = self.raw_offset.saturating_add(1);
            match self.state {
                SplitState::Normal => {
                    if b == 0x1b {
                        self.state = SplitState::Esc;
                    } else {
                        Self::push_pass_byte(&mut pass, &mut pass_offset, source_offset, b);
                    }
                }
                SplitState::Esc => {
                    if b == b'P' {
                        self.state = SplitState::DcsIntro;
                        self.intro.clear();
                        self.not_sixel = false;
                    } else if b == b']' {
                        self.state = SplitState::OscCode;
                        self.osc_code.clear();
                        self.osc_kind = None;
                        self.osc_discard = false;
                    } else {
                        Self::push_pass_byte(
                            &mut pass,
                            &mut pass_offset,
                            source_offset.saturating_sub(1),
                            0x1b,
                        );
                        if b == 0x1b {
                            // ESC ESC: 2つ目を新たな ESC として扱う
                        } else {
                            Self::push_pass_byte(&mut pass, &mut pass_offset, source_offset, b);
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
                            let intro_offset = source_offset
                                .saturating_sub(self.intro.len() as u64)
                                .saturating_sub(2);
                            let mut intro = Vec::with_capacity(self.intro.len() + 3);
                            intro.extend_from_slice(b"\x1bP");
                            intro.extend_from_slice(&self.intro);
                            intro.push(b);
                            Self::push_pass_slice(
                                &mut pass,
                                &mut pass_offset,
                                intro_offset,
                                &intro,
                            );
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
                        Self::flush_pass(&mut segs, &mut pass, &mut pass_offset);
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
                    Self::flush_pass(&mut segs, &mut pass, &mut pass_offset);
                    segs.push(Seg::Sixel(std::mem::take(&mut self.payload)));
                    self.state = SplitState::Normal;
                    if b == 0x1b {
                        self.state = SplitState::Esc;
                    } else if b != b'\\' {
                        Self::push_pass_byte(&mut pass, &mut pass_offset, source_offset, b);
                    }
                }
                SplitState::DcsPass => {
                    Self::push_pass_byte(&mut pass, &mut pass_offset, source_offset, b);
                    if b == 0x07 {
                        self.state = SplitState::Normal;
                    } else if b == 0x1b {
                        self.state = SplitState::DcsPassEsc;
                    }
                }
                SplitState::DcsPassEsc => {
                    Self::push_pass_byte(&mut pass, &mut pass_offset, source_offset, b);
                    self.state = if b == b'\\' {
                        SplitState::Normal
                    } else {
                        SplitState::DcsPass
                    };
                }
                SplitState::OscCode => {
                    if b == b';' {
                        self.osc_kind = match self.osc_code.as_slice() {
                            b"7" => Some(OscCode::Cwd),
                            b"7717" => Some(OscCode::Gt),
                            _ => None,
                        };
                        if self.osc_kind.is_some() {
                            self.payload.clear();
                            self.state = SplitState::OscData;
                        } else {
                            let osc_offset = source_offset
                                .saturating_sub(self.osc_code.len() as u64)
                                .saturating_sub(2);
                            let mut prefix = Vec::with_capacity(self.osc_code.len() + 3);
                            prefix.extend_from_slice(b"\x1b]");
                            prefix.extend_from_slice(&self.osc_code);
                            prefix.push(b';');
                            Self::push_pass_slice(&mut pass, &mut pass_offset, osc_offset, &prefix);
                            self.state = SplitState::OscPass;
                        }
                    } else if b == 0x07 {
                        let osc_offset = source_offset
                            .saturating_sub(self.osc_code.len() as u64)
                            .saturating_sub(2);
                        let mut sequence = Vec::with_capacity(self.osc_code.len() + 3);
                        sequence.extend_from_slice(b"\x1b]");
                        sequence.extend_from_slice(&self.osc_code);
                        sequence.push(b);
                        Self::push_pass_slice(&mut pass, &mut pass_offset, osc_offset, &sequence);
                        self.state = SplitState::Normal;
                    } else if b == 0x1b {
                        let osc_offset = source_offset
                            .saturating_sub(self.osc_code.len() as u64)
                            .saturating_sub(2);
                        let mut sequence = Vec::with_capacity(self.osc_code.len() + 3);
                        sequence.extend_from_slice(b"\x1b]");
                        sequence.extend_from_slice(&self.osc_code);
                        sequence.push(b);
                        Self::push_pass_slice(&mut pass, &mut pass_offset, osc_offset, &sequence);
                        self.state = SplitState::OscPassEsc;
                    } else if b.is_ascii_digit() {
                        self.osc_code.push(b);
                    } else {
                        let osc_offset = source_offset
                            .saturating_sub(self.osc_code.len() as u64)
                            .saturating_sub(2);
                        let mut sequence = Vec::with_capacity(self.osc_code.len() + 3);
                        sequence.extend_from_slice(b"\x1b]");
                        sequence.extend_from_slice(&self.osc_code);
                        sequence.push(b);
                        Self::push_pass_slice(&mut pass, &mut pass_offset, osc_offset, &sequence);
                        self.state = SplitState::OscPass;
                    }
                }
                SplitState::OscData => {
                    if b == 0x07 {
                        Self::flush_pass(&mut segs, &mut pass, &mut pass_offset);
                        segs.push(self.finish_osc());
                        self.state = SplitState::Normal;
                    } else if b == 0x1b {
                        self.state = SplitState::OscEsc;
                    } else {
                        self.push_osc_payload(b);
                    }
                }
                SplitState::OscEsc => {
                    if b == b'\\' {
                        Self::flush_pass(&mut segs, &mut pass, &mut pass_offset);
                        segs.push(self.finish_osc());
                        self.state = SplitState::Normal;
                    } else {
                        self.push_osc_payload(0x1b);
                        self.push_osc_payload(b);
                        self.state = SplitState::OscData;
                    }
                }
                SplitState::OscPass => {
                    Self::push_pass_byte(&mut pass, &mut pass_offset, source_offset, b);
                    if b == 0x07 {
                        self.state = SplitState::Normal;
                    } else if b == 0x1b {
                        self.state = SplitState::OscPassEsc;
                    }
                }
                SplitState::OscPassEsc => {
                    Self::push_pass_byte(&mut pass, &mut pass_offset, source_offset, b);
                    self.state = if b == b'\\' {
                        SplitState::Normal
                    } else {
                        SplitState::OscPass
                    };
                }
            }
        }
        Self::flush_pass(&mut segs, &mut pass, &mut pass_offset);
        segs
    }

    fn push_osc_payload(&mut self, b: u8) {
        if self.osc_discard {
            return;
        }
        if self.payload.len() >= OSC_MAX_BYTES {
            self.payload.clear();
            self.osc_discard = true;
        } else {
            self.payload.push(b);
        }
    }

    fn finish_osc(&mut self) -> Seg {
        if self.osc_discard {
            self.payload.clear();
            return Seg::Boundary;
        }
        let Some(code) = self.osc_kind else {
            self.payload.clear();
            return Seg::Boundary;
        };
        let bytes = std::mem::take(&mut self.payload);
        match String::from_utf8(bytes) {
            Ok(payload) => Seg::Osc { code, payload },
            Err(_) => Seg::Boundary,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DiagnosticModes {
    application_cursor: bool,
    alternate_screen: bool,
    mouse: bool,
}

impl DiagnosticModes {
    fn from_term<T>(term: &Term<T>) -> Self {
        use alacritty_terminal::term::TermMode;

        let mode = term.mode();
        Self {
            application_cursor: mode.contains(TermMode::APP_CURSOR),
            alternate_screen: mode.contains(TermMode::ALT_SCREEN),
            mouse: mode.intersects(TermMode::MOUSE_MODE),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Utf8Issue {
    offset: u64,
    modes: DiagnosticModes,
}

impl std::fmt::Debug for Utf8Issue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("Utf8Issue")
            .field(&self.offset)
            .field(&self.modes)
            .finish()
    }
}

struct Utf8Diagnostic {
    enabled: bool,
    pending: [u8; 3],
    pending_len: u8,
    pending_modes: Option<DiagnosticModes>,
    stream_offset: u64,
    invalid_count: u64,
    incomplete_count: u64,
}

impl Utf8Diagnostic {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            pending: [0; 3],
            pending_len: 0,
            pending_modes: None,
            stream_offset: 0,
            invalid_count: 0,
            incomplete_count: 0,
        }
    }

    fn observe_at(
        &mut self,
        source_offset: u64,
        bytes: &[u8],
        modes: DiagnosticModes,
    ) -> Vec<Utf8Issue> {
        if !self.enabled {
            return Vec::new();
        }

        let mut issues = if source_offset == self.stream_offset {
            Vec::new()
        } else {
            self.finish()
        };
        self.stream_offset = source_offset;
        issues.extend(self.observe(bytes, modes));
        issues
    }

    fn observe(&mut self, bytes: &[u8], modes: DiagnosticModes) -> Vec<Utf8Issue> {
        if !self.enabled || bytes.is_empty() {
            return Vec::new();
        }

        let pending_len = usize::from(self.pending_len);
        let carried_modes = self.pending_modes.take();
        let call_offset = self.stream_offset;
        let base_offset = self.stream_offset.saturating_sub(pending_len as u64);
        self.stream_offset = self.stream_offset.saturating_add(bytes.len() as u64);

        let mut input = Vec::with_capacity(pending_len + bytes.len());
        input.extend_from_slice(&self.pending[..pending_len]);
        input.extend_from_slice(bytes);
        self.pending = [0; 3];
        self.pending_len = 0;

        let mut issues = Vec::new();
        let mut cursor = 0;
        while cursor < input.len() {
            match std::str::from_utf8(&input[cursor..]) {
                Ok(_) => break,
                Err(error) => {
                    cursor += error.valid_up_to();
                    match error.error_len() {
                        Some(invalid_len) => {
                            let offset = base_offset.saturating_add(cursor as u64);
                            let issue = Utf8Issue {
                                offset,
                                modes: if offset < call_offset {
                                    carried_modes.unwrap_or(modes)
                                } else {
                                    modes
                                },
                            };
                            self.invalid_count = self.invalid_count.saturating_add(1);
                            log::warn!(
                                "UTF-8 diagnostic: invalid_sequence_count={} offset={} application_cursor={} alternate_screen={} mouse={}",
                                self.invalid_count,
                                issue.offset,
                                issue.modes.application_cursor,
                                issue.modes.alternate_screen,
                                issue.modes.mouse,
                            );
                            issues.push(issue);
                            cursor += invalid_len;
                        }
                        None => {
                            let incomplete = &input[cursor..];
                            debug_assert!(incomplete.len() <= self.pending.len());
                            self.pending[..incomplete.len()].copy_from_slice(incomplete);
                            self.pending_len = incomplete.len() as u8;
                            let offset = base_offset.saturating_add(cursor as u64);
                            self.pending_modes = Some(if offset < call_offset {
                                carried_modes.unwrap_or(modes)
                            } else {
                                modes
                            });
                            break;
                        }
                    }
                }
            }
        }

        issues
    }

    fn finish(&mut self) -> Vec<Utf8Issue> {
        if !self.enabled || self.pending_len == 0 {
            return Vec::new();
        }

        let issue = Utf8Issue {
            offset: self
                .stream_offset
                .saturating_sub(u64::from(self.pending_len)),
            modes: self
                .pending_modes
                .take()
                .expect("pending UTF-8 prefix has terminal modes"),
        };
        self.pending = [0; 3];
        self.pending_len = 0;
        self.incomplete_count = self.incomplete_count.saturating_add(1);
        log::warn!(
            "UTF-8 diagnostic: incomplete_sequence_count={} offset={} application_cursor={} alternate_screen={} mouse={}",
            self.incomplete_count,
            issue.offset,
            issue.modes.application_cursor,
            issue.modes.alternate_screen,
            issue.modes.mouse,
        );
        vec![issue]
    }
}

fn advance_terminal(processor: &mut Processor, term: &mut Term<EventProxy>, bytes: &[u8]) {
    use alacritty_terminal::grid::Scroll;
    use alacritty_terminal::term::TermMode;
    if !term.mode().contains(TermMode::VI) {
        processor.advance(term, bytes);
        return;
    }
    let cursor = term.vi_mode_cursor;
    let offset = term.grid().display_offset();
    let history = term.history_size();
    let selection = term
        .selection
        .as_ref()
        .and_then(|selection| selection.to_range(term));
    let alt = term.mode().contains(TermMode::ALT_SCREEN);
    processor.advance(term, bytes);
    if alt != term.mode().contains(TermMode::ALT_SCREEN) || !term.mode().contains(TermMode::VI) {
        return;
    }
    let moved = match (
        selection,
        term.selection
            .as_ref()
            .and_then(|selection| selection.to_range(term)),
    ) {
        (Some(before), Some(after)) => before.start.line.0 - after.start.line.0,
        _ => term.history_size().saturating_sub(history) as i32,
    };
    if moved > 0 {
        let desired = (offset + moved as usize).min(term.history_size());
        let delta = desired as i32 - term.grid().display_offset() as i32;
        term.grid_mut().scroll_display(Scroll::Delta(delta));
        term.vi_mode_cursor = cursor;
        term.vi_mode_cursor.point.line = Line(
            (cursor.point.line.0 - moved).clamp(term.topmost_line().0, term.bottommost_line().0),
        );
    }
}

fn advance_pass(
    processor: &mut Processor,
    term: &mut Term<EventProxy>,
    source_offset: u64,
    bytes: &[u8],
    diagnostic: &mut Utf8Diagnostic,
) -> Vec<Utf8Issue> {
    if !diagnostic.enabled {
        advance_terminal(processor, term, bytes);
        return Vec::new();
    }

    let mut issues = Vec::new();
    for (index, byte) in bytes.iter().enumerate() {
        let modes = DiagnosticModes::from_term(term);
        issues.extend(diagnostic.observe_at(
            source_offset.saturating_add(index as u64),
            std::slice::from_ref(byte),
            modes,
        ));
        advance_terminal(processor, term, std::slice::from_ref(byte));
    }
    issues
}

fn advance_pass_or_finish_boundary(
    seg: &Seg,
    processor: &mut Processor,
    term: &Arc<Mutex<Term<EventProxy>>>,
    diagnostic: &mut Utf8Diagnostic,
) -> Vec<Utf8Issue> {
    match seg {
        Seg::Pass {
            source_offset,
            bytes,
        } => advance_pass(
            processor,
            &mut term.lock().unwrap(),
            *source_offset,
            bytes,
            diagnostic,
        ),
        Seg::Sixel(_) | Seg::Osc { .. } | Seg::Boundary => diagnostic.finish(),
    }
}

pub(crate) fn parse_osc7(payload: &str) -> Option<(String, PathBuf)> {
    let rest = payload.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    let mut path = percent_decode_path(path)?;
    // Windows の file URI は "/C:/Users/..." の形になる（PowerShell からの
    // OSC 7 など）。先頭の "/" を剥がさないと存在しないパスとして扱われる。
    let b = path.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        path.remove(0);
    }
    Some((host.to_string(), PathBuf::from(path)))
}

fn percent_decode_path(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = *bytes.get(i + 1)?;
            let lo = *bytes.get(i + 2)?;
            out.push(hex_value(hi)? << 4 | hex_value(lo)?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn local_hostname() -> &'static str {
    static HOSTNAME: OnceLock<String> = OnceLock::new();
    HOSTNAME
        .get_or_init(|| {
            #[cfg(target_os = "linux")]
            {
                std::fs::read_to_string("/proc/sys/kernel/hostname")
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default()
            }
            #[cfg(windows)]
            {
                std::env::var("COMPUTERNAME").unwrap_or_default()
            }
            #[cfg(all(not(target_os = "linux"), not(windows)))]
            {
                String::new()
            }
        })
        .as_str()
}

fn push_gt_message(queue: &Arc<Mutex<Vec<GtMessage>>>, message: GtMessage) {
    const GT_QUEUE_MAX: usize = 1000;
    let mut queue = queue.lock().unwrap();
    if queue.len() >= GT_QUEUE_MAX {
        let drop_count = queue.len() + 1 - GT_QUEUE_MAX;
        queue.drain(0..drop_count);
    }
    queue.push(message);
}

pub(crate) fn classify_shell_location(
    host: &str,
    path: PathBuf,
    local_host: &str,
) -> ShellLocation {
    if host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || (!local_host.is_empty() && host.eq_ignore_ascii_case(local_host))
    {
        ShellLocation::Local(path)
    } else {
        ShellLocation::Remote {
            host: host.to_string(),
            path,
        }
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
        command: Option<&[String]>,
    ) -> Self {
        let _ = local_hostname();
        let gt_messages: Arc<Mutex<Vec<GtMessage>>> = Arc::new(Mutex::new(Vec::new()));
        #[cfg(windows)]
        let state_pipe = crate::state_pipe::StatePipe::new(gt_messages.clone());

        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(pty_size(cols, lines, cell_w, cell_h))
            .expect("openpty");

        // コマンド未指定時は既定シェル。指定が空なら安全側で既定シェルに戻す。
        let mut requested: Vec<String> = command
            .map(<[String]>::to_vec)
            .unwrap_or_else(|| crate::TOYTERM_CONFIG.shell.clone());
        if requested.is_empty() {
            requested = crate::TOYTERM_CONFIG.shell.clone();
        }

        // argv から、環境を整えた CommandBuilder を作る（成功／フォールバックで
        // 同じ環境設定を使うためクロージャに切り出す）。
        let build_cmd = |argv: &[String]| -> CommandBuilder {
            let mut cmd = CommandBuilder::new(&argv[0]);
            for arg in &argv[1..] {
                cmd.arg(arg);
            }
            // 親の環境を引き継ぐが、「別端末の正体」を示す変数は落とす。
            // これらが残ると yazi 等が「kitty/ghostty だから画像を出せる」と誤検出し、
            // 画像プロトコル非対応の gototerm で preview のたびに画面が乱れる。
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
                "GOTOTERM_STATE_PIPE",
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
            #[cfg(windows)]
            if let Some(pipe) = &state_pipe {
                cmd.env("GOTOTERM_STATE_PIPE", &pipe.name);
            }
            cmd.cwd(cwd);
            cmd
        };

        // 指定コマンドの起動に失敗（例: Windows で `claude` が .cmd シムのため
        // CreateProcess が解決できない）しても、落とさず既定シェルへフォールバックする。
        let mut child = match pair.slave.spawn_command(build_cmd(&requested)) {
            Ok(child) => child,
            Err(err) => {
                log::error!("failed to spawn {requested:?}: {err}; falling back to shell");
                pair.slave
                    .spawn_command(build_cmd(&crate::TOYTERM_CONFIG.shell))
                    .expect("spawn shell")
            }
        };
        let killer = child.clone_killer();
        // 回収スレッドに move する前に PID を控える（cwd 追従に使う）。
        let child_pid = child.process_id();
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
        let term_config = Config {
            scrolling_history: crate::TOYTERM_CONFIG.scrollback_lines,
            semantic_escape_chars: selection_delimiters(),
            ..Config::default()
        };
        let term = Arc::new(Mutex::new(Term::new(term_config, &size, proxy)));

        let exited = Arc::new(AtomicBool::new(false));
        let dirty = Arc::new(AtomicBool::new(true));
        let images: Arc<Mutex<Vec<PositionedImage>>> = Arc::new(Mutex::new(Vec::new()));
        let last_alt = Arc::new(AtomicBool::new(false));
        let shell_location = Arc::new(Mutex::new(None));

        // 読取スレッド：PTY 出力を Sixel と通常VTに分け、後者を Processor に流す
        {
            let term = term.clone();
            let exited = exited.clone();
            let dirty = dirty.clone();
            let images = images.clone();
            let gt_messages = gt_messages.clone();
            let winsize = winsize.clone();
            let shell_location = shell_location.clone();
            std::thread::spawn(move || {
                let mut processor: Processor = Processor::new();
                let mut splitter = SixelSplitter::default();
                let diagnostics_enabled = matches!(
                    std::env::var("GOTOTERM_UTF8_DIAGNOSTICS").as_deref(),
                    Ok("1")
                );
                let mut utf8_diagnostic = Utf8Diagnostic::new(diagnostics_enabled);
                let mut reader = reader;
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF: 子プロセス終了
                        Ok(n) => {
                            for seg in splitter.feed(&buf[..n]) {
                                let _ = advance_pass_or_finish_boundary(
                                    &seg,
                                    &mut processor,
                                    &term,
                                    &mut utf8_diagnostic,
                                );
                                match seg {
                                    Seg::Pass { .. } => {}
                                    Seg::Sixel(payload) => {
                                        place_sixel(
                                            &payload,
                                            &term,
                                            &images,
                                            &winsize,
                                            &mut processor,
                                        );
                                    }
                                    Seg::Osc { code, payload } => {
                                        if code == OscCode::Cwd {
                                            if let Some((host, path)) = parse_osc7(&payload) {
                                                let location = classify_shell_location(
                                                    &host,
                                                    path,
                                                    local_hostname(),
                                                );
                                                *shell_location.lock().unwrap() = Some(location);
                                            }
                                        } else if code == OscCode::Gt {
                                            if let Some(message) = parse_gt_message(&payload) {
                                                push_gt_message(&gt_messages, message);
                                            }
                                        }
                                    }
                                    Seg::Boundary => {}
                                }
                            }
                            dirty.store(true, Ordering::SeqCst);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                let _ = utf8_diagnostic.finish();
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
            #[cfg(windows)]
            _state_pipe: state_pipe,
            term,
            writer,
            master,
            killer,
            exited,
            dirty,
            winsize,
            images,
            gt_messages,
            last_alt,
            child_pid,
            shell_location,
            selection_drag: Mutex::new(None),
        }
    }

    /// シェルの現在の作業ディレクトリ。Linux では /proc/<pid>/cwd を読むので
    /// `cd` に追従できる。Windows には相当する安全な手段が無いため None を返す
    /// （Phase 4 の OSC 7 シェル統合で対応予定）。呼び出し側でフォールバックする。
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        match self.location() {
            Some(ShellLocation::Local(path)) => return Some(path),
            Some(ShellLocation::Remote { .. }) => return None,
            None => {}
        }
        #[cfg(target_os = "linux")]
        {
            let pid = self.child_pid?;
            std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    pub fn location(&self) -> Option<ShellLocation> {
        self.shell_location.lock().unwrap().clone()
    }

    pub fn take_gt_messages(&self) -> Vec<GtMessage> {
        std::mem::take(&mut *self.gt_messages.lock().unwrap())
    }

    /// 前回以降に画面内容が変わったか（変わっていれば true を返し、フラグを下げる）。
    pub(crate) fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::SeqCst);
    }

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
        self.term
            .lock()
            .unwrap()
            .mode()
            .intersects(TermMode::MOUSE_MODE)
    }

    /// 代替画面(Alt Screen)中か。nvim/less 等の全画面 TUI で true。
    pub fn alt_screen(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term
            .lock()
            .unwrap()
            .mode()
            .contains(TermMode::ALT_SCREEN)
    }

    pub fn application_cursor_mode(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term
            .lock()
            .unwrap()
            .mode()
            .contains(TermMode::APP_CURSOR)
    }

    pub fn sgr_mouse(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term
            .lock()
            .unwrap()
            .mode()
            .contains(TermMode::SGR_MOUSE)
    }

    pub fn bracketed_paste(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term
            .lock()
            .unwrap()
            .mode()
            .contains(TermMode::BRACKETED_PASTE)
    }

    /// スクロールバック（履歴）を消去する。
    pub fn clear_history(&self) {
        let mut term = self.term.lock().unwrap();
        term.grid_mut().clear_history();
        term.selection = None;
        *self.selection_drag.lock().unwrap() = None;
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// スクロールバック表示を delta 行ぶん動かす（正で過去方向＝上）。
    pub fn scroll(&self, delta: i32) {
        use alacritty_terminal::grid::Scroll;
        let mut term = self.term.lock().unwrap();
        let cursor = term.vi_mode_cursor;
        let selection = term.selection.clone();
        term.scroll_display(Scroll::Delta(delta));
        if term.mode().contains(alacritty_terminal::term::TermMode::VI) {
            term.vi_mode_cursor = cursor;
            term.selection = selection;
        }
        self.dirty.store(true, Ordering::SeqCst);
    }

    pub(crate) fn copy_mode_active(&self) -> bool {
        self.term
            .lock()
            .unwrap()
            .mode()
            .contains(alacritty_terminal::term::TermMode::VI)
    }

    pub(crate) fn toggle_copy_mode(&self) {
        let mut term = self.term.lock().unwrap();
        term.toggle_vi_mode();
        if term.mode().contains(alacritty_terminal::term::TermMode::VI) {
            term.selection = None;
        }
        self.dirty.store(true, Ordering::SeqCst);
    }

    pub(crate) fn copy_mode_motion(&self, motion: alacritty_terminal::vi_mode::ViMotion) {
        self.term.lock().unwrap().vi_motion(motion);
        self.dirty.store(true, Ordering::SeqCst);
    }

    pub(crate) fn copy_mode_edge(&self, oldest: bool) {
        let mut term = self.term.lock().unwrap();
        let line = if oldest {
            term.topmost_line()
        } else {
            term.bottommost_line()
        };
        let column = term.vi_mode_cursor.point.column;
        term.vi_goto_point(Point::new(line, column));
        self.dirty.store(true, Ordering::SeqCst);
    }

    pub(crate) fn copy_mode_page(&self, up: bool) {
        let rows = self.size().1 / 2;
        let motion = if up {
            alacritty_terminal::vi_mode::ViMotion::Up
        } else {
            alacritty_terminal::vi_mode::ViMotion::Down
        };
        for _ in 0..rows.max(1) {
            self.copy_mode_motion(motion);
        }
    }

    pub(crate) fn copy_mode_select(&self, kind: SelectionType) {
        let mut term = self.term.lock().unwrap();
        if term
            .selection
            .as_ref()
            .is_some_and(|selection| selection.ty == kind)
        {
            term.selection = None;
        } else {
            let mut selection = AlacSelection::new(kind, term.vi_mode_cursor.point, Side::Left);
            selection.include_all();
            term.selection = Some(selection);
        }
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// 現在のスクロールバック表示量（行）。0=最下部。選択を絶対行に
    /// 固定してスクロール追従させるために使う。
    pub fn display_offset(&self) -> usize {
        self.term.lock().unwrap().grid().display_offset()
    }

    #[cfg(test)]
    pub(crate) fn start_selection(
        &self,
        selection_type: SelectionType,
        point: Point,
        side: Side,
    ) -> bool {
        let mut term = self.term.lock().unwrap();
        let mut drag = self.selection_drag.lock().unwrap();
        start_selection_locked(&mut term, &mut drag, selection_type, point, side)
    }

    pub(crate) fn start_selection_at_pixel(
        &self,
        selection_type: SelectionType,
        x: f64,
        y: f64,
        cell_width: u32,
        cell_height: u32,
    ) -> bool {
        let mut term = self.term.lock().unwrap();
        let mut drag = self.selection_drag.lock().unwrap();
        let Some((point, side)) = pixel_selection_point(&term, x, y, cell_width, cell_height)
        else {
            term.selection = None;
            *drag = None;
            return false;
        };

        start_selection_locked(&mut term, &mut drag, selection_type, point, side)
    }

    #[cfg(test)]
    pub(crate) fn update_selection(&self, point: Point, side: Side) -> bool {
        let mut term = self.term.lock().unwrap();
        let mut drag = self.selection_drag.lock().unwrap();
        update_selection_locked(&mut term, &mut drag, point, side)
    }

    pub(crate) fn update_selection_at_pixel(
        &self,
        x: f64,
        y: f64,
        cell_width: u32,
        cell_height: u32,
    ) -> bool {
        let mut term = self.term.lock().unwrap();
        let mut drag = self.selection_drag.lock().unwrap();
        let Some((point, side)) = pixel_selection_point(&term, x, y, cell_width, cell_height)
        else {
            *drag = None;
            return false;
        };

        update_selection_locked(&mut term, &mut drag, point, side)
    }

    pub(crate) fn grid_selection(&self) -> Option<GridSelection> {
        let term = self.term.lock().unwrap();
        let range = term.selection.as_ref()?.to_range(&term)?;
        Some(GridSelection {
            start: range.start,
            end: range.end,
            block: range.is_block,
        })
    }

    pub(crate) fn clear_selection(&self) {
        self.term.lock().unwrap().selection = None;
        *self.selection_drag.lock().unwrap() = None;
    }

    /// Copy the logical grid range without consulting the current viewport.
    #[allow(dead_code)] // Explicit-range API retained for callers/tests; UI copy uses atomic tracking.
    pub(crate) fn selection_text(&self, selection: GridSelection) -> String {
        let term = self.term.lock().unwrap();
        selection_text_from_term(&term, selection)
    }

    pub(crate) fn tracked_selection_text(&self) -> Option<(GridSelection, String)> {
        let term = self.term.lock().unwrap();
        let range = term.selection.as_ref()?.to_range(&term)?;
        let selection = GridSelection {
            start: range.start,
            end: range.end,
            block: range.is_block,
        };
        let text = selection_text_from_term(&term, selection);
        Some((selection, text))
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
        // グリッドを再フローしただけでは PTY 出力が無く dirty が立たないため、
        // 明示的に dirty にして即再描画させる。これが無いと次の出力（Enter 等）まで
        // 旧サイズの画面が残る（Wayland はコンポジタ再描画で隠れていた）。
        self.dirty.store(true, Ordering::SeqCst);
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

use crate::terminal::{
    Cell as TCell, Color as TColor, Cursor as TCursor, CursorStyle, GraphicAttribute, Line as TLine,
};
use alacritty_terminal::term::cell::Flags;
use vte::ansi::{Color as AColor, CursorShape, NamedColor};

/// 表示位置のリンクを端末グリッドから取得する。OSC 8 と自動折り返しを保持する。
fn url_at(term: &Term<EventProxy>, row: usize, col: usize) -> Option<String> {
    if row >= term.screen_lines() || col >= term.columns() {
        return None;
    }
    let grid = term.grid();
    let line = Line(row as i32 - grid.display_offset() as i32);
    let mut point = Point::new(line, Column(col));
    if grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) && col > 0 {
        point.column = Column(col - 1);
    }
    let is_web_url = |url: &str| {
        (url.starts_with("https://") || url.starts_with("http://"))
            && !url.chars().any(char::is_control)
    };
    if let Some(link) = grid[point].hyperlink() {
        return is_web_url(link.uri()).then(|| link.uri().to_owned());
    }

    let last_col = Column(term.columns() - 1);
    let mut first = line;
    while first > term.topmost_line()
        && grid[Line(first.0 - 1)][last_col]
            .flags
            .contains(Flags::WRAPLINE)
    {
        first -= 1;
    }
    let mut last = line;
    while last < term.bottommost_line() && grid[last][last_col].flags.contains(Flags::WRAPLINE) {
        last += 1;
    }
    // 1要素＝画面の1列。全角後半は文字列化するときだけ除外する。
    let mut chars = Vec::new();
    for row in first.0..=last.0 {
        for col in 0..term.columns() {
            let cell = &grid[Line(row)][Column(col)];
            chars.push(
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    '\0'
                } else {
                    cell.c
                },
            );
        }
    }
    let index = (line.0 - first.0) as usize * term.columns() + point.column.0;
    let is_token = |c: char| {
        !c.is_whitespace()
            && !matches!(
                c,
                '"' | '\'' | '<' | '>' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '│'
            )
    };
    if !is_token(chars[index]) {
        return None;
    }
    let mut start = index;
    while start > 0 && is_token(chars[start - 1]) {
        start -= 1;
    }
    let mut end = index + 1;
    while end < chars.len() && is_token(chars[end]) {
        end += 1;
    }
    let token: String = chars[start..end].iter().filter(|c| **c != '\0').collect();
    let token = token.trim_end_matches(['.', ',', ';', ':', '!', '?', '。', '、']);
    is_web_url(token).then(|| token.to_owned())
}

/// 1フレーム分の描画スナップショット。
pub struct Snapshot {
    pub lines: Vec<TLine>,
    pub cursor: Option<TCursor>,
    pub images: Vec<PositionedImage>,
}

impl VtTerminal {
    pub fn url_at(&self, row: usize, col: usize) -> Option<String> {
        url_at(&self.term.lock().unwrap(), row, col)
    }

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
            let width: u16 = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };

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
                (
                    ws.cell_width.max(1) as usize,
                    ws.cell_height.max(1) as usize,
                )
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
                TColor::Rgb {
                    rgba: rgba_u32(r, g, b),
                }
            }
            _ => {
                // グレースケール 232..=255
                let v = 8 + 10 * (i - 232);
                TColor::Rgb {
                    rgba: rgba_u32(v, v, v),
                }
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
    use alacritty_terminal::index::Side;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::selection::SelectionType;
    use std::path::Path;

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

    fn link_test_term(cols: usize, input: &str) -> Term<EventProxy> {
        let (writer, _) = dummy_writer();
        let proxy = EventProxy {
            writer,
            winsize: Arc::new(Mutex::new(window_size(cols, 4, 9, 18))),
        };
        let mut term = Term::new(Config::default(), &GridSize { cols, lines: 4 }, proxy);
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, input.as_bytes());
        term
    }

    #[test]
    fn url_at_plain_mail_text_with_japanese_prefix() {
        let term = link_test_term(80, "本文 <https://example.com/path?q=1&lang=ja> 後続");
        assert_eq!(
            url_at(&term, 0, 6).as_deref(),
            Some("https://example.com/path?q=1&lang=ja")
        );
        assert_eq!(url_at(&term, 0, 3), None);
    }

    #[test]
    fn url_at_in_alternate_screen_with_app_mouse_mode() {
        let term = link_test_term(80, "\x1b[?1049h\x1b[?1000hhttps://example.com");
        assert_eq!(url_at(&term, 0, 5).as_deref(), Some("https://example.com"));
    }

    #[test]
    fn url_at_osc8_label_and_wide_character() {
        let term = link_test_term(
            40,
            "\x1b]8;;https://example.com/path\x1b\\日本語リンク\x1b]8;;\x1b\\ tail",
        );
        for col in 0..12 {
            assert_eq!(
                url_at(&term, 0, col).as_deref(),
                Some("https://example.com/path")
            );
        }
        assert_eq!(url_at(&term, 0, 13), None);
    }

    #[test]
    fn url_at_soft_wrapped_plain_url() {
        let term = link_test_term(16, "https://example.com/long/path");
        assert_eq!(
            url_at(&term, 0, 0).as_deref(),
            Some("https://example.com/long/path")
        );
        assert_eq!(
            url_at(&term, 1, 5).as_deref(),
            Some("https://example.com/long/path")
        );
    }

    #[test]
    fn url_at_does_not_join_hard_newlines() {
        let term = link_test_term(40, "https://example.com\r\n/unrelated");
        assert_eq!(url_at(&term, 0, 0).as_deref(), Some("https://example.com"));
        assert_eq!(url_at(&term, 1, 0), None);
    }

    #[test]
    fn url_at_uses_scrollback_and_rejects_outside_viewport() {
        let mut term = link_test_term(40, "https://example.com\r\n1\r\n2\r\n3\r\n4");
        term.scroll_display(alacritty_terminal::grid::Scroll::Top);
        assert_eq!(url_at(&term, 0, 5).as_deref(), Some("https://example.com"));
        assert_eq!(url_at(&term, 4, 0), None);
        assert_eq!(url_at(&term, 0, 40), None);
    }

    #[test]
    fn url_at_does_not_open_non_web_osc8_targets() {
        let term = link_test_term(40, "\x1b]8;;file:///tmp/example\x1b\\label\x1b]8;;\x1b\\");
        assert_eq!(url_at(&term, 0, 0), None);
    }

    fn render_split_input(input: &[u8], split: usize) -> String {
        let (writer, _buf) = dummy_writer();
        let winsize = Arc::new(Mutex::new(window_size(80, 24, 9, 18)));
        let proxy = EventProxy { writer, winsize };
        let mut term = Term::new(
            Config::default(),
            &GridSize {
                cols: 80,
                lines: 24,
            },
            proxy,
        );
        let mut processor: Processor = Processor::new();
        let mut splitter = SixelSplitter::default();

        for chunk in [&input[..split], &input[split..]] {
            for seg in splitter.feed(chunk) {
                if let Seg::Pass { bytes, .. } = seg {
                    processor.advance(&mut term, &bytes);
                }
            }
        }

        term.bounds_to_string(
            Point::new(Line(0), Column(0)),
            Point::new(Line(23), Column(79)),
        )
    }

    #[test]
    fn utf8_render_is_independent_of_read_boundary() {
        let cases: [&[u8]; 3] = [
            "件名：再利用メール".as_bytes(),
            "\x1b]7;file:///tmp\x07件名：再利用メール".as_bytes(),
            "\x1bPq#0;2;100;0;0~\x1b\\件名：再利用メール".as_bytes(),
        ];

        for input in cases {
            let unsplit = render_split_input(input, input.len());
            assert!(unsplit.contains("件名：再利用メール"));
            for split in 0..=input.len() {
                assert_eq!(
                    render_split_input(input, split),
                    unsplit,
                    "render changed at byte split {split}"
                );
            }
        }
    }

    fn diagnostic_modes() -> DiagnosticModes {
        DiagnosticModes {
            application_cursor: true,
            alternate_screen: false,
            mouse: true,
        }
    }

    #[test]
    fn utf8_diagnostic_carries_incomplete_prefix_without_reporting_content() {
        let mut diagnostic = Utf8Diagnostic::new(true);
        assert!(diagnostic
            .observe(&[0xe6, 0x97], diagnostic_modes())
            .is_empty());
        assert!(diagnostic.observe(&[0xa5], diagnostic_modes()).is_empty());
        assert!(diagnostic.finish().is_empty());
        assert_eq!(diagnostic.invalid_count, 0);
        assert_eq!(diagnostic.incomplete_count, 0);
    }

    #[test]
    fn utf8_diagnostic_issue_contains_position_and_modes_but_no_bytes() {
        let mut diagnostic = Utf8Diagnostic::new(true);
        let issues = diagnostic.observe(&[0xff], diagnostic_modes());
        assert_eq!(issues[0].offset, 0);
        assert_eq!(issues[0].modes, diagnostic_modes());
        assert!(!format!("{issues:?}").contains("ff"));
    }

    #[test]
    fn utf8_diagnostic_reports_truncated_prefix_only_at_eof() {
        let mut diagnostic = Utf8Diagnostic::new(true);
        assert!(diagnostic
            .observe(&[0xe6, 0x97], diagnostic_modes())
            .is_empty());

        let issues = diagnostic.finish();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].offset, 0);
        assert_eq!(issues[0].modes, diagnostic_modes());
        assert_eq!(diagnostic.invalid_count, 0);
        assert_eq!(diagnostic.incomplete_count, 1);
    }

    fn run_diagnostic_pipeline(chunks: &[&[u8]]) -> Vec<Utf8Issue> {
        let (writer, _buf) = dummy_writer();
        let winsize = Arc::new(Mutex::new(window_size(80, 24, 9, 18)));
        let proxy = EventProxy { writer, winsize };
        let term = Arc::new(Mutex::new(Term::new(
            Config::default(),
            &GridSize {
                cols: 80,
                lines: 24,
            },
            proxy,
        )));
        let mut processor: Processor = Processor::new();
        let mut splitter = SixelSplitter::default();
        let mut diagnostic = Utf8Diagnostic::new(true);
        let mut issues = Vec::new();

        for chunk in chunks {
            for seg in splitter.feed(chunk) {
                issues.extend(advance_pass_or_finish_boundary(
                    &seg,
                    &mut processor,
                    &term,
                    &mut diagnostic,
                ));
            }
        }
        issues.extend(diagnostic.finish());
        issues
    }

    #[test]
    fn utf8_prefix_interrupted_by_sixel_is_not_joined() {
        let input = b"\xe6\x97\x1bPq~\x1b\\\xa5";
        let issues = run_diagnostic_pipeline(&[input]);
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].offset, 0);
        assert_eq!(issues[1].offset, 8);

        for split in 0..=input.len() {
            assert_eq!(
                run_diagnostic_pipeline(&[&input[..split], &input[split..]]),
                issues,
                "Sixel interruption changed at byte split {split}"
            );
        }
    }

    #[test]
    fn utf8_issue_offsets_include_intercepted_osc_and_sixel_bytes() {
        let cases: [(&[u8], u64); 2] = [
            (b"\x1b]7;file:///tmp\x07\xff", 16),
            (b"\x1bPq~\x1b\\\xff", 6),
        ];

        for (input, expected_offset) in cases {
            let issues = run_diagnostic_pipeline(&[input]);
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].offset, expected_offset);
            for split in 0..=input.len() {
                assert_eq!(
                    run_diagnostic_pipeline(&[&input[..split], &input[split..]]),
                    issues,
                    "source offset changed at byte split {split}"
                );
            }
        }
    }

    #[test]
    fn utf8_issue_modes_follow_preceding_controls_independent_of_chunking() {
        let input = b"\x1b[?1h\xff\x1b[?1l\xff";
        let unsplit = run_diagnostic_pipeline(&[input]);
        assert_eq!(unsplit.len(), 2);
        assert!(unsplit[0].modes.application_cursor);
        assert!(!unsplit[1].modes.application_cursor);

        for split in 0..=input.len() {
            assert_eq!(
                run_diagnostic_pipeline(&[&input[..split], &input[split..]]),
                unsplit,
                "diagnostic mode changed at byte split {split}"
            );
        }
    }

    #[test]
    fn app_cursor_mode_tracks_decset() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(80, 24, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();

        processor.advance(&mut *terminal.term.lock().unwrap(), b"\x1b[?1h");
        assert!(terminal.application_cursor_mode());

        processor.advance(&mut *terminal.term.lock().unwrap(), b"\x1b[?1l");
        assert!(!terminal.application_cursor_mode());
    }

    #[test]
    fn selection_text_does_not_depend_on_display_offset() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();

        let mut data = Vec::new();
        for line in 0..8 {
            data.extend_from_slice(format!("L{line}\r\n").as_bytes());
        }
        processor.advance(&mut *terminal.term.lock().unwrap(), &data);

        let selection = GridSelection {
            start: Point::new(Line(0), Column(0)),
            end: Point::new(Line(0), Column(1)),
            block: false,
        };
        let before = terminal.selection_text(selection);
        assert_eq!(before, "L5");

        terminal.scroll(3);
        let after = terminal.selection_text(selection);
        assert_eq!(after, before);
    }

    #[test]
    fn copy_mode_selection_crosses_scrollback_without_moving_anchor() {
        let command = vec!["sh".into(), "-c".into(), "exit 0".into()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        for n in 0..10 {
            processor.advance(
                &mut *terminal.term.lock().unwrap(),
                format!("L{n}\r\n").as_bytes(),
            );
        }
        terminal.toggle_copy_mode();
        terminal.copy_mode_edge(true);
        terminal.copy_mode_select(SelectionType::Lines);
        for _ in 0..7 {
            terminal.copy_mode_motion(alacritty_terminal::vi_mode::ViMotion::Down);
        }
        assert_eq!(
            terminal.tracked_selection_text().unwrap().1,
            "L0\nL1\nL2\nL3\nL4\nL5\nL6\nL7"
        );
        terminal.scroll(2);
        assert_eq!(
            terminal.tracked_selection_text().unwrap().1,
            "L0\nL1\nL2\nL3\nL4\nL5\nL6\nL7"
        );
        terminal.toggle_copy_mode();
        assert!(!terminal.copy_mode_active());
    }

    #[test]
    fn copy_mode_wide_characters_and_new_output_preserve_selected_text() {
        use alacritty_terminal::vi_mode::ViMotion;
        let command = vec!["sh".into(), "-c".into(), "exit 0".into()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        processor.advance(
            &mut *terminal.term.lock().unwrap(),
            "日本語\r\nsecond\r\nthird\r\nfourth".as_bytes(),
        );
        terminal.toggle_copy_mode();
        terminal.copy_mode_edge(true);
        terminal.copy_mode_motion(ViMotion::First);
        terminal.copy_mode_select(SelectionType::Simple);
        assert_eq!(terminal.tracked_selection_text().unwrap().1, "日");
        terminal.copy_mode_motion(ViMotion::Right);
        assert_eq!(terminal.tracked_selection_text().unwrap().1, "日本");
        advance_pass(
            &mut processor,
            &mut terminal.term.lock().unwrap(),
            0,
            b"\r\nmore\r\noutput",
            &mut Utf8Diagnostic::new(false),
        );
        assert_eq!(terminal.tracked_selection_text().unwrap().1, "日本");
        terminal.copy_mode_motion(ViMotion::Right);
        assert_eq!(terminal.tracked_selection_text().unwrap().1, "日本語");
        terminal.copy_mode_select(SelectionType::Block);
        terminal.copy_mode_motion(ViMotion::Left);
        terminal.copy_mode_motion(ViMotion::Down);
        let selection = terminal.tracked_selection_text().unwrap();
        assert!(selection.0.block);
        assert_eq!(selection.1, "本語\non");
    }

    #[test]
    fn block_selection_text_keeps_columns_per_line() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();

        let mut data = Vec::new();
        for line in 0..8 {
            data.extend_from_slice(format!("R{line}abcd\r\n").as_bytes());
        }
        processor.advance(&mut *terminal.term.lock().unwrap(), &data);

        let selection = GridSelection {
            start: Point::new(Line(1), Column(4)),
            end: Point::new(Line(0), Column(2)),
            block: true,
        };
        assert_eq!(terminal.selection_text(selection), "abc\nabc");
    }

    #[test]
    fn selection_tracks_grid_rotation_after_additional_output() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        let mut data = Vec::new();
        for line in 0..8 {
            data.extend_from_slice(format!("L{line}\r\n").as_bytes());
        }
        processor.advance(&mut *terminal.term.lock().unwrap(), &data);

        terminal.start_selection(
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
            Side::Left,
        );
        terminal.update_selection(Point::new(Line(0), Column(1)), Side::Right);
        assert_eq!(
            terminal.selection_text(terminal.grid_selection().unwrap()),
            "L5"
        );

        processor.advance(&mut *terminal.term.lock().unwrap(), b"L8\r\n");
        let rotated = terminal
            .grid_selection()
            .expect("selection should rotate with its text");
        assert_eq!(terminal.selection_text(rotated), "L5");
        assert_eq!(
            terminal.tracked_selection_text(),
            Some((rotated, "L5".to_owned()))
        );
    }

    #[test]
    fn cleared_history_rejects_stale_selection_bounds() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        let mut data = Vec::new();
        for line in 0..8 {
            data.extend_from_slice(format!("L{line}\r\n").as_bytes());
        }
        processor.advance(&mut *terminal.term.lock().unwrap(), &data);

        let stale = GridSelection {
            start: Point::new(Line(-1), Column(0)),
            end: Point::new(Line(-1), Column(1)),
            block: false,
        };
        assert_eq!(terminal.selection_text(stale), "L4");

        processor.advance(&mut *terminal.term.lock().unwrap(), b"\x1b[3J");
        assert_eq!(terminal.selection_text(stale), "");
    }

    #[test]
    fn explicit_history_clear_invalidates_tracked_selection() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        let mut data = Vec::new();
        for line in 0..8 {
            data.extend_from_slice(format!("L{line}\r\n").as_bytes());
        }
        processor.advance(&mut *terminal.term.lock().unwrap(), &data);

        terminal.start_selection(
            SelectionType::Simple,
            Point::new(Line(-1), Column(0)),
            Side::Left,
        );
        terminal.update_selection(Point::new(Line(-1), Column(1)), Side::Right);
        terminal.clear_history();

        assert!(terminal.term.lock().unwrap().selection.is_none());
    }

    #[test]
    fn stale_semantic_start_after_history_clear_is_ignored() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        let mut data = Vec::new();
        for line in 0..8 {
            data.extend_from_slice(format!("word{line}\r\n").as_bytes());
        }
        processor.advance(&mut *terminal.term.lock().unwrap(), &data);
        terminal.clear_history();

        terminal.start_selection(
            SelectionType::Semantic,
            Point::new(Line(-1), Column(2)),
            Side::Left,
        );

        assert!(terminal.term.lock().unwrap().selection.is_none());
        assert!(terminal.selection_drag.lock().unwrap().is_none());
    }

    #[test]
    fn stale_line_start_after_history_clear_is_ignored() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        let mut data = Vec::new();
        for line in 0..8 {
            data.extend_from_slice(format!("word{line}\r\n").as_bytes());
        }
        processor.advance(&mut *terminal.term.lock().unwrap(), &data);
        terminal.clear_history();

        terminal.start_selection(
            SelectionType::Lines,
            Point::new(Line(-1), Column(2)),
            Side::Left,
        );

        assert!(terminal.term.lock().unwrap().selection.is_none());
        assert!(terminal.selection_drag.lock().unwrap().is_none());
    }

    #[test]
    fn stale_semantic_update_after_history_clear_is_ignored() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        terminal.clear_history();
        let anchor = Point::new(Line(0), Column(2));
        terminal.start_selection(SelectionType::Semantic, anchor, Side::Left);
        let before = terminal.grid_selection();

        terminal.update_selection(Point::new(Line(-1), Column(2)), Side::Right);

        assert_eq!(terminal.grid_selection(), before);
        assert!(terminal.selection_drag.lock().unwrap().is_none());
    }

    #[test]
    fn stale_line_update_after_history_clear_is_ignored() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        terminal.clear_history();
        let anchor = Point::new(Line(0), Column(2));
        terminal.start_selection(SelectionType::Lines, anchor, Side::Left);
        let before = terminal.grid_selection();

        terminal.update_selection(Point::new(Line(-1), Column(2)), Side::Right);

        assert_eq!(terminal.grid_selection(), before);
        assert!(terminal.selection_drag.lock().unwrap().is_none());
    }

    #[test]
    fn pixel_selection_resolves_against_the_current_grid_generation() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        let mut data = Vec::new();
        for line in 0..8 {
            data.extend_from_slice(format!("word{line}\r\n").as_bytes());
        }
        processor.advance(&mut *terminal.term.lock().unwrap(), &data);
        terminal.scroll(2);
        terminal.clear_history();

        assert!(terminal.start_selection_at_pixel(SelectionType::Semantic, 19.0, 1.0, 9, 18,));
        assert_eq!(terminal.tracked_selection_text().unwrap().1, "word5");

        assert!(terminal.update_selection_at_pixel(19.0, 19.0, 9, 18));
        assert_eq!(terminal.tracked_selection_text().unwrap().1, "word5\nword6");
    }

    #[test]
    fn pixel_selection_maps_to_scrollback_cell_and_side() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        let mut data = Vec::new();
        for line in 0..8 {
            data.extend_from_slice(format!("word{line}\r\n").as_bytes());
        }
        processor.advance(&mut *terminal.term.lock().unwrap(), &data);
        terminal.scroll(3);

        assert_eq!(
            pixel_selection_point(&terminal.term.lock().unwrap(), 25.0, 45.0, 10, 20),
            Some((Point::new(Line(-1), Column(2)), Side::Right))
        );
    }

    #[test]
    fn alternate_screen_switch_invalidates_selection() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        processor.advance(&mut *terminal.term.lock().unwrap(), b"primary");

        terminal.start_selection(
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
            Side::Left,
        );
        terminal.update_selection(Point::new(Line(0), Column(6)), Side::Right);
        assert!(terminal.grid_selection().is_some());

        processor.advance(&mut *terminal.term.lock().unwrap(), b"\x1b[?1049h");
        assert_eq!(terminal.grid_selection(), None);
    }

    #[test]
    fn semantic_selection_keeps_expanded_anchor_while_display_scrolls() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        let mut data = Vec::new();
        for line in 0..8 {
            data.extend_from_slice(format!("word{line}\r\n").as_bytes());
        }
        processor.advance(&mut *terminal.term.lock().unwrap(), &data);

        let anchor = Point::new(Line(0), Column(2));
        terminal.start_selection(SelectionType::Semantic, anchor, Side::Left);
        terminal.update_selection(anchor, Side::Right);
        assert_eq!(
            terminal.selection_text(terminal.grid_selection().unwrap()),
            "word5"
        );

        terminal.scroll(2);
        let current_top = Point::new(Line(-2), Column(2));
        terminal.update_selection(current_top, Side::Right);
        assert_eq!(
            terminal.selection_text(terminal.grid_selection().unwrap()),
            "word3\nword4\nword5"
        );
    }

    #[test]
    fn semantic_selection_tracks_expanded_anchor_through_pty_output() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        let mut data = Vec::new();
        for line in 0..8 {
            data.extend_from_slice(format!("word{line}\r\n").as_bytes());
        }
        processor.advance(&mut *terminal.term.lock().unwrap(), &data);

        let anchor = Point::new(Line(0), Column(2));
        terminal.start_selection(SelectionType::Semantic, anchor, Side::Left);
        terminal.update_selection(anchor, Side::Right);
        let before = terminal.grid_selection().unwrap();
        assert_eq!(terminal.tracked_selection_text().unwrap().1, "word5");

        processor.advance(&mut *terminal.term.lock().unwrap(), b"word8\r\n");

        let after = terminal.grid_selection().unwrap();
        assert_eq!(after.start.line, before.start.line - 1);
        assert_eq!(after.end.line, before.end.line - 1);
        assert_eq!(terminal.tracked_selection_text().unwrap().1, "word5");
    }

    #[test]
    fn semantic_selection_preserves_ascii_punctuation_word_boundaries() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        processor.advance(&mut *terminal.term.lock().unwrap(), b"foo-bar");

        let anchor = Point::new(Line(0), Column(1));
        terminal.start_selection(SelectionType::Semantic, anchor, Side::Left);
        terminal.update_selection(anchor, Side::Right);
        assert_eq!(
            terminal.selection_text(terminal.grid_selection().unwrap()),
            "foo"
        );
    }

    #[test]
    fn semantic_selection_on_punctuation_selects_only_that_cell() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(10, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        processor.advance(&mut *terminal.term.lock().unwrap(), b"(foo)");

        let anchor = Point::new(Line(0), Column(0));
        terminal.start_selection(SelectionType::Semantic, anchor, Side::Left);
        terminal.update_selection(anchor, Side::Right);
        assert_eq!(terminal.tracked_selection_text().unwrap().1, "(");
    }

    #[test]
    fn semantic_selection_stops_at_soft_wrapped_physical_row() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(5, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        processor.advance(&mut *terminal.term.lock().unwrap(), b"abcdefgh");

        let anchor = Point::new(Line(0), Column(2));
        terminal.start_selection(SelectionType::Semantic, anchor, Side::Left);
        terminal.update_selection(anchor, Side::Right);
        assert_eq!(terminal.tracked_selection_text().unwrap().1, "abcde");
    }

    #[test]
    fn line_selection_stops_at_soft_wrapped_physical_row() {
        let command = vec!["sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let terminal = VtTerminal::new(5, 4, 9, 18, Path::new("."), Some(&command));
        let mut processor: Processor = Processor::new();
        processor.advance(&mut *terminal.term.lock().unwrap(), b"abcdefgh");

        let anchor = Point::new(Line(0), Column(2));
        terminal.start_selection(SelectionType::Lines, anchor, Side::Left);
        terminal.update_selection(anchor, Side::Right);
        assert_eq!(terminal.tracked_selection_text().unwrap().1, "abcde");
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
            .filter_map(|s| match s {
                Seg::Pass { bytes, .. } => Some(('P', String::from_utf8_lossy(bytes).into_owned())),
                Seg::Sixel(b) => Some(('S', String::from_utf8_lossy(b).into_owned())),
                Seg::Osc { code, payload } => Some(match code {
                    OscCode::Cwd => ('7', payload.clone()),
                    OscCode::Gt => ('G', payload.clone()),
                }),
                Seg::Boundary => None,
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
    fn splitter_extracts_osc7_bel() {
        let mut sp = SixelSplitter::default();
        let segs = sp.feed(b"pre\x1b]7;file://host/tmp/a%20b\x07post");
        assert_eq!(
            segs_to_debug(&segs),
            vec![
                ('P', "pre".to_string()),
                ('7', "file://host/tmp/a%20b".to_string()),
                ('P', "post".to_string()),
            ]
        );
    }

    #[test]
    fn splitter_extracts_osc7_st_across_chunks() {
        let mut sp = SixelSplitter::default();
        let mut got = Vec::new();
        got.extend(segs_to_debug(&sp.feed(b"\x1b]7;file://host/")));
        got.extend(segs_to_debug(&sp.feed(b"work\x1b\\x")));
        assert_eq!(
            got,
            vec![
                ('7', "file://host/work".to_string()),
                ('P', "x".to_string()),
            ]
        );
    }

    #[test]
    fn splitter_passes_other_osc_through() {
        let mut sp = SixelSplitter::default();
        let segs = sp.feed(b"\x1b]0;title\x1b\\ok");
        assert_eq!(
            segs_to_debug(&segs),
            vec![('P', "\x1b]0;title\x1b\\ok".to_string())]
        );
    }

    #[test]
    fn splitter_discards_oversized_osc7() {
        let mut sp = SixelSplitter::default();
        let mut input = b"\x1b]7;".to_vec();
        input.extend(std::iter::repeat(b'a').take(OSC_MAX_BYTES + 1));
        input.push(0x07);
        input.extend_from_slice(b"ok");
        let segs = sp.feed(&input);
        assert_eq!(segs_to_debug(&segs), vec![('P', "ok".to_string())]);
    }

    #[test]
    fn splitter_handles_sixel_and_osc_mix() {
        let mut sp = SixelSplitter::default();
        let segs = sp.feed(b"\x1bPqabc\x1b\\\x1b]7717;event;kind=mod\x07z");
        assert_eq!(
            segs_to_debug(&segs),
            vec![
                ('S', "abc".to_string()),
                ('G', "event;kind=mod".to_string()),
                ('P', "z".to_string()),
            ]
        );
    }

    #[test]
    fn parse_osc7_accepts_host_and_empty_host() {
        assert_eq!(
            parse_osc7("file://host/tmp/a%20b"),
            Some(("host".to_string(), PathBuf::from("/tmp/a b")))
        );
        assert_eq!(
            parse_osc7("file:///tmp"),
            Some(("".to_string(), PathBuf::from("/tmp")))
        );
    }

    #[test]
    fn parse_osc7_strips_leading_slash_of_windows_drive_path() {
        // PowerShell 等が送る Windows 形式: file://HOST/C:/Users/naoto
        assert_eq!(
            parse_osc7("file://DESKTOP/C:/Users/naoto"),
            Some(("DESKTOP".to_string(), PathBuf::from("C:/Users/naoto")))
        );
        // Unix パスの先頭スラッシュは剥がさない
        assert_eq!(
            parse_osc7("file://host/code"),
            Some(("host".to_string(), PathBuf::from("/code")))
        );
    }

    #[test]
    fn parse_osc7_accepts_percent_encoded_utf8_and_rejects_invalid() {
        assert_eq!(
            parse_osc7("file://host/%E6%97%A5%E6%9C%AC%E8%AA%9E"),
            Some(("host".to_string(), PathBuf::from("/日本語")))
        );
        assert_eq!(parse_osc7("http://host/tmp"), None);
        assert_eq!(parse_osc7("file://host/%GG"), None);
        assert_eq!(parse_osc7("file://host/%E6%97"), None);
    }

    #[test]
    fn classify_shell_location_detects_local_hosts() {
        assert_eq!(
            classify_shell_location("", PathBuf::from("/tmp"), "mybox"),
            ShellLocation::Local(PathBuf::from("/tmp"))
        );
        assert_eq!(
            classify_shell_location("localhost", PathBuf::from("/tmp"), "mybox"),
            ShellLocation::Local(PathBuf::from("/tmp"))
        );
        assert_eq!(
            classify_shell_location("MYBOX", PathBuf::from("/tmp"), "mybox"),
            ShellLocation::Local(PathBuf::from("/tmp"))
        );
        assert_eq!(
            classify_shell_location("other", PathBuf::from("/home/n"), "mybox"),
            ShellLocation::Remote {
                host: "other".to_string(),
                path: PathBuf::from("/home/n"),
            }
        );
    }

    #[test]
    fn responds_to_text_area_pixel_size_query() {
        use alacritty_terminal::term::{Config, Term};
        use vte::ansi::Processor;

        let (writer, buf) = dummy_writer();
        let winsize = Arc::new(Mutex::new(window_size(80, 24, 9, 18)));
        let proxy = EventProxy { writer, winsize };

        let size = GridSize {
            cols: 80,
            lines: 24,
        };
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
            &GridSize {
                cols: 80,
                lines: 24,
            },
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
        assert_eq!(
            color_index_to_rgb(231),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        // 196 = 赤(5,0,0) -> (255,0,0)
        assert_eq!(color_index_to_rgb(196), Rgb { r: 255, g: 0, b: 0 });
        // グレースケール: 232=8, 255=238（v=8+10*23）
        assert_eq!(color_index_to_rgb(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(
            color_index_to_rgb(255),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
    }
}
