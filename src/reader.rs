use std::path::{Path, PathBuf};

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthChar;
use winit::{
    dpi::PhysicalPosition,
    event::{ElementState, KeyEvent},
    keyboard::{KeyCode, PhysicalKey},
};

use crate::config::resolve_editor;
use crate::highlight::{self, HighlightedLine};
use crate::preview::{FilePreview, PreviewLines};
use crate::terminal::{Cell, Color, GraphicAttribute, Line, PositionedImage};
use crate::view::{TerminalView, Viewport};
use crate::Display;

const PLACEHOLDER: &str = "ファイルを選ぶか、AI がファイルを書くとここに表示されます";
const READER_WRAP_MAX: usize = 100;
/// 本文の左余白（列）。
const BODY_MARGIN: usize = 1;
/// 行番号の色（Tokyo Night の LineNr より少し明るく、半透明の背景でも読める濃さ）。
const LINE_NUMBER_FG: Color = Color::Rgb { rgba: 0x565F_89FF };

pub struct ReaderPane {
    view: TerminalView,
    preview: FilePreview,
    pinned: bool,
    focused: bool,
    reader_scroll: usize,
    reader_lines: Vec<StyledLine>,
    reader_notice: Option<String>,
    row_actions: Vec<Option<ReaderAction>>,
}

impl ReaderPane {
    pub fn new(display: Display, viewport: Viewport) -> Self {
        let mut pane = Self {
            view: TerminalView::with_viewport(
                display,
                viewport,
                crate::TOYTERM_CONFIG.font_size,
                None,
            ),
            preview: FilePreview::new(),
            pinned: false,
            focused: false,
            reader_scroll: 0,
            reader_lines: Vec::new(),
            reader_notice: None,
            row_actions: Vec::new(),
        };
        pane.refresh_reader_document();
        pane.rebuild();
        pane
    }

    pub fn contains(&self, p: PhysicalPosition<f64>) -> bool {
        self.view.viewport().contains(p)
    }

