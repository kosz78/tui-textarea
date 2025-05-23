use crate::ratatui::buffer::Buffer;
use crate::ratatui::layout::Rect;
use crate::ratatui::text::{Span, Text};
use crate::ratatui::widgets::{Paragraph, Widget};
use crate::textarea::TextArea;
use crate::util::{line_rows, num_digits};
#[cfg(feature = "ratatui")]
use ratatui::text::Line;
use ratatui::widgets::Wrap;
use std::cmp;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "tuirs")]
use tui::text::Spans as Line;

// &mut 'a (u16, u16, u16, u16) is not available since `render` method takes immutable reference of TextArea
// instance. In the case, the TextArea instance cannot be accessed from any other objects since it is mutablly
// borrowed.
//
// `ratatui::Frame::render_stateful_widget` would be an assumed way to render a stateful widget. But at this
// point we stick with using `ratatui::Frame::render_widget` because it is simpler API. Users don't need to
// manage states of textarea instances separately.
// https://docs.rs/ratatui/latest/ratatui/terminal/struct.Frame.html#method.render_stateful_widget
#[derive(Default, Debug)]
pub struct Viewport(AtomicU64);

impl Clone for Viewport {
    fn clone(&self) -> Self {
        let u = self.0.load(Ordering::Relaxed);
        Viewport(AtomicU64::new(u))
    }
}

impl Viewport {
    pub fn scroll_top(&self) -> (u16, u16) {
        let u = self.0.load(Ordering::Relaxed);
        ((u >> 16) as u16, u as u16)
    }

    pub fn rect(&self) -> (u16, u16, u16, u16) {
        let u = self.0.load(Ordering::Relaxed);
        let width = (u >> 48) as u16;
        let height = (u >> 32) as u16;
        let row = (u >> 16) as u16;
        let col = u as u16;
        (row, col, width, height)
    }

    pub fn position(&self) -> (u16, u16, u16, u16) {
        let (row_top, col_top, width, height) = self.rect();
        let row_bottom = row_top.saturating_add(height).saturating_sub(1);
        let col_bottom = col_top.saturating_add(width).saturating_sub(1);

        (
            row_top,
            col_top,
            cmp::max(row_top, row_bottom),
            cmp::max(col_top, col_bottom),
        )
    }

    fn store(&self, row: u16, col: u16, width: u16, height: u16) {
        // Pack four u16 values into one u64 value
        let u =
            ((width as u64) << 48) | ((height as u64) << 32) | ((row as u64) << 16) | col as u64;
        self.0.store(u, Ordering::Relaxed);
    }

    pub fn scroll(&mut self, rows: i16, cols: i16) {
        fn apply_scroll(pos: u16, delta: i16) -> u16 {
            if delta >= 0 {
                pos.saturating_add(delta as u16)
            } else {
                pos.saturating_sub(-delta as u16)
            }
        }

        let u = self.0.get_mut();
        let row = apply_scroll((*u >> 16) as u16, rows);
        let col = apply_scroll(*u as u16, cols);
        *u = (*u & 0xffff_ffff_0000_0000) | ((row as u64) << 16) | (col as u64);
    }
}

#[inline]
fn next_scroll_top(prev_top: u16, cursor: u16, len: u16) -> u16 {
    if cursor < prev_top {
        cursor
    } else if prev_top + len <= cursor {
        cursor + 1 - len
    } else {
        prev_top
    }
}

