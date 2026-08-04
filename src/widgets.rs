//! The one widget `term.rs` does not have: a list you can walk through.
//!
//! Both panes are this. Rows are pre-built segments of text and style, so the
//! caller decides what a row looks like and the widget only has to place it,
//! keep the cursor in view, and mark what is ticked.

use crate::term::{Block, CONTINUATION, Color, Frame, Modifier, Rect, Style, Widget};
use crate::width;

/// A row is a sequence of coloured pieces laid side by side. The caller has
/// already sized them; anything past the edge is cut.
pub struct Row {
    pub segments: Vec<(String, Style)>,
    /// Ticked with the space bar. Drawn as the box in front of the row.
    pub selected: bool,
    /// Rows that can be ticked show a box; repository headers do not.
    pub selectable: bool,
}

impl Row {
    pub fn new(segments: Vec<(String, Style)>) -> Row {
        Row {
            segments,
            selected: false,
            selectable: false,
        }
    }

    pub fn tickable(mut self, selected: bool) -> Row {
        self.selectable = true;
        self.selected = selected;
        self
    }
}

pub struct List {
    rows: Vec<Row>,
    cursor: usize,
    offset: usize,
    block: Option<Block>,
    /// The pane holding the keyboard gets a reversed cursor; the other a bold
    /// one, so you always know where a key would land.
    focused: bool,
}

impl List {
    pub fn new(rows: Vec<Row>) -> List {
        List {
            rows,
            cursor: 0,
            offset: 0,
            block: None,
            focused: false,
        }
    }

    pub fn block(mut self, block: Block) -> List {
        self.block = Some(block);
        self
    }

    pub fn cursor(mut self, cursor: usize) -> List {
        self.cursor = cursor;
        self
    }

    pub fn offset(mut self, offset: usize) -> List {
        self.offset = offset;
        self
    }

    pub fn focused(mut self, focused: bool) -> List {
        self.focused = focused;
        self
    }
}

/// Where the window onto the rows should start, given where the cursor is.
///
/// Kept outside the widget because the caller needs it too: it decides what a
/// page up does, and it has to survive between frames.
pub fn scroll_to(offset: usize, cursor: usize, height: usize, len: usize) -> usize {
    if height == 0 || len == 0 {
        return 0;
    }
    let mut offset = offset.min(len.saturating_sub(1));
    if cursor < offset {
        offset = cursor;
    }
    if cursor >= offset + height {
        offset = cursor + 1 - height;
    }
    // Never leave a gap at the bottom while rows are still hidden above.
    offset.min(len.saturating_sub(height.min(len)))
}

/// Draws text into one row, honouring how wide each character really is.
///
/// Wide characters claim the cell they spill into, combining marks hang on the
/// character before them, and everything stops at the pane's edge.
fn put(
    frame: &mut Frame,
    y: u16,
    x: &mut u16,
    used: &mut usize,
    limit: usize,
    text: &str,
    style: Style,
) {
    for c in text.chars() {
        let w = width::char_width(c);
        if w == 0 {
            // A mark with nothing to attach to would be invisible anyway.
            if *x > 0 {
                frame.add_combining(*x - 1, y, c);
            }
            continue;
        }
        if *used + w > limit {
            break;
        }
        frame.set(*x, y, c, style);
        *x += 1;
        *used += 1;
        if w == 2 {
            frame.set(*x, y, CONTINUATION, style);
            *x += 1;
            *used += 1;
        }
    }
}

impl Widget for List {
    fn render(self, area: Rect, frame: &mut Frame) {
        let inner = match &self.block {
            Some(block) => block.render_frame(area, frame),
            None => area,
        };
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let limit = inner.width as usize;
        let height = inner.height as usize;
        let last = self.rows.len().min(self.offset + height);

        for (line, index) in (self.offset..last).enumerate() {
            let row = &self.rows[index];
            let y = inner.y + line as u16;
            let cursor_here = index == self.cursor;
            let mark = |style: Style| match (cursor_here, self.focused) {
                (true, true) => style.add_modifier(Modifier::REVERSED),
                (true, false) => style.add_modifier(Modifier::BOLD),
                _ => style,
            };

            let mut x = inner.x;
            let mut used = 0;

            if row.selectable {
                let box_style = if row.selected {
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(Color::DarkGray)
                };
                let glyph = if row.selected { "▣ " } else { "▢ " };
                put(frame, y, &mut x, &mut used, limit, glyph, mark(box_style));
            } else {
                put(frame, y, &mut x, &mut used, limit, "  ", mark(Style::new()));
            }

            for (text, style) in &row.segments {
                put(frame, y, &mut x, &mut used, limit, text, mark(*style));
            }

            // Paint the rest so a reversed cursor spans the whole pane.
            if cursor_here && self.focused {
                while used < limit {
                    put(frame, y, &mut x, &mut used, limit, " ", mark(Style::new()));
                }
            }
        }
    }
}