    pub fn cell_height(&self) -> u32 {
        self.view.cell_size().h
    }

    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.view.set_viewport(viewport);
        // 画像は表示領域に合わせて縮小済みなので、ペインの大きさが変わったら
        // 収まるサイズで取り直す。
        if self.update_fit() && self.preview.image().is_some() {
            self.preview.refresh_current();
        }
        self.refresh_reader_document();
        self.rebuild();
    }

    /// 画像を収める領域（px）をビューポートから概算して preview に伝える。
    /// ヘッダ行ぶんはざっくり差し引く。戻り値 true=変わった。
    fn update_fit(&mut self) -> bool {
        let vp = self.view.viewport();
        let cw = self.view.cell_size().w.max(1);
        let ch = self.view.cell_size().h.max(1);
        let w = vp.w.saturating_sub(cw * 2);
        let h = vp.h.saturating_sub(ch * 5);
        self.preview.set_fit(w, h)
    }

    pub fn draw(&mut self, surface: &mut glium::Frame) {
        self.view.draw(surface);
    }

    pub fn needs_redraw(&self) -> bool {
        self.view.needs_redraw()
    }

    pub fn is_following(&self) -> bool {
        !self.pinned
    }

    pub fn viewport(&self) -> Viewport {
        self.view.viewport()
    }

    pub fn target_abs(&self) -> Option<&Path> {
        self.preview.target_abs()
    }

    pub fn on_click(&mut self, p: PhysicalPosition<f64>) -> Option<ReaderRequest> {
        if !self.contains(p) {
            return None;
        }
        let row = click_row(self.view.viewport(), self.view.cell_size().h, p);
        let action = self.row_actions.get(row).and_then(Option::as_ref)?.clone();
        self.run_action(action)
    }

    pub fn set_focused(&mut self, focused: bool) {
        if self.focused == focused {
            return;
        }
        self.focused = focused;
        self.rebuild();
    }

    pub fn on_scroll(&mut self, delta: i32) {
        if delta != 0 {
            self.scroll_by(-(delta as isize));
        }
    }

    pub fn on_key(&mut self, key: &KeyEvent) -> ReaderKeyResult {
        if key.state != ElementState::Pressed {
            return ReaderKeyResult::Consumed;
        }
        let code = match key.physical_key {
            PhysicalKey::Code(code) => code,
            PhysicalKey::Unidentified(_) => return ReaderKeyResult::Consumed,
        };

        match reader_key_intent(code) {
            ReaderKeyIntent::Release => ReaderKeyResult::ReleaseFocus,
            ReaderKeyIntent::Scroll(delta) => {
                self.scroll_by(delta);
                ReaderKeyResult::Consumed
            }
            ReaderKeyIntent::Page(pages) => {
                self.scroll_by(pages * self.page_step() as isize);
                ReaderKeyResult::Consumed
            }
            ReaderKeyIntent::Top => {
                self.scroll_to(0);
                ReaderKeyResult::Consumed
            }
            ReaderKeyIntent::Bottom => {
                self.scroll_to(usize::MAX);
                ReaderKeyResult::Consumed
            }
            ReaderKeyIntent::Header(action) => self
                .on_header_key(action)
                .map_or(ReaderKeyResult::Consumed, ReaderKeyResult::Request),
            ReaderKeyIntent::Ignore => ReaderKeyResult::Consumed,
        }
    }

    /// 1ページぶんの行数（1行だけ重ねて位置を見失わないようにする）。
    fn page_step(&self) -> usize {
        self.reader_body_slots().saturating_sub(1).max(1)
    }

    pub fn on_header_key(&mut self, action: ReaderHeaderAction) -> Option<ReaderRequest> {
        match action {
            ReaderHeaderAction::Edit => self
                .reader_action(|action| matches!(action, ReaderAction::EditFile(_)))
                .and_then(|action| self.run_action(action)),
            ReaderHeaderAction::OpenWithSystem => self
                .reader_action(|action| matches!(action, ReaderAction::OpenWithSystem(_)))
                .and_then(|action| self.run_action(action)),
        }
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let pinned_now = self.pin_at_tail();
        let next = if delta.is_negative() {
            self.reader_scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.reader_scroll.saturating_add(delta as usize)
        };
        self.set_scroll(next);
        if pinned_now {
            self.rebuild();
        }
    }

    fn scroll_to(&mut self, offset: usize) {
        let pinned_now = self.pin_at_tail();
        self.set_scroll(offset);
        if pinned_now {
            self.rebuild();
        }
    }

    /// 追従中にスクロールされたら、いま見えている末尾の位置で固定する。
    /// tail -f を上へスクロールしたら追従が止まるのと同じ考え方。戻り値 true=固定した。
    fn pin_at_tail(&mut self) -> bool {
        if self.pinned || self.preview.target().is_none() {
            return false;
        }
        self.pinned = true;
        // pinned は markdown 整形の条件も兼ねるので、固定してから作り直す。
        self.refresh_reader_document();
        self.reader_scroll = reader_scroll_max(self.reader_lines.len(), self.reader_body_slots());
        true
    }

    pub fn preview_pinned(&mut self, abs_path: &Path, root: Option<&Path>) {
        let display_path = root
            .and_then(|root| abs_path.strip_prefix(root).ok())
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| abs_path.to_path_buf());

        self.pinned = true;
        self.reader_scroll = 0;
        self.reader_notice = None;
        self.update_fit();
        self.preview
            .set_target_abs(abs_path.to_path_buf(), display_path);
        self.refresh_reader_document();
        self.rebuild();
    }

    pub fn follow_target(&mut self, abs_path: PathBuf, root: Option<&Path>) {
        if self.pinned {
            return;
        }
        let display_path = root
            .and_then(|root| abs_path.strip_prefix(root).ok())
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| abs_path.clone());
        self.update_fit();
        self.preview.notify_target_abs(abs_path, display_path);
        self.refresh_reader_document();
        self.rebuild();
    }

    pub fn show_remote_content(&mut self, path: PathBuf, bytes: Vec<u8>) {
        self.pinned = true;
        self.reader_scroll = 0;
        self.reader_notice = Some(" (remote)".to_owned());
        self.preview.set_memory_content(path, bytes);
        self.refresh_reader_document();
        self.rebuild();
    }

    pub fn refresh_current(&mut self) {
        self.preview.refresh_current();
        self.reader_notice = None;
        self.refresh_reader_document();
        self.rebuild();
    }

    pub fn poll(&mut self) -> bool {
        if self.preview.poll() {
            self.refresh_reader_document();
            self.rebuild();
            true
        } else {
            false
        }
    }

    pub fn show_missing_editor(&mut self, command: &str) {
        self.reader_notice = Some(format!(
            " （編集コマンドが見つかりません: {command}。config.toml の editor で設定できます）"
        ));
        self.refresh_reader_document();
        self.rebuild();
    }

    fn run_action(&mut self, action: ReaderAction) -> Option<ReaderRequest> {
        match action {
            ReaderAction::Unpin => {
                self.pinned = false;
                self.reader_scroll = 0;
                self.reader_notice = None;
                self.refresh_reader_document();
                self.rebuild();
                None
            }
            ReaderAction::EditFile(path) => Some(ReaderRequest::EditFile(path)),
            ReaderAction::OpenWithSystem(path) => Some(ReaderRequest::OpenWithSystem(path)),
        }
    }

    fn reader_action(&self, predicate: impl Fn(&ReaderAction) -> bool) -> Option<ReaderAction> {
        self.row_actions
            .iter()
            .flatten()
            .find(|action| predicate(action))
            .cloned()
    }

    fn set_scroll(&mut self, offset: usize) {
        let clamped =
            clamp_reader_scroll(offset, self.reader_lines.len(), self.reader_body_slots());
        if clamped != self.reader_scroll {
            self.reader_scroll = clamped;
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        let cols = (self.view.viewport().w / self.view.cell_size().w).max(1) as usize;
        let rows = (self.view.viewport().h / self.view.cell_size().h).max(1) as usize;
        // フォーカス中は最下行をキーヒントに使う（サイドバーと同じ）。
        let rows = rows.saturating_sub(usize::from(self.focused));
        let mut lines = Vec::new();
        let mut row_actions = Vec::new();

        if let Some(target) = self.preview.target() {
            let suffix = if self.pinned {
                " 📌(クリックで追従に戻る)"
            } else {
                ""
            };
            let available = cols.saturating_sub(1 + display_width(suffix));
            let path = abbreviate_start(&target.display().to_string(), available);
            let action = self.pinned.then_some(ReaderAction::Unpin);
            push_line(
                &mut lines,
                &mut row_actions,
                cols,
                &format!(" {path}{suffix}"),
                Color::BrightWhite,
                action,
            );
            if self.preview.is_diff() {
                push_line(
                    &mut lines,
                    &mut row_actions,
                    cols,
                    " ● HEAD との差分",
                    Color::Cyan,
                    None,
                );
            }
        }

        if let Some(abs_path) = self.preview.target_abs() {
            let env_editor = std::env::var("EDITOR").ok();
            let editor = resolve_editor(&crate::TOYTERM_CONFIG.editor, env_editor.as_deref());
            push_line(
                &mut lines,
                &mut row_actions,
                cols,
                &format!(" [編集: {}]", editor[0]),
                Color::Cyan,
                Some(ReaderAction::EditFile(abs_path.to_path_buf())),
            );
            push_line(
                &mut lines,
                &mut row_actions,
                cols,
                " [OSの既定アプリで開く]",
                Color::Cyan,
                Some(ReaderAction::OpenWithSystem(abs_path.to_path_buf())),
            );
        }

        if let Some(notice) = &self.reader_notice {
            push_line(
                &mut lines,
                &mut row_actions,
                cols,
                notice,
                Color::BrightBlack,
                None,
            );
        }

        if !lines.is_empty() {
            push_separator(&mut lines, &mut row_actions, cols);
        }

        // 画像プレビューはヘッダの下に画像を1枚だけ置き、本文テキストは出さない。
        let mut images = Vec::new();
        if let Some((rgb, w, h)) = self.preview.image() {
            let header_rows = lines.len();
            let cw = self.view.cell_size().w.max(1);
            // 横方向は中央寄せ（画像が幅より小さいとき）。
            let x_off = self.view.viewport().w.saturating_sub(w) / 2;
            images.push(PositionedImage {
                row: header_rows as isize,
                col: (x_off / cw) as isize,
                width: w as u64,
                height: h as u64,
                data: rgb.to_vec(),
            });
        } else {
            let available = rows.saturating_sub(lines.len());
            if available > 0 {
                let scroll = if self.pinned {
                    clamp_reader_scroll(self.reader_scroll, self.reader_lines.len(), available)
                } else {
                    self.reader_lines.len().saturating_sub(available)
                };
                for line in self.reader_lines.iter().skip(scroll).take(available) {
                    push_styled_line(&mut lines, &mut row_actions, cols, line, None);
                }
            }
        }

        lines.truncate(rows);
        row_actions.truncate(rows);
        if self.focused {
            // ソリッド背景でも読める明るさ（サイドバーのヒントと同じ色）。
            push_line(
                &mut lines,
                &mut row_actions,
                cols,
                " j/k:スクロール e:編集 o:既定アプリ Esc:端末",
                Color::Rgb { rgba: 0x565F_89FF },
                None,
            );
        }
        self.row_actions = row_actions;
        self.view.update_contents(|view| {
            // ターミナル／ランチャーと同じ透過（セルの Color::Background クアッドで
            // 半透明を出す）。区切りはマネージャの黒フレームクリアが担う。
            view.bg_color = Color::Background;
            view.skip_default_bg = false;
            view.lines = lines;
            view.images = images;
            view.cursor = None;
            view.selection_range = None;
        });
    }

    fn refresh_reader_document(&mut self) {
        self.reader_lines = if self.preview.target().is_none() {
            vec![styled_plain(PLACEHOLDER.to_owned(), Color::BrightBlack)]
        } else {
            match self.preview.lines() {
                PreviewLines::Text(text) => {
                    if self.pinned
                        && self
                            .preview
                            .target()
                            .and_then(Path::extension)
                            .and_then(|ext| ext.to_str())
                            .is_some_and(is_markdown_extension)
                    {
                        // 整形済みの文章は行番号を出さない（読み物として読むため）。
                        render_markdown(&text.lines.join("\n"))
                            .into_iter()
                            .flat_map(|line| indent_wrapped(&line, self.reader_wrap_cols()))
                            .collect()
                    } else {
                        let source: Vec<StyledLine> = match &text.highlighted {
                            Some(highlighted) => {
                                highlighted.iter().map(styled_code).collect()
                            }
                            None => text
                                .lines
                                .iter()
                                .map(|line| styled_plain(line.clone(), Color::White))
                                .collect(),
                        };
                        // 末尾だけ読んだファイルは先頭が1行目ではないので番号を出さない。
                        number_and_wrap(source, self.reader_wrap_cols(), !text.truncated)
                    }
                }
                PreviewLines::Diff(diff_lines) => {
                    let wrap_cols = self.reader_wrap_cols();
                    diff_lines
                        .iter()
                        .flat_map(|line| {
                            let color = diff_line_color(line);
                            wrap_line(line, wrap_cols)
                                .into_iter()
                                .map(move |line| styled_plain(line, color))
                        })
                        .collect()
                }
                PreviewLines::Message(message) => {
                    vec![styled_plain(message.to_owned(), Color::BrightBlack)]
                }
            }
        };

        self.reader_scroll = clamp_reader_scroll(
            self.reader_scroll,
            self.reader_lines.len(),
            self.reader_body_slots(),
        );
    }

    fn reader_wrap_cols(&self) -> usize {
        let cols = (self.view.viewport().w / self.view.cell_size().w).max(1) as usize;
        cols.saturating_sub(1).max(1).min(READER_WRAP_MAX)
    }

    fn reader_body_slots(&self) -> usize {
        let rows = (self.view.viewport().h / self.view.cell_size().h).max(1) as usize;
        let header_rows = usize::from(self.preview.target().is_some())
            + usize::from(self.preview.is_diff())
            + usize::from(self.preview.target_abs().is_some()) * 2
            + usize::from(self.reader_notice.is_some())
            + usize::from(self.preview.target().is_some())
            + usize::from(self.focused);
        rows.saturating_sub(header_rows)
    }
}