impl<'a> TextArea<'a> {
    fn text_widget(&'a self, top_row: usize, height: usize) -> Text<'a> {
        let lines_len = self.lines().len();
        let lnum_len = num_digits(lines_len);
        let bottom_row = cmp::min(top_row + height, lines_len);
        let mut lines = Vec::with_capacity(bottom_row - top_row);
        for (i, line) in self.lines()[top_row..bottom_row].iter().enumerate() {
            lines.push(self.line_spans(line.as_str(), top_row + i, lnum_len));
        }
        Text::from(lines)
    }

    fn placeholder_widget(&'a self) -> Text<'a> {
        let cursor = Span::styled(" ", self.cursor_style);
        let text = Span::raw(self.placeholder.as_str());
        Text::from(Line::from(vec![cursor, text]))
    }

    fn scroll_top_row(&self, prev_top: u16, height: u16) -> u16 {
        next_scroll_top(prev_top, self.cursor().0 as u16, height)
    }

    fn scroll_top_col(&self, prev_top: u16, width: u16) -> u16 {
        let mut cursor = self.cursor().1 as u16;
        // Adjust the cursor position due to the width of line number.
        if self.line_number_style().is_some() {
            let lnum = num_digits(self.lines().len()) as u16 + 2; // `+ 2` for margins
            if cursor <= lnum {
                cursor *= 2; // Smoothly slide the line number into the screen on scrolling left
            } else {
                cursor += lnum; // The cursor position is shifted by the line number part
            };
        }
        next_scroll_top(prev_top, cursor, width)
    }
}

impl Widget for &TextArea<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Rect { width, height, .. } = if let Some(b) = self.block() {
            b.inner(area)
        } else {
            area
        };

        let (top_row, top_col) = self.viewport.scroll_top();
        let mut top_row = self.scroll_top_row(top_row, height);
        let mut top_col = self.scroll_top_col(top_col, width);

        let cursor = self.cursor();
        let wrap = self.get_wrap();
        if wrap {
            let wrapped_rows =
                wrapped_rows(&self.lines(), width, self.line_number_style().is_some());
            top_row = next_scroll_row_wrapped(top_row, cursor.0 as u16, height, &wrapped_rows);
            // Column for scoll should never change with wrapping (no horiz scroll)
            // FIXME: Edge case where line can't fit in screen and overflows?
        } else {
            top_row = next_scroll_top(top_row, cursor.0 as u16, height);
            top_col = next_scroll_top(top_col, cursor.1 as u16, width);
        }
        let (top_row, top_col) = (top_row, top_col);

        // Transform lines into array of row count for each line
        fn wrapped_rows(lines: &[String], wrap_width: u16, has_lnum: bool) -> Vec<u16> {
            let num_lines = lines.len();
            lines
                .iter()
                .map(|line| line_rows(&line, wrap_width, has_lnum, num_lines))
                .collect()
        }

        let (text, style) = if !self.placeholder.is_empty() && self.is_empty() {
            (self.placeholder_widget(), self.placeholder_style)
        } else {
            (self.text_widget(top_row as _, height as _), self.style())
        };
        fn next_scroll_row_wrapped(
            prev_top_row: u16,
            cursor_row: u16,
            viewport_height: u16,
            wrapped_rows: &Vec<u16>,
        ) -> u16 {
            if cursor_row < prev_top_row {
                return cursor_row;
            } else {
                // Calculate the number of wrap rows between the top row and the cursor row
                // TODO: Clarify why +1 is needed
                let rows_from_top_to_cursor = wrapped_rows
                    [prev_top_row as usize..cursor_row as usize]
                    .iter()
                    .sum::<u16>()
                    + 1;
                let cursor_row_wraps = wrapped_rows[cursor_row as usize] - 1;
                let cursor_line_on_screen =
                    rows_from_top_to_cursor + cursor_row_wraps <= viewport_height;
                let rows_to_move =
                    (rows_from_top_to_cursor + cursor_row_wraps).saturating_sub(viewport_height);

                if !cursor_line_on_screen {
                    // Count how many lines add up to enough rows to get entire cursor line on screen again
                    let lines_to_move = wrapped_rows[prev_top_row as usize..cursor_row as usize]
                        .iter()
                        .scan(0, |acc, &row| {
                            // Sum wrap rows to this line
                            *acc += row;
                            Some(*acc)
                        })
                        // Return index of line where acc exceeds rows_to_move
                        .position(|sum| sum >= rows_to_move)
                        .unwrap_or(0) as u16;
                    let lines_to_move = lines_to_move + 1; // Convert from index

                    // Never move below cursor row in case terminal can't fit it
                    return (prev_top_row + lines_to_move).min(cursor_row);
                } else {
                    return prev_top_row;
                }
            };
        }

        // To get fine control over the text color and the surrrounding block they have to be rendered separately
        // see https://github.com/ratatui/ratatui/issues/144
        let mut text_area = area;
        let mut inner = Paragraph::new(text)
            .style(style)
            .alignment(self.alignment());
        if wrap {
            inner = inner.wrap(Wrap { trim: false });
        }
        if let Some(b) = self.block() {
            text_area = b.inner(area);
            // ratatui does not need `clone()` call because `Block` implements `WidgetRef` and `&T` implements `Widget`
            // where `T: WidgetRef`. So `b.render` internally calls `b.render_ref` and it doesn't move out `self`.
            #[cfg(feature = "tuirs")]
            let b = b.clone();
            b.render(area, buf)
        }
        if top_col != 0 {
            inner = inner.scroll((0, top_col));
        }
        // TODO: Vertical scroll to position top edge in middle of wrapped line

        // Store scroll top position for rendering on the next tick
        self.viewport.store(top_row, top_col, width, height);

        inner.render(text_area, buf);
    }
}
