//! Temporary text captured only while copying across application scrolling.
use alacritty_terminal::selection::SelectionType;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub cells: Vec<String>,
    pub wrapped: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub rows: Vec<Row>,
    pub alternate: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Point {
    pub row: i64,
    pub col: usize,
}
pub(crate) struct TuiCopy {
    pub frame: Frame,
    rows: BTreeMap<i64, Row>,
    pub offset: i64,
    pub cursor: Point,
    anchor: Option<Point>,
    kind: SelectionType,
    body: Option<std::ops::Range<usize>>,
    limit: usize,
    pub stopped: Option<&'static str>,
}
impl TuiCopy {
    pub fn new(frame: Frame, row: usize, col: usize, limit: usize) -> Self {
        let rows = frame
            .rows
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, r)| (i as i64, r))
            .collect();
        Self {
            frame,
            rows,
            offset: 0,
            cursor: Point {
                row: row as i64,
                col,
            },
            anchor: None,
            kind: SelectionType::Simple,
            body: None,
            limit,
            stopped: None,
        }
    }
    pub fn select(&mut self, kind: SelectionType) {
        if self.anchor.is_some() && self.kind == kind {
            self.anchor = None;
        } else {
            self.kind = kind;
            self.anchor = Some(self.cursor);
        }
    }
    pub fn stop(&mut self, reason: &'static str) {
        self.stopped = Some(reason);
    }
    pub fn observe_scroll(&mut self, frame: Frame, down: bool) -> bool {
        if let Some((delta, _)) = scroll_match(&self.frame, &frame, self.body.as_ref()) {
            if (delta > 0) != down {
                self.stop("要求と異なる画面移動のためコピー継続を停止しました");
                return false;
            }
        }
        self.observe(frame)
    }
    pub fn observe(&mut self, frame: Frame) -> bool {
        if self.stopped.is_some() {
            return false;
        }
        if self.frame.alternate != frame.alternate || self.frame.rows.len() != frame.rows.len() {
            self.stop("画面切替・サイズ変更でコピー継続を停止しました");
            return false;
        }
        if self.frame == frame {
            return true;
        }
        if let Some(body) = &self.body {
            if self.frame.rows[body.clone()] == frame.rows[body.clone()] {
                self.frame = frame;
                return true;
            }
        }
        let Some((delta, body)) = scroll_match(&self.frame, &frame, self.body.as_ref()) else {
            self.stop("本文の連続性を確認できません。保持済みの範囲はコピーできます");
            return false;
        };
        let offset = self.offset + delta;
        if self.body.is_none()
            && self
                .anchor
                .is_some_and(|p| p.row < body.start as i64 || p.row >= body.end as i64)
        {
            self.stop("本文外の選択からはコピーを継続できません");
            return false;
        }
        let mut rows = self.rows.clone();
        if self.body.is_none() {
            rows.retain(|i, _| *i >= body.start as i64 && *i < body.end as i64);
        }
        for i in body.clone() {
            let index = offset + i as i64;
            if rows.get(&index).is_some_and(|r| r != &frame.rows[i]) {
                self.stop("保持した本文が変更されたためコピー継続を停止しました");
                return false;
            }
            rows.insert(index, frame.rows[i].clone());
        }
        if rows.len() > self.limit {
            self.stop("コピー履歴の上限です。保持済みの範囲をコピーしてください");
            return false;
        }
        self.rows = rows;
        self.cursor.row += delta;
        self.offset = offset;
        self.body = Some(body);
        self.frame = frame;
        true
    }
    pub fn move_cursor(&mut self, row: usize, col: usize) {
        if self.stopped.is_some() {
            return;
        }
        let body = self.body.clone().unwrap_or(0..self.frame.rows.len());
        let row = row.clamp(body.start, body.end.saturating_sub(1));
        let cells = &self.frame.rows[row].cells;
        let mut col = col.min(cells.len().saturating_sub(1));
        while col > 0 && cells[col].is_empty() {
            col -= 1;
        }
        self.cursor = Point {
            row: self.offset + row as i64,
            col,
        };
    }
    pub fn bounds(&self) -> std::ops::Range<usize> {
        self.body.clone().unwrap_or(0..self.frame.rows.len())
    }
    pub fn visible_cursor(&self) -> (usize, usize) {
        (
            (self.cursor.row - self.offset).max(0) as usize,
            self.cursor.col,
        )
    }
    pub fn selection(&self) -> Option<(Point, Point, bool)> {
        let mut a = self.anchor?;
        let mut b = self.cursor;
        if self.kind == SelectionType::Lines {
            let (lo, hi) = if a.row <= b.row {
                (a.row, b.row)
            } else {
                (b.row, a.row)
            };
            a = Point { row: lo, col: 0 };
            b = Point {
                row: hi,
                col: self.rows.get(&hi)?.cells.len().saturating_sub(1),
            };
        }
        Some((a, b, self.kind == SelectionType::Block))
    }
    pub fn text(&self) -> Option<String> {
        let (a, b, block) = self.selection()?;
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let mut text = String::new();
        for i in start.row..=end.row {
            let r = self.rows.get(&i)?;
            let left = if block {
                a.col.min(b.col)
            } else if i == start.row {
                start.col
            } else {
                0
            };
            let right = if block {
                a.col.max(b.col)
            } else if i == end.row {
                end.col
            } else {
                r.cells.len().saturating_sub(1)
            };
            let mut chunk = r
                .cells
                .iter()
                .skip(left)
                .take(right.saturating_sub(left) + 1)
                .cloned()
                .collect::<String>();
            if block || !r.wrapped {
                chunk = chunk.trim_end_matches(' ').to_owned();
            }
            text.push_str(&chunk);
            if i < end.row && (block || !r.wrapped) {
                text.push('\n');
            }
        }
        Some(text)
    }
}