pub enum ReaderRequest {
    EditFile(PathBuf),
    OpenWithSystem(PathBuf),
}

pub enum ReaderKeyResult {
    Consumed,
    ReleaseFocus,
    Request(ReaderRequest),
}

/// フォーカス中のビューアでキーが何を意味するか。ページ幅は表示行数に依るので、
/// ここでは「何ページ動かすか」までにして実際の行数は呼び出し側で掛ける。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReaderKeyIntent {
    Scroll(isize),
    Page(isize),
    Top,
    Bottom,
    Release,
    Header(ReaderHeaderAction),
    Ignore,
}

fn reader_key_intent(code: KeyCode) -> ReaderKeyIntent {
    match code {
        KeyCode::Escape => ReaderKeyIntent::Release,
        // 上下移動：矢印＋ vim の k/j（サイドバーと同じ流儀）。
        KeyCode::ArrowUp | KeyCode::KeyK => ReaderKeyIntent::Scroll(-1),
        KeyCode::ArrowDown | KeyCode::KeyJ => ReaderKeyIntent::Scroll(1),
        KeyCode::PageUp => ReaderKeyIntent::Page(-1),
        KeyCode::PageDown => ReaderKeyIntent::Page(1),
        KeyCode::Home => ReaderKeyIntent::Top,
        KeyCode::End => ReaderKeyIntent::Bottom,
        KeyCode::KeyE => ReaderKeyIntent::Header(ReaderHeaderAction::Edit),
        KeyCode::KeyO => ReaderKeyIntent::Header(ReaderHeaderAction::OpenWithSystem),
        _ => ReaderKeyIntent::Ignore,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReaderHeaderAction {
    Edit,
    OpenWithSystem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ReaderAction {
    Unpin,
    EditFile(PathBuf),
    OpenWithSystem(PathBuf),
}

fn click_row(viewport: Viewport, cell_height: u32, p: PhysicalPosition<f64>) -> usize {
    ((p.y - viewport.y as f64) / cell_height.max(1) as f64) as usize
}

fn push_separator(lines: &mut Vec<Line>, row_actions: &mut Vec<Option<ReaderAction>>, cols: usize) {
    push_line(lines, row_actions, cols, " \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}", Color::BrightBlack, None);
}

fn push_line(
    lines: &mut Vec<Line>,
    row_actions: &mut Vec<Option<ReaderAction>>,
    cols: usize,
    text: &str,
    fg: Color,
    action: Option<ReaderAction>,
) {
    lines.push(Line::from_cells(cells_for_line(text, cols, fg), false));
    row_actions.push(action);
}

fn push_styled_line(
    lines: &mut Vec<Line>,
    row_actions: &mut Vec<Option<ReaderAction>>,
    cols: usize,
    line: &StyledLine,
    action: Option<ReaderAction>,
) {
    lines.push(Line::from_cells(cells_for_styled_line(line, cols), false));
    row_actions.push(action);
}

fn cells_for_line(text: &str, cols: usize, fg: Color) -> Vec<Cell> {
    cells_for_segments(&[(text, fg)], cols)
}

fn cells_for_segments(segments: &[(&str, Color)], cols: usize) -> Vec<Cell> {
    let styled: Vec<StyledSegment> = segments
        .iter()
        .map(|(text, fg)| StyledSegment {
            text: (*text).to_owned(),
            style: TextStyle {
                fg: *fg,
                bold: false,
            },
        })
        .collect();
    cells_for_styled_line(&styled, cols)
}

fn cells_for_styled_line(segments: &StyledLine, cols: usize) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut used = 0usize;

    for segment in segments {
        for ch in segment.text.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width == 0 || used + width > cols {
                return cells;
            }

            let attr = GraphicAttribute {
                fg: segment.style.fg,
                bold: i8::from(segment.style.bold),
                ..GraphicAttribute::default()
            };
            cells.push(Cell::head(ch, width as u16, attr));
            for i in 1..width {
                cells.push(Cell::spacer(i as u16));
            }
            used += width;
        }
    }

    cells
}

pub(crate) type StyledLine = Vec<StyledSegment>;

#[derive(Clone, Debug)]
pub(crate) struct StyledSegment {
    text: String,
    style: TextStyle,
}

#[derive(Clone, Copy, Debug)]
struct TextStyle {
    fg: Color,
    bold: bool,
}

fn styled_plain(text: String, fg: Color) -> StyledLine {
    vec![StyledSegment {
        text,
        style: TextStyle { fg, bold: false },
    }]
}

pub(crate) fn diff_line_color(line: &str) -> Color {
    if line.starts_with("@@") {
        Color::Cyan
    } else if line.starts_with("+++ ") || line.starts_with("--- ") {
        Color::BrightBlack
    } else if line.starts_with("diff --git")
        || line.starts_with("index ")
        || line.starts_with("new file")
        || line.starts_with("deleted file")
        || line.starts_with("rename ")
        || line.starts_with("similarity ")
    {
        Color::BrightBlack
    } else if line.starts_with('+') {
        Color::Green
    } else if line.starts_with('-') {
        Color::Red
    } else {
        Color::White
    }
}

pub(crate) fn wrap_line(line: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    if line.is_empty() {
        return vec![String::new()];
    }

    let mut out = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for ch in line.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width == 0 {
            continue;
        }
        if used > 0 && used + width > cols {
            out.push(current);
            current = String::new();
            used = 0;
        }
        current.push(ch);
        used += width;
    }
    out.push(current);
    out
}

