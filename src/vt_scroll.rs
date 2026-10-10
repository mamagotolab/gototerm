//! Forward VT operations unchanged, recording explicit grid rotations for TUI copying.
use super::EventProxy;
use crate::tui_copy::ScrollTrace;
use alacritty_terminal::{
    grid::Dimensions,
    term::{Term, TermMode},
};
use std::{
    ops::Range,
    sync::{Arc, Mutex},
};
use vte::ansi::cursor_icon::CursorIcon;
use vte::ansi::*;

pub(super) struct Processor {
    inner: vte::ansi::Processor,
    pub trace: Arc<Mutex<ScrollTrace>>,
    region: Range<usize>,
    size: (usize, usize),
}
impl Processor {
    pub fn new() -> Self {
        Self {
            inner: vte::ansi::Processor::new(),
            trace: Arc::default(),
            region: 0..0,
            size: (0, 0),
        }
    }
    pub fn advance(&mut self, term: &mut Term<EventProxy>, bytes: &[u8]) {
        let mut trace = self.trace.lock().unwrap();
        let size = (term.columns(), term.screen_lines());
        if size != self.size {
            self.size = size;
            self.region = 0..size.1;
            trace.push(None);
            trace.pending.clear();
        }
        self.inner.advance(
            &mut Tracked {
                term,
                trace: &mut trace,
                region: &mut self.region,
            },
            bytes,
        );
    }
}
struct Tracked<'a> {
    term: &'a mut Term<EventProxy>,
    trace: &'a mut ScrollTrace,
    region: &'a mut Range<usize>,
}
impl Tracked<'_> {
    fn record(&mut self, start: usize, count: usize, down: bool) {
        let region = start..self.region.end;
        let count = count.min(region.len());
        if count > 0 {
            let delta = if down { -(count as i64) } else { count as i64 };
            self.trace.pending = self
                .trace
                .pending
                .iter()
                .filter_map(|&row| {
                    if !region.contains(&row) {
                        return Some(row);
                    }
                    let moved = row as i64 - delta;
                    (moved >= region.start as i64 && moved < region.end as i64)
                        .then_some(moved as usize)
                })
                .collect();
            let exposed = if down {
                region.start..region.start + count
            } else {
                region.end - count..region.end
            };
            self.trace.pending.extend(exposed);
            self.trace.push(Some((delta, region)));
        }
    }
    fn feed(&mut self) {
        if self.term.grid().cursor.point.line.0 + 1 == self.region.end as i32 {
            self.record(self.region.start, 1, false);
        }
    }
}
impl Handler for Tracked<'_> {
    fn input(&mut self, c: char) {
        use unicode_width::UnicodeWidthChar;
        let cursor = &self.term.grid().cursor;
        // Implicit wrapping is not an application scroll request. Invalidate a
        // pending hint rather than misclassifying a redraw as document movement.
        if c.width().unwrap_or(0) > 0
            && self.term.mode().contains(TermMode::LINE_WRAP)
            && (cursor.input_needs_wrap
                || (c.width() == Some(2) && cursor.point.column.0 + 1 >= self.term.columns()))
            && cursor.point.line.0 + 1 == self.region.end as i32
        {
            self.trace.push(None);
        }
        if c.width().unwrap_or(0) > 0 {
            // Use the post-input row since a character can wrap first.
            self.term.input(c);
            self.trace
                .pending
                .remove(&(self.term.grid().cursor.point.line.0 as usize));
        } else {
            self.term.input(c);
        }
    }
    fn linefeed(&mut self) {
        self.feed();
        self.term.linefeed();
    }
    fn newline(&mut self) {
        self.feed();
        self.term.newline();
    }
    fn reverse_index(&mut self) {
        if self.term.grid().cursor.point.line.0 == self.region.start as i32 {
            self.record(self.region.start, 1, true);
        }
        self.term.reverse_index();
    }
    fn scroll_up(&mut self, n: usize) {
        self.record(self.region.start, n, false);
        self.term.scroll_up(n);
    }
    fn scroll_down(&mut self, n: usize) {
        self.record(self.region.start, n, true);
        self.term.scroll_down(n);
    }
    fn delete_lines(&mut self, n: usize) {
        let row = self.term.grid().cursor.point.line.0 as usize;
        if self.region.contains(&row) {
            self.record(row, n, false);
        }
        self.term.delete_lines(n);
    }
    fn insert_blank_lines(&mut self, n: usize) {
        let row = self.term.grid().cursor.point.line.0 as usize;
        if self.region.contains(&row) {
            self.record(row, n, true);
        }
        self.term.insert_blank_lines(n);
    }
    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        let end = bottom.unwrap_or(self.term.screen_lines());
        if top < end {
            *self.region = top.saturating_sub(1).min(self.term.screen_lines())
                ..end.min(self.term.screen_lines());
        }
        self.term.set_scrolling_region(top, bottom);
    }
    fn reset_state(&mut self) {
        self.trace.push(None);
        self.trace.pending.clear();
        *self.region = 0..self.term.screen_lines();
        self.term.reset_state();
    }
    fn clear_screen(&mut self, mode: ClearMode) {
        let row = self.term.grid().cursor.point.line.0 as usize;
        let col = self.term.grid().cursor.point.column.0;
        match mode {
            ClearMode::All => {
                self.trace.push(None);
                self.trace.pending.clear();
            }
            ClearMode::Below => self
                .trace
                .pending
                .retain(|r| *r < row + usize::from(col != 0)),
            ClearMode::Above => self
                .trace
                .pending
                .retain(|r| *r >= row + usize::from(col + 1 == self.term.columns())),
            ClearMode::Saved => {}
        }
        self.term.clear_screen(mode);
    }
    fn set_private_mode(&mut self, mode: PrivateMode) {
        let alt = self.term.mode().contains(TermMode::ALT_SCREEN);
        self.term.set_private_mode(mode);
        if alt != self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.trace.push(None);
            self.trace.pending.clear();
        }
    }
    fn unset_private_mode(&mut self, mode: PrivateMode) {
        let alt = self.term.mode().contains(TermMode::ALT_SCREEN);
        self.term.unset_private_mode(mode);
        if alt != self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.trace.push(None);
            self.trace.pending.clear();
        }
    }
    fn set_title(&mut self, a0: Option<String>) {
        self.term.set_title(a0);
    }
    fn set_cursor_style(&mut self, a0: Option<CursorStyle>) {
        self.term.set_cursor_style(a0);
    }
    fn set_cursor_shape(&mut self, a0: CursorShape) {
        self.term.set_cursor_shape(a0);
    }
    fn goto(&mut self, a0: i32, a1: usize) {
        self.term.goto(a0, a1);
    }
    fn goto_line(&mut self, a0: i32) {
        self.term.goto_line(a0);
    }
    fn goto_col(&mut self, a0: usize) {
        self.term.goto_col(a0);
    }
    fn insert_blank(&mut self, a0: usize) {
        self.term.insert_blank(a0);
    }
    fn move_up(&mut self, a0: usize) {
        self.term.move_up(a0);
    }
    fn move_down(&mut self, a0: usize) {
        self.term.move_down(a0);
    }
    fn identify_terminal(&mut self, a0: Option<char>) {
        self.term.identify_terminal(a0);
    }
    fn device_status(&mut self, a0: usize) {
        self.term.device_status(a0);
    }
    fn move_forward(&mut self, a0: usize) {
        self.term.move_forward(a0);
    }
    fn move_backward(&mut self, a0: usize) {
        self.term.move_backward(a0);
    }
    fn move_down_and_cr(&mut self, a0: usize) {
        self.term.move_down_and_cr(a0);
    }
    fn move_up_and_cr(&mut self, a0: usize) {
        self.term.move_up_and_cr(a0);
    }
    fn put_tab(&mut self, a0: u16) {
        self.term.put_tab(a0);
    }
    fn backspace(&mut self) {
        self.term.backspace();
    }
    fn carriage_return(&mut self) {
        self.term.carriage_return();
    }
    fn bell(&mut self) {
        self.term.bell();
    }
    fn substitute(&mut self) {
        self.term.substitute();
    }
    fn set_horizontal_tabstop(&mut self) {
        self.term.set_horizontal_tabstop();
    }
    fn erase_chars(&mut self, a0: usize) {
        self.term.erase_chars(a0);
    }
    fn delete_chars(&mut self, a0: usize) {
        self.term.delete_chars(a0);
    }
    fn move_backward_tabs(&mut self, a0: u16) {
        self.term.move_backward_tabs(a0);
    }
    fn move_forward_tabs(&mut self, a0: u16) {
        self.term.move_forward_tabs(a0);
    }
    fn save_cursor_position(&mut self) {
        self.term.save_cursor_position();
    }
    fn restore_cursor_position(&mut self) {
        self.term.restore_cursor_position();
    }
    fn clear_line(&mut self, mode: LineClearMode) {
        let cursor = &self.term.grid().cursor;
        let complete = match mode {
            LineClearMode::All => true,
            LineClearMode::Right => cursor.point.column.0 == 0,
            LineClearMode::Left => cursor.point.column.0 + 1 == self.term.columns(),
        };
        if complete {
            self.trace.pending.remove(&(cursor.point.line.0 as usize));
        }
        self.term.clear_line(mode);
    }
    fn clear_tabs(&mut self, a0: TabulationClearMode) {
        self.term.clear_tabs(a0);
    }
    fn set_tabs(&mut self, a0: u16) {
        self.term.set_tabs(a0);
    }
    fn terminal_attribute(&mut self, a0: Attr) {
        self.term.terminal_attribute(a0);
    }
    fn set_mode(&mut self, a0: Mode) {
        self.term.set_mode(a0);
    }
    fn unset_mode(&mut self, a0: Mode) {
        self.term.unset_mode(a0);
    }
    fn report_mode(&mut self, a0: Mode) {
        self.term.report_mode(a0);
    }
    fn report_private_mode(&mut self, a0: PrivateMode) {
        self.term.report_private_mode(a0);
    }
    fn set_keypad_application_mode(&mut self) {
        self.term.set_keypad_application_mode();
    }
    fn unset_keypad_application_mode(&mut self) {
        self.term.unset_keypad_application_mode();
    }
    fn set_active_charset(&mut self, a0: CharsetIndex) {
        self.term.set_active_charset(a0);
    }
    fn configure_charset(&mut self, a0: CharsetIndex, a1: StandardCharset) {
        self.term.configure_charset(a0, a1);
    }
    fn set_color(&mut self, a0: usize, a1: Rgb) {
        self.term.set_color(a0, a1);
    }
    fn dynamic_color_sequence(&mut self, a0: String, a1: usize, a2: &str) {
        self.term.dynamic_color_sequence(a0, a1, a2);
    }
    fn reset_color(&mut self, a0: usize) {
        self.term.reset_color(a0);
    }
    fn clipboard_store(&mut self, a0: u8, a1: &[u8]) {
        self.term.clipboard_store(a0, a1);
    }
    fn clipboard_load(&mut self, a0: u8, a1: &str) {
        self.term.clipboard_load(a0, a1);
    }
    fn decaln(&mut self) {
        self.term.decaln();
    }
    fn push_title(&mut self) {
        self.term.push_title();
    }
    fn pop_title(&mut self) {
        self.term.pop_title();
    }
    fn text_area_size_pixels(&mut self) {
        self.term.text_area_size_pixels();
    }
    fn text_area_size_chars(&mut self) {
        self.term.text_area_size_chars();
    }
    fn set_hyperlink(&mut self, a0: Option<Hyperlink>) {
        self.term.set_hyperlink(a0);
    }
    fn set_mouse_cursor_icon(&mut self, a0: CursorIcon) {
        self.term.set_mouse_cursor_icon(a0);
    }
    fn report_keyboard_mode(&mut self) {
        self.term.report_keyboard_mode();
    }
    fn push_keyboard_mode(&mut self, a0: KeyboardModes) {
        self.term.push_keyboard_mode(a0);
    }
    fn pop_keyboard_modes(&mut self, a0: u16) {
        self.term.pop_keyboard_modes(a0);
    }
    fn set_keyboard_mode(&mut self, a0: KeyboardModes, a1: KeyboardModesApplyBehavior) {
        self.term.set_keyboard_mode(a0, a1);
    }
    fn set_modify_other_keys(&mut self, a0: ModifyOtherKeys) {
        self.term.set_modify_other_keys(a0);
    }
    fn report_modify_other_keys(&mut self) {
        self.term.report_modify_other_keys();
    }
    fn set_scp(&mut self, a0: ScpCharPath, a1: ScpUpdateMode) {
        self.term.set_scp(a0, a1);
    }
}
