use crate::doc::{Doc, Segment};
use crate::engine::{Engine, Page};
use crate::layout::{cursor_position, layout_paragraph, ParagraphLayout};
use ratatui::{
    layout::{
        Alignment,
        Constraint::{Fill, Length},
        Layout, Rect,
    },
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthStr;

/// Information the key-event loop needs after each render: what's visible,
/// where the cursor sits visually, and the paragraph layouts the renderer
/// already computed (so vertical cursor moves can re-use them instead of
/// re-running `layout_paragraph` on every keypress).
#[derive(Default, Clone)]
pub struct ViewInfo {
    pub total_rows: usize,
    /// Visual row index of the cursor across the whole document.
    pub cursor_row: usize,
    /// Display column on `cursor_row` where the cursor sits.
    pub cursor_col: usize,
    /// One layout per paragraph, in document order.
    pub layouts: Vec<ParagraphLayout>,
}

const POPUP_INDEX_KEYS: [char; 10] = ['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'];

pub fn render(frame: &mut Frame, doc: &Doc, engine: &Engine, info: &mut ViewInfo) {
    let area = frame.area();
    let [doc_rect, scroll_rect] = Layout::horizontal([Fill(1), Length(1)]).areas::<2>(area);

    let chars_per_row = doc_rect.width as usize;
    let rows_per_screen = doc_rect.height as usize;

    let mut visual_lines: Vec<Line> = Vec::new();
    let mut paragraph_offsets: Vec<usize> = Vec::with_capacity(doc.paragraphs.len());
    let mut layouts: Vec<ParagraphLayout> = Vec::with_capacity(doc.paragraphs.len());

    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;

    for (p_idx, paragraph) in doc.paragraphs.iter().enumerate() {
        paragraph_offsets.push(visual_lines.len());
        let pl = layout_paragraph(paragraph, chars_per_row);
        let pstyle = if p_idx % 2 == 0 {
            Style::default()
        } else {
            Style::default().bg(Color::DarkGray)
        };

        if pl.rows.is_empty() {
            visual_lines.push(Line::from("").style(pstyle));
        }

        for row in &pl.rows {
            let mut spans: Vec<Span> = Vec::new();
            for piece in &row.pieces {
                let seg = &paragraph[piece.seg_idx];
                let slice = &seg.text()[piece.text_start..piece.text_end];
                let style = match seg {
                    Segment::Composing(_) => {
                        pstyle.fg(Color::Yellow).add_modifier(Modifier::UNDERLINED)
                    }
                    Segment::Committed(_) => pstyle,
                };
                spans.push(Span::styled(slice.to_string(), style));
            }
            visual_lines.push(Line::from(spans).style(pstyle));
        }

        if doc.cursor.0 == p_idx {
            let target = doc.cursor.1;
            let (row_in_para, col_on_row) = cursor_position(&pl, target);
            cursor_row = paragraph_offsets[p_idx] + row_in_para;
            cursor_col = col_on_row;
        }

        layouts.push(pl);
    }

    let total_rows = visual_lines.len();

    let mut scroll = doc.scroll_offset;
    if cursor_row < scroll {
        scroll = cursor_row;
    } else if cursor_row >= scroll + rows_per_screen.max(1) {
        scroll = cursor_row + 1 - rows_per_screen.max(1);
    }

    let drawn = visual_lines
        .iter()
        .skip(scroll)
        .take(rows_per_screen)
        .cloned()
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(drawn)), doc_rect);

    if rows_per_screen > 0 && total_rows > rows_per_screen {
        render_scrollbar(frame, scroll_rect, total_rows, rows_per_screen, scroll);
    }

    let visible_cursor_row = cursor_row.saturating_sub(scroll);
    if visible_cursor_row < rows_per_screen && cursor_col <= chars_per_row {
        frame.set_cursor_position((
            doc_rect.x + cursor_col as u16,
            doc_rect.y + visible_cursor_row as u16,
        ));
    }

    // Popup, if composing.
    if let Some((p_idx, s_idx)) = doc.composing {
        if let Some(page) = engine.current_page() {
            if let Some(pl) = layouts.get(p_idx) {
                let (row_in_para, col_on_row) = cursor_position(pl, s_idx);
                let anchor_row = paragraph_offsets[p_idx] + row_in_para;
                let anchor_visible = anchor_row.saturating_sub(scroll);
                if anchor_visible < rows_per_screen {
                    render_popup(
                        frame,
                        doc_rect,
                        anchor_visible as u16,
                        col_on_row as u16,
                        rows_per_screen,
                        &page,
                    );
                }
            }
        }
    }

    *info = ViewInfo {
        total_rows,
        cursor_row,
        cursor_col,
        layouts,
    };
}

fn render_scrollbar(frame: &mut Frame, area: Rect, total: usize, height: usize, scroll: usize) {
    let h = height;
    let thumb_h = ((h * h).div_ceil(total + h)).max(1);
    let max_off = total.saturating_sub(1).max(1);
    let thumb_off = (h.saturating_sub(thumb_h)) * scroll / max_off;
    let mut bar: Vec<Line> = Vec::with_capacity(h);
    for y in 0..h {
        bar.push(if y >= thumb_off && y < thumb_off + thumb_h {
            Line::from("▒")
        } else {
            Line::from("░")
        });
    }
    frame.render_widget(Paragraph::new(Text::from(bar)), area);
}