fn wrap_styled_line(line: &StyledLine, cols: usize) -> Vec<StyledLine> {
    let cols = cols.max(1);
    if line.is_empty() || styled_width(line) == 0 {
        return vec![Vec::new()];
    }

    let mut out = Vec::new();
    let mut current = Vec::new();
    let mut used = 0usize;
    for segment in line {
        for ch in segment.text.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width == 0 {
                continue;
            }
            if used > 0 && used + width > cols {
                out.push(current);
                current = Vec::new();
                used = 0;
            }
            push_char_segment(&mut current, ch, segment.style);
            used += width;
        }
    }
    out.push(current);
    out
}

/// ハイライト済みの1行を描画用の行にする。
fn styled_code(line: &HighlightedLine) -> StyledLine {
    line.iter()
        .map(|(fg, text)| StyledSegment {
            text: text.clone(),
            style: TextStyle {
                fg: *fg,
                bold: false,
            },
        })
        .collect()
}

/// 本文を1列ぶん右へ寄せて折り返す（左端に文字が貼り付くのを避ける）。
fn indent_wrapped(line: &StyledLine, cols: usize) -> Vec<StyledLine> {
    wrap_styled_line(line, cols.saturating_sub(BODY_MARGIN).max(1))
        .into_iter()
        .map(|wrapped| {
            let mut out = vec![StyledSegment {
                text: " ".repeat(BODY_MARGIN),
                style: TextStyle {
                    fg: Color::White,
                    bold: false,
                },
            }];
            out.extend(wrapped);
            out
        })
        .collect()
}

