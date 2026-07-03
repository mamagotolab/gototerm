use std::path::{Path, PathBuf};

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthChar;
use winit::dpi::PhysicalPosition;

use crate::config::resolve_editor;
use crate::preview::{FilePreview, PreviewLines};
use crate::terminal::{Cell, Color, GraphicAttribute, Line};
use crate::view::{TerminalView, Viewport};
use crate::Display;

const PLACEHOLDER: &str = "ファイルを選ぶか、AI がファイルを書くとここに表示されます";
const READER_WRAP_MAX: usize = 100;

pub struct ReaderPane {
    view: TerminalView,
    preview: FilePreview,
    pinned: bool,
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
        self.refresh_reader_document();
        self.rebuild();
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

    pub fn on_scroll(&mut self, delta: i32) {
        if delta != 0 {
            self.scroll_by(-(delta as isize));
        }
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
        let next = if delta.is_negative() {
            self.reader_scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.reader_scroll.saturating_add(delta as usize)
        };
        self.set_scroll(next);
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

        lines.truncate(rows);
        row_actions.truncate(rows);
        self.row_actions = row_actions;
        self.view.update_contents(|view| {
            // 透過は残しつつ罫線・文字が読める中間の背景（ターミナルより少し濃い）。
            view.bg_color = crate::view::panel_bg_color();
            view.lines = lines;
            view.images = Vec::new();
            view.cursor = None;
            view.selection_range = None;
        });
    }

    fn refresh_reader_document(&mut self) {
        self.reader_lines = if self.preview.target().is_none() {
            vec![styled_plain(PLACEHOLDER.to_owned(), Color::BrightBlack)]
        } else {
            match self.preview.lines() {
                PreviewLines::Text(text_lines) => {
                    if self.pinned
                        && self
                            .preview
                            .target()
                            .and_then(Path::extension)
                            .and_then(|ext| ext.to_str())
                            .is_some_and(is_markdown_extension)
                    {
                        render_markdown(&text_lines.join("\n"))
                            .into_iter()
                            .flat_map(|line| wrap_styled_line(&line, self.reader_wrap_cols()))
                            .collect()
                    } else {
                        text_lines
                            .iter()
                            .flat_map(|line| wrap_line(line, self.reader_wrap_cols()))
                            .map(|line| styled_plain(line, Color::White))
                            .collect()
                    }
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
            + usize::from(self.preview.target_abs().is_some()) * 2
            + usize::from(self.reader_notice.is_some())
            + usize::from(self.preview.target().is_some());
        rows.saturating_sub(header_rows)
    }
}

pub enum ReaderRequest {
    EditFile(PathBuf),
    OpenWithSystem(PathBuf),
}

#[derive(Clone, Copy)]
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
    image_alt: Option<String>,
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
                self.flush_current();
                self.style = TextStyle {
                    fg: Color::Cyan,
                    bold: true,
                };
                self.push_text(&format!("{} ", heading_marks(level)));
            }
            Tag::List(_) => self.list_depth += 1,
            Tag::Item => {
                self.flush_current();
                self.push_text(&format!(
                    "{}• ",
                    "  ".repeat(self.list_depth.saturating_sub(1))
                ));
            }
            Tag::CodeBlock(_) => {
                self.flush_current();
                self.in_code_block = true;
                self.style = TextStyle {
                    fg: Color::Green,
                    bold: false,
                };
            }
            Tag::BlockQuote(_) => {
                self.flush_current();
                self.current.push(StyledSegment {
                    text: "│ ".to_owned(),
                    style: TextStyle {
                        fg: Color::BrightBlack,
                        bold: false,
                    },
                });
            }
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
            TagEnd::List(_) => self.list_depth = self.list_depth.saturating_sub(1),
            TagEnd::Item => self.flush_current(),
            TagEnd::CodeBlock => {
                self.flush_current();
                self.in_code_block = false;
                self.style = TextStyle {
                    fg: Color::White,
                    bold: false,
                };
            }
            TagEnd::BlockQuote(_) => {
                self.flush_current();
            }
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
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    self.flush_current();
                }
                self.push_text(line);
            }
        } else {
            self.push_text(text);
        }

        if inline_code {
            self.style = previous;
        }
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

fn heading_marks(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "#",
        HeadingLevel::H2 => "##",
        HeadingLevel::H3 => "###",
        HeadingLevel::H4 => "####",
        HeadingLevel::H5 => "#####",
        HeadingLevel::H6 => "######",
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
    use super::{clamp_reader_scroll, render_markdown, wrap_line, StyledLine};

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
    fn render_markdown_formats_representative_blocks() {
        let lines = rendered_text(
            "# Title\n\nnormal `code`\n\n- item\n\n> quote\n\n```rust\nlet x = 1;\n```\n\n---\n\n![alt](image.png)",
        );

        assert!(lines.contains(&"# Title".to_owned()));
        assert!(lines.contains(&"normal code".to_owned()));
        assert!(lines.contains(&"• item".to_owned()));
        assert!(lines.contains(&"│ quote".to_owned()));
        assert!(lines.contains(&"let x = 1;".to_owned()));
        assert!(lines.contains(&" ────".to_owned()));
        assert!(lines.contains(&"[画像: alt]".to_owned()));
    }
}