/// Render the candidate popup. Anchored to `anchor_col` of `anchor_row`
/// (already in screen coordinates relative to `doc_rect`). Drops down or
/// flips up depending on the row's position in the screen. In flipped mode
/// the candidate list renders bottom-up so index 1 sits closest to the
/// cursor.
fn render_popup(
    frame: &mut Frame,
    doc_rect: Rect,
    anchor_row: u16,
    anchor_col: u16,
    rows_per_screen: usize,
    page: &Page<'_>,
) {
    if page.candidates.is_empty() {
        return;
    }

    let max_w = page
        .candidates
        .iter()
        .map(|c| 2 + UnicodeWidthStr::width(c.text.as_str())) // "1 " prefix
        .max()
        .unwrap_or(4);
    // Border + 1-cell padding.
    let popup_w = (max_w as u16 + 2).min(doc_rect.width);

    let n = page.candidates.len() as u16;
    // 1 row for page indicator + 1 row each for candidates + 2 for borders.
    let popup_h = (n + 3).min(doc_rect.height);

    let reverse = (anchor_row as usize) >= rows_per_screen / 2;

    let popup_rect = if reverse {
        // Flip up: bottom border ends one row above the anchor.
        let bottom = doc_rect.y + anchor_row;
        let top = bottom.saturating_sub(popup_h);
        Rect {
            x: doc_rect.x + anchor_col.min(doc_rect.width.saturating_sub(popup_w)),
            y: top,
            width: popup_w,
            height: popup_h,
        }
    } else {
        // Drop down: top border just below the anchor row.
        let top = doc_rect.y + anchor_row + 1;
        let max_h = doc_rect.y + doc_rect.height - top;
        Rect {
            x: doc_rect.x + anchor_col.min(doc_rect.width.saturating_sub(popup_w)),
            y: top,
            width: popup_w,
            height: popup_h.min(max_h),
        }
    };

    frame.render_widget(Clear, popup_rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(popup_rect);
    frame.render_widget(block, popup_rect);

    let total_inner_rows = inner.height as usize;
    if total_inner_rows == 0 {
        return;
    }

    let max_visible = total_inner_rows.saturating_sub(1); // 1 row reserved for indicator
    let visible_count = (page.candidates.len()).min(max_visible);
    let indicator = format_indicator(page);

    let mut cand_lines: Vec<Line> = Vec::with_capacity(visible_count);
    if reverse {
        for i in (0..visible_count).rev() {
            cand_lines.push(candidate_line(page, i));
        }
    } else {
        for i in 0..visible_count {
            cand_lines.push(candidate_line(page, i));
        }
    }

    // Carve off one row for the indicator (top in reverse mode, bottom
    // otherwise) so it can be a separate Paragraph with right alignment.
    // Setting alignment on a Line inside a shared Paragraph is unreliable
    // when other lines have implicit Left alignment; rendering in its own
    // widget avoids the quirk.
    // Reserve 1 col on the right of the indicator rect so the right arrow
    // doesn't sit flush against the popup border.
    let ind_w = inner.width.saturating_sub(1);
    let (cand_rect, indicator_rect) = if reverse {
        (
            Rect {
                x: inner.x,
                y: inner.y + 1,
                width: inner.width,
                height: inner.height.saturating_sub(1),
            },
            Rect {
                x: inner.x,
                y: inner.y,
                width: ind_w,
                height: 1,
            },
        )
    } else {
        let last_y = inner.y + inner.height.saturating_sub(1);
        (
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: inner.height.saturating_sub(1),
            },
            Rect {
                x: inner.x,
                y: last_y,
                width: ind_w,
                height: 1,
            },
        )
    };

    frame.render_widget(Paragraph::new(Text::from(cand_lines)), cand_rect);
    frame.render_widget(
        Paragraph::new(indicator)
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Right),
        indicator_rect,
    );
}

fn candidate_line(page: &Page<'_>, idx_in_page: usize) -> Line<'static> {
    let key = POPUP_INDEX_KEYS.get(idx_in_page).copied().unwrap_or('?');
    let cand = &page.candidates[idx_in_page];
    let style = if idx_in_page == page.highlighted {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(format!("{key} "), Style::default().fg(Color::Gray)),
        Span::styled(cand.text.clone(), style),
    ])
}

fn format_indicator(page: &Page<'_>) -> String {
    match (page.has_prev, page.has_next) {
        (false, false) => String::new(),
        (false, true) => "→".into(),
        (true, false) => "←".into(),
        (true, true) => "← →".into(),
    }
}

/// Map a popup index key (`'1'..'9'`, `'0'`) to the candidate index within
/// the current page. Returns `None` if the key isn't a popup index.
pub fn popup_index_for_key(key: char) -> Option<usize> {
    POPUP_INDEX_KEYS.iter().position(|&k| k == key)
}
