//! セッションレビュー — Stop hook（AI の応答完了）を受けたときに出す、
//! 変更点の要約ポップアップ。
//!
//! 新しい描画スタックは作らない（workbench-v2.md 判断1）。ランチャーの
//! エージェント選択ポップアップと同じ手法（透過の空キャンバスに、罫線ボックスを
//! セル単位で stamp する）を流用する。テスト結果のパースはここではやらない
//! （ターミナル出力のスクレイピングになるため。判断3のPTYスクレイピング禁止）。

use unicode_width::UnicodeWidthChar;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::file_style::{ACCENT, DIM};
use crate::launcher::{border_cell, border_row, column_cells, stamp};
use crate::terminal::{Color, Line};
use crate::timeline::SessionFileSummary;
use crate::view::{TerminalView, Viewport};
use crate::workspace::DiffStat;
use crate::Display;

/// 表示するファイル別の要確認リスト最大数。それ以上は「…ほかN件」に畳む。
const MAX_RISK_ROWS: usize = 8;

pub struct SessionSummary {
    pub files: SessionFileSummary,
    pub diff: Option<DiffStat>,
}

pub enum SessionReviewOutcome {
    None,
    Dismissed,
}

pub struct SessionReview {
    view: TerminalView,
    summary: SessionSummary,
}

impl SessionReview {
    pub fn new(display: Display, viewport: Viewport, summary: SessionSummary) -> Self {
        let mut review = Self {
            view: TerminalView::with_viewport(
                display,
                viewport,
                crate::TOYTERM_CONFIG.font_size,
                None,
            ),
            summary,
        };
        review.rebuild();
        review
    }

    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.view.set_viewport(viewport);
        self.rebuild();
    }

    pub fn change_font_size(&mut self, diff: i32) {
        self.view.increase_font_size(diff);
        self.rebuild();
    }

    pub fn draw(&mut self, surface: &mut glium::Frame) {
        self.view.draw(surface);
    }

    pub fn needs_redraw(&self) -> bool {
        self.view.needs_redraw()
    }

    /// Enter/Esc のどちらでも閉じる（読んだら戻るだけの表示専用ポップアップなので、
    /// 「実行」と「取消」を分ける必要がない）。
    pub fn handle_key(&mut self, event: &KeyEvent) -> SessionReviewOutcome {
        if event.state != ElementState::Pressed {
            return SessionReviewOutcome::None;
        }
        match event.physical_key {
            PhysicalKey::Code(KeyCode::Escape | KeyCode::Enter) => SessionReviewOutcome::Dismissed,
            _ => SessionReviewOutcome::None,
        }
    }

    fn rebuild(&mut self) {
        let cols = (self.view.viewport().w / self.view.cell_size().w).max(1) as usize;
        let rows = (self.view.viewport().h / self.view.cell_size().h).max(1) as usize;
        let lines = render(&self.summary, cols, rows);
        self.view.update_contents(|view| {
            // ターミナルと同じ透過（背景キャンバスは空セルのみ、ボックスは不透明）。
            view.bg_color = Color::Background;
            view.skip_default_bg = false;
            view.lines = lines;
            view.images = Vec::new();
            view.cursor = None;
            view.selection_range = None;
            view.scroll_bar = None;
            view.view_focused = true;
        });
    }
}

