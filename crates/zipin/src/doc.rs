use unicode_width::UnicodeWidthStr;

/// Atomic unit of text in the document.
pub enum Segment {
    /// Committed text — a hanzi, a phrase, an ASCII run, full-width blank, or
    /// punctuation. Atomic for cursor navigation.
    Committed(String),
    /// Active composition — raw keystrokes the user has typed and not yet
    /// committed.
    Composing(String),
}

impl Segment {
    pub fn text(&self) -> &str {
        match self {
            Self::Committed(s) | Self::Composing(s) => s.as_str(),
        }
    }

    pub fn width(&self) -> usize {
        UnicodeWidthStr::width(self.text())
    }
}

/// Document model: paragraphs separated by Enter, each a sequence of
/// segments. At most one composing segment exists at a time.
pub struct Doc {
    pub paragraphs: Vec<Vec<Segment>>,
    /// `(paragraph, segment-index-within-paragraph)`. The index points at the
    /// segment the cursor is *before*; new input lands here.
    pub cursor: (usize, usize),
    pub composing: Option<(usize, usize)>,
    pub scroll_offset: usize,
}

impl Doc {
    pub fn new() -> Self {
        Self {
            paragraphs: vec![Vec::new()],
            cursor: (0, 0),
            composing: None,
            scroll_offset: 0,
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    pub fn open_or_extend_composition(&mut self, c: char) {
        if let Some((p, s)) = self.composing {
            if let Segment::Composing(buf) = &mut self.paragraphs[p][s] {
                buf.push(c);
                return;
            }
        }
        let (p, s) = self.cursor;
        self.paragraphs[p].insert(s, Segment::Composing(c.to_string()));
        self.composing = Some((p, s));
    }

    pub fn shrink_composition(&mut self) -> bool {
        let Some((p, s)) = self.composing else {
            return false;
        };
        let Segment::Composing(buf) = &mut self.paragraphs[p][s] else {
            return false;
        };
        buf.pop();
        if buf.is_empty() {
            self.paragraphs[p].remove(s);
            self.composing = None;
        }
        true
    }

    pub fn cancel_composition(&mut self) {
        if let Some((p, s)) = self.composing.take() {
            self.paragraphs[p].remove(s);
        }
    }

    /// Replace the composing segment with `text` (one segment per char) and
    /// advance the cursor past it. Per-char segmentation keeps arrow-key
    /// navigation aligned with what the user sees: one keypress, one glyph.
    pub fn commit_composition(&mut self, text: String) -> bool {
        let Some((p, s)) = self.composing.take() else {
            return false;
        };
        let mut chars = text.chars();
        let Some(first) = chars.next() else {
            self.paragraphs[p].remove(s);
            self.cursor = (p, s);
            return true;
        };
        self.paragraphs[p][s] = Segment::Committed(first.to_string());
        let mut count = 1;
        for ch in chars {
            self.paragraphs[p].insert(s + count, Segment::Committed(ch.to_string()));
            count += 1;
        }
        self.cursor = (p, s + count);
        true
    }

    pub fn insert_committed(&mut self, text: String) {
        let (p, s) = self.cursor;
        let mut count = 0;
        for (i, ch) in text.chars().enumerate() {
            self.paragraphs[p].insert(s + i, Segment::Committed(ch.to_string()));
            count = i + 1;
        }
        self.cursor.1 += count;
        if let Some((cp, cs)) = self.composing.as_mut() {
            if *cp == p && *cs >= s {
                *cs += count;
            }
        }
    }

    pub fn break_line(&mut self) {
        let (p, s) = self.cursor;
        let tail = self.paragraphs[p].split_off(s);
        self.paragraphs.insert(p + 1, tail);
        self.cursor = (p + 1, 0);
        if let Some((cp, cs)) = self.composing.as_mut() {
            if *cp == p && *cs >= s {
                *cp = p + 1;
                *cs -= s;
            }
        }
    }

    pub fn delete_segment_left(&mut self) {
        let (p, s) = self.cursor;
        if s > 0 {
            self.paragraphs[p].remove(s - 1);
            self.cursor.1 -= 1;
            self.shift_composing_for_delete(p, s - 1);
        } else if p > 0 {
            let mut tail = self.paragraphs.remove(p);
            let prev_len = self.paragraphs[p - 1].len();
            self.paragraphs[p - 1].append(&mut tail);
            self.cursor = (p - 1, prev_len);
            if let Some((cp, cs)) = self.composing.as_mut() {
                if *cp == p {
                    *cp = p - 1;
                    *cs += prev_len;
                } else if *cp > p {
                    *cp -= 1;
                }
            }
        }
    }

    pub fn delete_segment_under(&mut self) {
        let (p, s) = self.cursor;
        if s < self.paragraphs[p].len() {
            self.paragraphs[p].remove(s);
            self.shift_composing_for_delete(p, s);
        } else if p + 1 < self.paragraphs.len() {
            let mut tail = self.paragraphs.remove(p + 1);
            self.paragraphs[p].append(&mut tail);
            if let Some((cp, cs)) = self.composing.as_mut() {
                if *cp == p + 1 {
                    *cp = p;
                    *cs += s;
                } else if *cp > p + 1 {
                    *cp -= 1;
                }
            }
        }
    }

    fn shift_composing_for_delete(&mut self, p: usize, s: usize) {
        if let Some((cp, cs)) = self.composing.as_mut() {
            if *cp == p && *cs >= s {
                if *cs == s {
                    self.composing = None;
                } else {
                    *cs -= 1;
                }
            }
        }
    }

    pub fn move_cursor_left(&mut self) {
        let (p, s) = self.cursor;
        if s > 0 {
            self.cursor.1 -= 1;
        } else if p > 0 {
            self.cursor = (p - 1, self.paragraphs[p - 1].len());
        }
    }

    pub fn move_cursor_right(&mut self) {
        let (p, s) = self.cursor;
        if s < self.paragraphs[p].len() {
            self.cursor.1 += 1;
        } else if p + 1 < self.paragraphs.len() {
            self.cursor = (p + 1, 0);
        }
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor.1 = 0;
    }

    pub fn move_cursor_end(&mut self) {
        let p = self.cursor.0;
        self.cursor.1 = self.paragraphs[p].len();
    }
}