/// 左に行番号を添えて折り返す。nvim と同じく、折り返した2行目以降の番号は空にする。
fn number_and_wrap(source: Vec<StyledLine>, cols: usize, numbered: bool) -> Vec<StyledLine> {
    if !numbered {
        return source
            .iter()
            .flat_map(|line| indent_wrapped(line, cols))
            .collect();
    }

    let gutter = gutter_width(source.len());
    let body_cols = cols.saturating_sub(gutter).max(1);
    let mut out = Vec::new();
    for (index, line) in source.iter().enumerate() {
        for (row, wrapped) in wrap_styled_line(line, body_cols).into_iter().enumerate() {
            let label = if row == 0 {
                format!("{:>width$} ", index + 1, width = gutter - 1)
            } else {
                " ".repeat(gutter)
            };
            let mut cells = vec![StyledSegment {
                text: label,
                style: TextStyle {
                    fg: LINE_NUMBER_FG,
                    bold: false,
                },
            }];
            cells.extend(wrapped);
            out.push(cells);
        }
    }
    out
}

/// 行番号の桁数＋区切りの空白1つ。
fn gutter_width(lines: usize) -> usize {
    let digits = lines.max(1).to_string().len();
    digits.max(2) + 1
}

fn push_char_segment(line: &mut StyledLine, ch: char, style: TextStyle) {
    line.push(StyledSegment {
        text: ch.to_string(),
        style,
    });
}

fn styled_width(line: &StyledLine) -> usize {
    line.iter()
        .map(|segment| display_width(&segment.text))
        .sum()
}