fn render(summary: &SessionSummary, cols: usize, rows: usize) -> Vec<Line> {
    let content = review_lines(summary);

    let mut lines: Vec<Line> = (0..rows)
        .map(|_| Line::from_cells(column_cells("", Color::White, Color::Background, cols), false))
        .collect();

    let inner_w = content
        .iter()
        .map(|(text, _)| display_width(text))
        .max()
        .unwrap_or(10)
        .clamp(10, cols.saturating_sub(6).max(10));
    let interior_w = inner_w + 2; // 左右パディング1ずつ
    let box_w = (interior_w + 2).min(cols); // 左右のボーダー
    let box_h = (content.len() + 2).min(rows);
    let x0 = cols.saturating_sub(box_w) / 2;
    let y0 = rows.saturating_sub(box_h) / 2;
    let dash = interior_w.min(box_w.saturating_sub(2));

    stamp(&mut lines, y0, x0, border_row('╭', '╮', dash));
    for (i, (text, fg)) in content.iter().enumerate() {
        let y = y0 + 1 + i;
        if y >= y0 + box_h - 1 {
            break;
        }
        let interior = column_cells(
            &format!(" {text}"),
            *fg,
            crate::view::panel_bg_color(),
            interior_w,
        );
        let mut row = Vec::with_capacity(box_w);
        row.push(border_cell('│'));
        row.extend(interior);
        row.push(border_cell('│'));
        stamp(&mut lines, y, x0, row);
    }
    stamp(&mut lines, y0 + box_h - 1, x0, border_row('╰', '╯', dash));

    lines
}

/// ポップアップの中身（テキスト・文字色）を組み立てる。純粋関数（幅計算・stamp は
/// 呼び出し側）にしてあるので、内容の正しさはユニットテストで固定できる。
fn review_lines(summary: &SessionSummary) -> Vec<(String, Color)> {
    let mut content: Vec<(String, Color)> = Vec::new();
    content.push(("セッションレビュー".to_owned(), ACCENT));
    content.push((String::new(), Color::White));
    content.push((
        format!("変更ファイル  {}件", summary.files.changed_files),
        Color::BrightWhite,
    ));
    if let Some(diff) = &summary.diff {
        content.push((
            format!("+{} / -{} 行", diff.added, diff.removed),
            Color::White,
        ));
    }

    if !summary.files.risks.is_empty() {
        content.push((String::new(), Color::White));
        content.push(("要確認".to_owned(), Color::BrightYellow));
        for (path, risk) in summary.files.risks.iter().take(MAX_RISK_ROWS) {
            content.push((format!(" ▲{risk}  {}", path.display()), Color::White));
        }
        let rest = summary.files.risks.len().saturating_sub(MAX_RISK_ROWS);
        if rest > 0 {
            content.push((format!(" … ほか{rest}件"), DIM));
        }
    }

    content.push((String::new(), Color::White));
    content.push(("Enter / Esc: ターミナルへ戻る".to_owned(), DIM));
    content
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn review_lines_includes_file_count_and_diff_stat() {
        let summary = SessionSummary {
            files: SessionFileSummary {
                changed_files: 3,
                risks: Vec::new(),
            },
            diff: Some(DiffStat {
                added: 42,
                removed: 8,
            }),
        };
        let lines = review_lines(&summary);
        let texts: Vec<&str> = lines.iter().map(|(t, _)| t.as_str()).collect();
        assert!(texts.contains(&"変更ファイル  3件"));
        assert!(texts.contains(&"+42 / -8 行"));
        // リスクが無ければ「要確認」見出しは出ない。
        assert!(!texts.contains(&"要確認"));
    }

    #[test]
    fn review_lines_omits_diff_stat_when_git_unavailable() {
        let summary = SessionSummary {
            files: SessionFileSummary {
                changed_files: 1,
                risks: Vec::new(),
            },
            diff: None,
        };
        let lines = review_lines(&summary);
        // "+N / -M 行" の diff 統計行が出ない（"Enter / Esc" のヒント行は残る）。
        assert!(lines.iter().all(|(t, _)| !t.contains('行')));
    }

    #[test]
    fn review_lines_lists_risks_and_truncates_after_max() {
        let risks: Vec<_> = (0..10)
            .map(|i| (PathBuf::from(format!("f{i}.rs")), "設定"))
            .collect();
        let summary = SessionSummary {
            files: SessionFileSummary {
                changed_files: 10,
                risks,
            },
            diff: None,
        };
        let lines = review_lines(&summary);
        let texts: Vec<&str> = lines.iter().map(|(t, _)| t.as_str()).collect();
        assert!(texts.contains(&"要確認"));
        assert!(texts.iter().any(|t| t.contains("f0.rs")));
        assert!(texts.iter().any(|t| t.contains("ほか2件")));
    }
}