/// Match a unique, contiguous displacement with at least two distinct nonblank rows.
/// Unmatched margins are headers/footers; never manufacture missing content.
fn scroll_match(
    old: &Frame,
    new: &Frame,
    body: Option<&std::ops::Range<usize>>,
) -> Option<(i64, std::ops::Range<usize>)> {
    let h = old.rows.len();
    let mut candidates = Vec::new();
    for delta in -(h as i64 - 1)..h as i64 {
        if delta == 0 {
            continue;
        }
        let mut run = None;
        for j in 0..=h {
            let i = j as i64 + delta;
            let matched = j < h && i >= 0 && i < h as i64 && old.rows[i as usize] == new.rows[j];
            if matched && run.is_none() {
                run = Some(j);
            }
            if !matched {
                if let Some(start) = run.take() {
                    let end = j;
                    let old_start = (start as i64 + delta) as usize;
                    let region = if delta > 0 {
                        start..(end as i64 + delta) as usize
                    } else {
                        old_start..end
                    };
                    if end - start < 2 || body.is_some_and(|b| b != &region) {
                        continue;
                    }
                    let distinct: std::collections::BTreeSet<_> = new.rows[start..end]
                        .iter()
                        .map(|r| r.cells.concat())
                        .filter(|s| !s.trim().is_empty())
                        .collect();
                    if distinct.len() < 2 {
                        continue;
                    }
                    candidates.push((end - start, delta, region));
                }
            }
        }
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
    let best = candidates.first()?;
    if candidates.get(1).is_some_and(|c| c.0 == best.0) {
        return None;
    }
    Some((best.1, best.2.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(rows: &[&str]) -> Frame {
        Frame {
            rows: rows
                .iter()
                .map(|r| Row {
                    cells: r.chars().map(|c| c.to_string()).collect(),
                    wrapped: false,
                })
                .collect(),
            alternate: true,
        }
    }
    #[test]
    fn wide_spacer_cursor_selects_complete_glyph() {
        let f = Frame {
            rows: vec![Row {
                cells: vec!["日".into(), "".into(), "本".into(), "".into()],
                wrapped: false,
            }],
            alternate: true,
        };
        let mut c = TuiCopy::new(f, 0, 0, 10);
        c.move_cursor(0, 1);
        c.select(SelectionType::Simple);
        assert_eq!(c.text().as_deref(), Some("日"));
    }
    #[test]
    fn request_direction_cannot_be_reversed_by_unrelated_redraw() {
        let mut c = TuiCopy::new(frame(&["one", "two", "three", ":"]), 0, 0, 100);
        c.select(SelectionType::Lines);
        assert!(!c.observe_scroll(frame(&["two", "three", "four", ":"]), false));
        assert_eq!(c.offset, 0);
        assert_eq!(c.text().as_deref(), Some("one"));
    }
    #[test]
    fn forward_scroll_preserves_anchor_and_excludes_fixed_status() {
        let mut c = TuiCopy::new(
            frame(&["header", "one", "two", "three", "status"]),
            1,
            0,
            100,
        );
        c.select(SelectionType::Lines);
        c.cursor.row = 3;
        assert!(c.observe(frame(&["header", "two", "three", "four", "status2"])));
        assert_eq!(c.offset, 1);
        assert_eq!(c.text().as_deref(), Some("one\ntwo\nthree\nfour"));
    }
    #[test]
    fn reverse_scroll_reuses_rows_without_duplication() {
        let a = frame(&["one", "two", "three", ":"]);
        let b = frame(&["two", "three", "four", ":"]);
        let mut c = TuiCopy::new(a.clone(), 0, 0, 100);
        c.select(SelectionType::Lines);
        c.cursor.row = 2;
        assert!(c.observe(b));
        assert!(c.observe(a));
        assert_eq!(c.offset, 0);
        assert_eq!(c.text().as_deref(), Some("one\ntwo\nthree"));
    }
    #[test]
    fn repeated_rows_and_full_redraw_stop_without_losing_text() {
        for (old, new) in [
            (
                frame(&["same", "same", "same"]),
                frame(&["same", "same", "different"]),
            ),
            (
                frame(&["one", "two", "three"]),
                frame(&["four", "five", "six"]),
            ),
        ] {
            let mut c = TuiCopy::new(old, 0, 0, 100);
            c.select(SelectionType::Lines);
            c.cursor.row = 1;
            let text = c.text();
            assert!(!c.observe(new));
            assert!(c.stopped.is_some());
            assert_eq!(c.text(), text);
        }
    }
    #[test]
    fn unchanged_frame_is_safe_and_boundary_changes_stop() {
        let f = frame(&["one", "two", "three"]);
        let mut c = TuiCopy::new(f.clone(), 0, 0, 100);
        assert!(c.observe(f.clone()));
        assert_eq!(c.offset, 0);
        let mut changed = f;
        changed.alternate = false;
        assert!(!c.observe(changed));
        assert!(c.stopped.is_some());
    }
    #[test]
    fn history_limit_stops_before_selected_rows_are_lost() {
        let mut c = TuiCopy::new(frame(&["one", "two", "three", ":"]), 0, 0, 3);
        c.select(SelectionType::Lines);
        assert!(!c.observe(frame(&["two", "three", "four", ":"])));
        assert!(c.stopped.is_some());
        assert_eq!(c.text().as_deref(), Some("one"));
    }
    #[test]
    fn wide_cells_wrapping_and_block_selection() {
        let f = Frame {
            rows: vec![
                Row {
                    cells: vec!["日".into(), "".into(), "本".into(), "".into()],
                    wrapped: true,
                },
                Row {
                    cells: vec!["語".into(), "".into(), "!".into(), " ".into()],
                    wrapped: false,
                },
            ],
            alternate: true,
        };
        let mut c = TuiCopy::new(f, 0, 0, 100);
        c.select(SelectionType::Simple);
        c.cursor = Point { row: 1, col: 2 };
        assert_eq!(c.text().as_deref(), Some("日本語!"));
        c.select(SelectionType::Block);
        c.cursor = Point { row: 0, col: 0 };
        assert_eq!(c.text().as_deref(), Some("日本\n語!"));
    }
}