pub(crate) fn render_markdown(src: &str) -> Vec<StyledLine> {
    let mut renderer = MarkdownRenderer::new();
    let parser = Parser::new_ext(src, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    for event in parser {
        renderer.handle(event);
    }
    renderer.finish()
}

struct MarkdownRenderer {
    lines: Vec<StyledLine>,
    current: StyledLine,
    style: TextStyle,
    list_depth: usize,
    in_code_block: bool,
    /// コードブロックは言語ごとに色を付けたいので、閉じるまで溜めてから流す。
    code_lang: String,
    code_buffer: String,
    /// 表は列幅を揃えたいので、閉じるまでセルを溜める。
    table: Option<TableBuilder>,
    image_alt: Option<String>,
}

/// 組み立て中の表。
#[derive(Default)]
struct TableBuilder {
    rows: Vec<Vec<StyledLine>>,
    row: Vec<StyledLine>,
    /// 見出し行の数（0 or 1）。この下に罫線を引く。
    head_rows: usize,
}

impl MarkdownRenderer {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            current: Vec::new(),
            style: TextStyle {
                fg: Color::White,
                bold: false,
            },
            list_depth: 0,
            in_code_block: false,
            code_lang: String::new(),
            code_buffer: String::new(),
            table: None,
            image_alt: None,
        }
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text, false),
            Event::Code(text) => self.text(&text, true),
            Event::SoftBreak | Event::HardBreak => self.flush_current(),
            Event::Rule => self
                .lines
                .push(styled_plain(" ────".to_owned(), Color::BrightBlack)),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { level, .. } => {
                self.blank_line_before_block();
                let (mark, fg) = heading_style(level);
                self.style = TextStyle { fg, bold: true };
                self.push_text(mark);
            }
            Tag::Paragraph => self.blank_line_before_block(),
            Tag::List(_) => {
                self.blank_line_before_block();
                self.list_depth += 1;
            }
            Tag::Item => {
                self.flush_current();
                self.push_text(&format!(
                    "{}• ",
                    "  ".repeat(self.list_depth.saturating_sub(1))
                ));
            }
            Tag::CodeBlock(kind) => {
                self.blank_line_before_block();
                self.in_code_block = true;
                self.code_lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.split_whitespace().next().unwrap_or("").to_owned(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code_buffer.clear();
                self.style = TextStyle {
                    fg: Color::Green,
                    bold: false,
                };
            }
            Tag::BlockQuote(_) => {
                self.blank_line_before_block();
                self.current.push(StyledSegment {
                    text: "│ ".to_owned(),
                    style: TextStyle {
                        fg: Color::BrightBlack,
                        bold: false,
                    },
                });
            }
            Tag::Table(_) => {
                self.blank_line_before_block();
                self.table = Some(TableBuilder::default());
            }
            Tag::TableHead => self.style.bold = true,
            Tag::Strong => self.style.bold = true,
            Tag::Image { .. } => self.image_alt = Some(String::new()),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.flush_current();
                self.style = TextStyle {
                    fg: Color::White,
                    bold: false,
                };
            }
            // 段落の終わりで改行する。これが無いと次の段落が同じ行に続いてしまう。
            TagEnd::Paragraph => self.flush_current(),
            TagEnd::List(_) => self.list_depth = self.list_depth.saturating_sub(1),
            TagEnd::Item => self.flush_current(),
            TagEnd::CodeBlock => {
                self.flush_code_block();
                self.in_code_block = false;
                self.style = TextStyle {
                    fg: Color::White,
                    bold: false,
                };
            }
            TagEnd::BlockQuote(_) => {
                self.flush_current();
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.current);
                if let Some(table) = &mut self.table {
                    table.row.push(cell);
                }
            }
            TagEnd::TableHead => {
                self.style.bold = false;
                if let Some(table) = &mut self.table {
                    table.rows.push(std::mem::take(&mut table.row));
                    table.head_rows = 1;
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    table.rows.push(std::mem::take(&mut table.row));
                }
            }
            TagEnd::Table => self.flush_table(),
            TagEnd::Strong => self.style.bold = false,
            TagEnd::Image => {
                if let Some(alt) = self.image_alt.take() {
                    self.push_text(&format!("[画像: {alt}]"));
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str, inline_code: bool) {
        if let Some(alt) = &mut self.image_alt {
            alt.push_str(text);
            return;
        }

        let previous = self.style;
        if inline_code {
            self.style = TextStyle {
                fg: Color::Yellow,
                bold: previous.bold,
            };
        }

        if self.in_code_block {
            // 閉じるまで溜める（言語ごとのハイライトはブロック全体で解析するため）。
            self.code_buffer.push_str(text);
        } else {
            self.push_text(text);
        }

        if inline_code {
            self.style = previous;
        }
    }

    /// 段落・見出し・コードブロックの前に空行を1つ入れる（詰まって見えないように）。
    /// 箇条書きの途中には入れない（1項目ずつ離れると逆に読みにくい）。
    fn blank_line_before_block(&mut self) {
        // 行の途中（引用の「│ 」や箇条書きの「• 」を置いた直後）なら、その飾りを
        // 本文から切り離さないよう何もしない。
        if !self.current.is_empty() {
            return;
        }
        self.flush_current();
        if self.list_depth > 0 || self.lines.is_empty() {
            return;
        }
        if self.lines.last().is_some_and(|line| styled_width(line) > 0) {
            self.lines.push(Vec::new());
        }
    }

    /// 溜めた表を流す。列幅を中身に合わせて揃え、`│` で区切る。
    /// 見出し行の下には罫線を引く（どこまでが見出しか分かるように）。
    fn flush_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        let widths = table_column_widths(&table.rows);
        if widths.is_empty() {
            return;
        }

        for (index, row) in table.rows.iter().enumerate() {
            self.lines.push(table_row_line(row, &widths));
            if index + 1 == table.head_rows {
                self.lines.push(table_rule_line(&widths));
            }
        }
    }

    /// 溜めたコードブロックを流す。言語が分かれば色分けし、分からなければ従来どおり緑。
    fn flush_code_block(&mut self) {
        self.flush_current();
        let code = self.code_buffer.trim_end_matches('\n').to_owned();
        if code.is_empty() {
            return;
        }

        match highlight::highlight_code_block(&code, &self.code_lang) {
            Some(highlighted) => self
                .lines
                .extend(highlighted.iter().map(styled_code)),
            None => self.lines.extend(
                code.split('\n')
                    .map(|line| styled_plain(line.to_owned(), Color::Green)),
            ),
        }
        self.code_buffer.clear();
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.current.push(StyledSegment {
            text: text.to_owned(),
            style: self.style,
        });
    }

    fn flush_current(&mut self) {
        if self.current.is_empty() {
            return;
        }
        self.lines.push(std::mem::take(&mut self.current));
    }

    fn finish(mut self) -> Vec<StyledLine> {
        self.flush_current();
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
        self.lines
    }
}

/// 各列の幅（いちばん長いセルに合わせる）。
fn table_column_widths(rows: &[Vec<StyledLine>]) -> Vec<usize> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(styled_width)
                .max()
                .unwrap_or(0)
        })
        .collect()
}

fn table_row_line(row: &[StyledLine], widths: &[usize]) -> StyledLine {
    let mut line = StyledLine::new();
    for (column, cell) in row.iter().enumerate() {
        if column > 0 {
            line.push(dim_segment(" │ "));
        }
        line.extend(cell.iter().cloned());
        // 最後の列は余白を足さない（右端に無駄な空白を作らない）。
        if column + 1 < row.len() {
            let pad = widths[column].saturating_sub(styled_width(cell));
            if pad > 0 {
                line.push(dim_segment(&" ".repeat(pad)));
            }
        }
    }
    line
}

fn table_rule_line(widths: &[usize]) -> StyledLine {
    let rule = widths
        .iter()
        .map(|width| "─".repeat(*width))
        .collect::<Vec<_>>()
        .join("─┼─");
    vec![dim_segment(&rule)]
}

