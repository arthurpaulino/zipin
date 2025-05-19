use crate::doc::Segment;
use unicode_width::UnicodeWidthStr;

/// One paragraph laid out into visual rows. Each row carries pieces that
/// reference back to the source segments.
#[derive(Default, Clone)]
pub struct ParagraphLayout {
    pub rows: Vec<RowLayout>,
}

#[derive(Default, Clone)]
pub struct RowLayout {
    pub pieces: Vec<RowPiece>,
}

#[derive(Default, Clone)]
pub struct RowPiece {
    pub seg_idx: usize,
    pub text_start: usize,
    pub text_end: usize,
    pub col_start: usize,
    pub col_width: usize,
}

fn char_width(ch: char) -> usize {
    let mut buf = [0u8; 4];
    UnicodeWidthStr::width(ch.encode_utf8(&mut buf))
}

/// Greedy display-column wrap. Segments wider than `width` are split by
/// chars so they continue on the next row.
pub fn layout_paragraph(paragraph: &[Segment], width: usize) -> ParagraphLayout {
    let width = width.max(1);
    let mut rows: Vec<RowLayout> = vec![RowLayout::default()];
    let mut col = 0usize;

    for (seg_idx, seg) in paragraph.iter().enumerate() {
        let text = seg.text();
        let seg_w = seg.width();

        if seg_w <= width.saturating_sub(col) {
            rows.last_mut().unwrap().pieces.push(RowPiece {
                seg_idx,
                text_start: 0,
                text_end: text.len(),
                col_start: col,
                col_width: seg_w,
            });
            col += seg_w;
            continue;
        }

        // Split the segment across rows.
        let mut byte_cursor = 0usize;

        while byte_cursor < text.len() {
            let avail = width.saturating_sub(col);
            if avail == 0 {
                rows.push(RowLayout::default());
                col = 0;
                continue;
            }
            let piece_text_start = byte_cursor;
            let mut piece_w = 0usize;

            for ch in text[byte_cursor..].chars() {
                let cw = char_width(ch);
                if piece_w + cw > avail {
                    break;
                }
                piece_w += cw;
                byte_cursor += ch.len_utf8();
            }

            if piece_w == 0 {
                // Single char wider than the row — force-place to make
                // progress; clips visually.
                if let Some(ch) = text[byte_cursor..].chars().next() {
                    piece_w = char_width(ch);
                    byte_cursor += ch.len_utf8();
                } else {
                    break;
                }
            }

            let piece_text_end = byte_cursor;
            rows.last_mut().unwrap().pieces.push(RowPiece {
                seg_idx,
                text_start: piece_text_start,
                text_end: piece_text_end,
                col_start: col,
                col_width: piece_w,
            });
            col += piece_w;

            if byte_cursor < text.len() {
                rows.push(RowLayout::default());
                col = 0;
            }
        }
    }

    ParagraphLayout { rows }
}

/// Returns `(row_in_paragraph, col_on_row)` for the cursor pointing at
/// `target_seg`. If the cursor sits past the last segment, returns the row
/// and column just past the final piece.
pub fn cursor_position(pl: &ParagraphLayout, target_seg: usize) -> (usize, usize) {
    if pl.rows.is_empty() {
        return (0, 0);
    }
    for (row_idx, row) in pl.rows.iter().enumerate() {
        for piece in &row.pieces {
            if piece.seg_idx == target_seg && piece.text_start == 0 {
                return (row_idx, piece.col_start);
            }
        }
    }
    let last_row_idx = pl.rows.len() - 1;
    let last_row = &pl.rows[last_row_idx];
    let col = last_row
        .pieces
        .last()
        .map(|p| p.col_start + p.col_width)
        .unwrap_or(0);
    (last_row_idx, col)
}