fn dim_segment(text: &str) -> StyledSegment {
    StyledSegment {
        text: text.to_owned(),
        style: TextStyle {
            fg: Color::BrightBlack,
            bold: false,
        },
    }
}

/// 見出しの印と色。`#` の数を数えなくてもレベルが分かるよう、記号と色で表す。
/// 絵文字は使わない（このフォント構成では字が出ず、空白になってしまう）。
fn heading_style(level: HeadingLevel) -> (&'static str, Color) {
    match level {
        HeadingLevel::H1 => ("■ ", Color::Rgb { rgba: 0x7AA2_F7FF }),
        HeadingLevel::H2 => ("◆ ", Color::Rgb { rgba: 0x2AC3_DEFF }),
        HeadingLevel::H3 => ("▸ ", Color::Rgb { rgba: 0x9ECE_6AFF }),
        // H4 以下は深さぶん下げて、印を小さくする。
        HeadingLevel::H4 => ("  ▸ ", Color::Rgb { rgba: 0x9ECE_6AFF }),
        HeadingLevel::H5 => ("  · ", Color::Rgb { rgba: 0x565F_89FF }),
        HeadingLevel::H6 => ("  · ", Color::Rgb { rgba: 0x565F_89FF }),
    }
}

fn is_markdown_extension(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
}

fn abbreviate_start(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "\u{2026}".to_owned();
    }

    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars().rev() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width == 0 || used + width > max_width - 1 {
            break;
        }
        out.insert(0, ch);
        used += width;
    }
    format!("\u{2026}{out}")
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

pub(crate) fn clamp_reader_scroll(offset: usize, len: usize, visible: usize) -> usize {
    offset.min(reader_scroll_max(len, visible))
}

fn reader_scroll_max(len: usize, visible: usize) -> usize {
    len.saturating_sub(visible)
}

#[cfg(test)]
mod tests {
    use super::{
        clamp_reader_scroll, diff_line_color, gutter_width, number_and_wrap, reader_key_intent,
        reader_scroll_max, render_markdown, styled_plain, wrap_line, Color, KeyCode,
        ReaderHeaderAction, ReaderKeyIntent, StyledLine,
    };

    /// terminal::Color は比較できないので、色の数値で見る。
    fn rgba_of(color: Color) -> u32 {
        match color {
            Color::Rgb { rgba } => rgba,
            other => panic!("Rgb 色のはず: {other:?}"),
        }
    }

    fn line_text(line: &StyledLine) -> String {
        line.iter().map(|segment| segment.text.as_str()).collect()
    }

    fn rendered_text(src: &str) -> Vec<String> {
        render_markdown(src).iter().map(line_text).collect()
    }

    #[test]
    fn clamp_reader_scroll_stays_within_visible_range() {
        assert_eq!(clamp_reader_scroll(0, 20, 5), 0);
        assert_eq!(clamp_reader_scroll(12, 20, 5), 12);
        assert_eq!(clamp_reader_scroll(99, 20, 5), 15);
        assert_eq!(clamp_reader_scroll(3, 4, 10), 0);
    }

    #[test]
    fn reader_keys_scroll_like_the_sidebar() {
        assert_eq!(reader_key_intent(KeyCode::KeyK), ReaderKeyIntent::Scroll(-1));
        assert_eq!(
            reader_key_intent(KeyCode::ArrowUp),
            ReaderKeyIntent::Scroll(-1)
        );
        assert_eq!(reader_key_intent(KeyCode::KeyJ), ReaderKeyIntent::Scroll(1));
        assert_eq!(
            reader_key_intent(KeyCode::ArrowDown),
            ReaderKeyIntent::Scroll(1)
        );
        assert_eq!(reader_key_intent(KeyCode::PageUp), ReaderKeyIntent::Page(-1));
        assert_eq!(
            reader_key_intent(KeyCode::PageDown),
            ReaderKeyIntent::Page(1)
        );
        assert_eq!(reader_key_intent(KeyCode::Home), ReaderKeyIntent::Top);
        assert_eq!(reader_key_intent(KeyCode::End), ReaderKeyIntent::Bottom);
    }

    #[test]
    fn escape_releases_focus_and_header_keys_act() {
        assert_eq!(reader_key_intent(KeyCode::Escape), ReaderKeyIntent::Release);
        assert_eq!(
            reader_key_intent(KeyCode::KeyE),
            ReaderKeyIntent::Header(ReaderHeaderAction::Edit)
        );
        assert_eq!(
            reader_key_intent(KeyCode::KeyO),
            ReaderKeyIntent::Header(ReaderHeaderAction::OpenWithSystem)
        );
    }

    #[test]
    fn other_keys_are_swallowed_not_sent_to_the_shell() {
        assert_eq!(reader_key_intent(KeyCode::KeyA), ReaderKeyIntent::Ignore);
        assert_eq!(reader_key_intent(KeyCode::Enter), ReaderKeyIntent::Ignore);
    }

    /// 追従中にスクロールしたら、いま見えている末尾の位置で固定する（tail -f と同じ）。
    #[test]
    fn pin_position_keeps_the_tail_in_view() {
        // 100行を20行の枠で追従中 → 固定位置は末尾ページの先頭（80行目）。
        assert_eq!(reader_scroll_max(100, 20), 80);
        // 枠より短い内容は先頭のまま（上に空白を作らない）。
        assert_eq!(reader_scroll_max(5, 20), 0);
    }

    #[test]
    fn line_numbers_label_only_the_first_row_of_a_wrapped_line() {
        let source = vec![
            styled_plain("abcdef".to_owned(), Color::White),
            styled_plain("x".to_owned(), Color::White),
        ];
        // 桁数2 + 空白1 = 溝3、本文3列で "abcdef" は2行に折り返る。
        let out = number_and_wrap(source, 6, true);

        let texts: Vec<String> = out.iter().map(line_text).collect();
        assert_eq!(texts, vec![" 1 abc", "   def", " 2 x"]);
    }

    #[test]
    fn line_numbers_are_dropped_for_tail_only_files() {
        let source = vec![styled_plain("abc".to_owned(), Color::White)];
        // 末尾だけ読んだファイルは先頭が1行目とは限らないので番号を出さない。
        let out = number_and_wrap(source, 6, false);

        assert_eq!(line_text(&out[0]), " abc", "左余白だけ付く");
    }

    #[test]
    fn gutter_grows_with_the_line_count() {
        assert_eq!(gutter_width(9), 3); // " 9 "
        assert_eq!(gutter_width(120), 4); // "120 "
        assert_eq!(gutter_width(0), 3); // 空でも桁を詰めすぎない
    }

    #[test]
    fn markdown_separates_blocks_with_a_blank_line() {
        let lines = rendered_text("# Title\npara one\n\npara two");

        // 見出しと段落、段落同士の間に空行が1つ入る（詰まって見えないように）。
        assert_eq!(lines, vec!["■ Title", "", "para one", "", "para two"]);
    }

    #[test]
    fn markdown_table_aligns_columns_and_rules_the_head() {
        let lines = rendered_text("| key | note |\n|---|---|\n| a | long value |\n| bb | x |");

        assert_eq!(
            lines,
            vec![
                "key │ note",
                "────┼───────────",
                "a   │ long value",
                "bb  │ x",
            ],
            "列幅が揃い、見出しの下に罫線が入る"
        );
    }

    #[test]
    fn markdown_headings_use_a_mark_per_level() {
        let lines = rendered_text("# one\n\n## two\n\n### three");

        assert_eq!(
            lines.iter().filter(|line| !line.is_empty()).collect::<Vec<_>>(),
            vec!["■ one", "◆ two", "▸ three"],
            "レベルごとに印が変わる（# を数えなくて済む）"
        );
    }

    #[test]
    fn markdown_list_items_stay_together() {
        let lines = rendered_text("- one\n- two");

        assert_eq!(lines, vec!["• one", "• two"], "箇条書きの間は空けない");
    }

    #[test]
    fn markdown_code_fence_is_highlighted_by_language() {
        let lines = render_markdown("```rust\nlet x = 1; // hi\n```");
        let code = lines.last().expect("コード行");

        // 素の緑1色ではなく、断片ごとに色が分かれている。
        let colors: Vec<u32> = code.iter().map(|segment| rgba_of(segment.style.fg)).collect();
        assert!(
            colors.windows(2).any(|pair| pair[0] != pair[1]),
            "コードが色分けされていない: {code:?}"
        );
    }

    #[test]
    fn markdown_code_fence_without_language_stays_plain() {
        let lines = render_markdown("```\nsome text\n```");
        let code = lines.last().expect("コード行");

        assert_eq!(line_text(code), "some text");
        assert!(matches!(code[0].style.fg, Color::Green), "従来どおり緑のまま");
    }

    #[test]
    fn wrap_line_wraps_ascii() {
        assert_eq!(wrap_line("abcdef", 3), vec!["abc", "def"]);
    }

    #[test]
    fn wrap_line_wraps_mixed_width_text() {
        assert_eq!(wrap_line("ab日本cd", 4), vec!["ab日", "本cd"]);
    }

    #[test]
    fn wrap_line_keeps_exact_boundary() {
        assert_eq!(wrap_line("abcd", 4), vec!["abcd"]);
    }

    #[test]
    fn wrap_line_preserves_empty_line() {
        assert_eq!(wrap_line("", 4), vec![""]);
    }

    #[test]
    fn diff_line_color_classifies_unified_diff_lines() {
        assert!(matches!(
            diff_line_color("@@ -1,3 +1,4 @@"),
            crate::terminal::Color::Cyan
        ));
        assert!(matches!(
            diff_line_color("+added"),
            crate::terminal::Color::Green
        ));
        assert!(matches!(
            diff_line_color("-removed"),
            crate::terminal::Color::Red
        ));
        assert!(matches!(
            diff_line_color("+++ b/x"),
            crate::terminal::Color::BrightBlack
        ));
        assert!(matches!(
            diff_line_color("--- a/x"),
            crate::terminal::Color::BrightBlack
        ));
        assert!(matches!(
            diff_line_color("diff --git a/x b/x"),
            crate::terminal::Color::BrightBlack
        ));
        assert!(matches!(
            diff_line_color(" context"),
            crate::terminal::Color::White
        ));
    }

    #[test]
    fn render_markdown_formats_representative_blocks() {
        let lines = rendered_text(
            "# Title\n\nnormal `code`\n\n- item\n\n> quote\n\n```rust\nlet x = 1;\n```\n\n---\n\n![alt](image.png)",
        );

        assert!(lines.contains(&"■ Title".to_owned()));
        assert!(lines.contains(&"normal code".to_owned()));
        assert!(lines.contains(&"• item".to_owned()));
        assert!(lines.contains(&"│ quote".to_owned()));
        assert!(lines.contains(&"let x = 1;".to_owned()));
        assert!(lines.contains(&" ────".to_owned()));
        assert!(lines.contains(&"[画像: alt]".to_owned()));
    }
}
