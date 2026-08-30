//! The line renderer.
//!
//! This is a port of bestline's `bestlineRefreshLineImpl`. The important
//! property is that it never clears the screen before drawing: it overwrites
//! cells and then erases forwards with `ESC [ K` and `ESC [ J`, which is what
//! keeps a redraw from flickering.
//!
//! The renderer walks the line character by character tracking the column
//! itself, rather than dividing the total width by the terminal width. That is
//! what makes a double-width glyph at the right edge wrap correctly and what
//! lets the cursor be placed on the right row of a line that spans several.

use crate::terminal::WinSize;
use crate::unicode::{char_width, display_width, display_width_parts};

/// What to draw.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// Prompt text, which may contain ANSI escape sequences.
    pub prompt: &'a str,
    /// The line being edited.
    pub line: &'a str,
    /// Cursor position as a byte offset into `line`.
    pub pos: usize,
    /// Optional hint drawn after the cursor, already styled.
    pub hint: Option<&'a str>,
    /// Draw every character as `*`.
    pub mask: bool,
    /// Byte offsets of a bracket pair to embolden.
    pub highlight: Option<(usize, usize)>,
    /// Final draw before the line is submitted: suppress hints and highlights
    /// so the terminal is left showing exactly what the caller receives.
    pub finished: bool,
}

impl<'a> Frame<'a> {
    /// Creates a frame with no hint, mask or highlight.
    #[must_use]
    pub fn new(prompt: &'a str, line: &'a str, pos: usize) -> Self {
        Frame {
            prompt,
            line,
            pos,
            hint: None,
            mask: false,
            highlight: None,
            finished: false,
        }
    }
}

/// Tracks where the cursor was left so the next redraw can find its way back.
#[derive(Debug, Clone)]
pub struct Renderer {
    win: WinSize,
    /// Rows the previous frame occupied.
    rows: usize,
    /// Rows between the cursor and the bottom of the previous frame.
    rows_below_cursor: usize,
}

impl Renderer {
    /// Creates a renderer for a terminal of the given size.
    #[must_use]
    pub fn new(win: WinSize) -> Self {
        Renderer {
            win,
            rows: 1,
            rows_below_cursor: 0,
        }
    }

    /// The terminal size the renderer is drawing for.
    #[must_use]
    pub fn win_size(&self) -> WinSize {
        self.win
    }

    /// Updates the terminal size after a resize.
    pub fn set_win_size(&mut self, win: WinSize) {
        self.win = win;
    }

    /// Moves the cursor below the rendered line and starts a fresh one.
    ///
    /// Call this before printing anything underneath the prompt (a completion
    /// list, a submitted line) so the output does not land on top of it.
    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.rows_below_cursor > 0 {
            push_csi(&mut out, self.rows_below_cursor, b'B');
        }
        out.extend_from_slice(b"\r\n");
        self.rows = 1;
        self.rows_below_cursor = 0;
        out
    }

    /// Forgets the previous frame's geometry.
    ///
    /// Used after the screen is cleared, when there is nothing above the cursor
    /// to move back up to.
    pub fn reset(&mut self) {
        self.rows = 1;
        self.rows_below_cursor = 0;
    }

    /// Renders `frame`, returning the bytes to write to the terminal.
    pub fn render(&mut self, frame: &Frame<'_>) -> Vec<u8> {
        let xn = self.win.cols();
        let yn = self.win.rows();
        let pwidth = display_width(frame.prompt);
        let (window, _) = self.visible_window(frame, pwidth, xn, yn);
        let (start, end, pos) = window;

        let mut out = Vec::with_capacity(frame.line.len() + 64);

        // Return to the first row of the previous frame.
        out.push(b'\r');
        if self.rows > self.rows_below_cursor + 1 {
            push_csi(&mut out, self.rows - self.rows_below_cursor - 1, b'A');
        }

        out.extend_from_slice(frame.prompt.as_bytes());

        let highlight = if frame.finished {
            None
        } else {
            frame.highlight
        };

        let mut x = pwidth;
        let mut rows = 1;
        // `None` until the loop reaches the cursor. `cursor_col` and
        // `rows_below` are only meaningful together.
        let mut cursor_col: Option<usize> = None;
        let mut rows_below = 0usize;

        for (offset, c) in frame.line[start..end].char_indices() {
            let i = start + offset;
            let w = if frame.mask { 1 } else { char_width(c) };

            // Wrap before drawing, so a wide glyph is never split by the edge.
            if x > 0 && x + w > xn {
                if cursor_col.is_some() {
                    rows_below += 1;
                }
                if x < xn {
                    out.extend_from_slice(b"\x1b[K");
                }
                out.extend_from_slice(b"\r\n");
                rows += 1;
                x = 0;
            }

            if i == pos {
                cursor_col = Some(x);
                rows_below = 0;
            }

            if frame.mask {
                out.push(b'*');
            } else {
                let flip = highlight.is_some_and(|(a, b)| i == a || i == b);
                if flip {
                    out.extend_from_slice(b"\x1b[1m");
                }
                let mut encoded = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut encoded).as_bytes());
                if flip {
                    out.extend_from_slice(b"\x1b[22m");
                }
            }
            x += w;
        }

        // A hint is only drawn if it fits in what is left of the current row.
        let hint = frame
            .hint
            .filter(|h| !frame.finished && display_width(h) < xn.saturating_sub(x));
        if let Some(hint) = hint {
            // With the cursor at the end of the line it belongs before the
            // hint, not after it.
            cursor_col = cursor_col.or(Some(x));
            out.extend_from_slice(hint.as_bytes());
        }

        // Erase whatever the previous, longer frame left behind.
        out.extend_from_slice(b"\x1b[J");

        // With the cursor at the end of a line that exactly fills the last row,
        // the terminal leaves it in the final column rather than wrapping. Emit
        // a newline so the next character does not overwrite the last one.
        if pos > start && pos == end && x >= xn {
            out.extend_from_slice(b"\n\r");
            rows += 1;
            if cursor_col.is_none() {
                cursor_col = Some(0);
            }
        }

        if rows_below > 0 {
            push_csi(&mut out, rows_below, b'A');
        }
        match cursor_col {
            Some(0) => out.push(b'\r'),
            Some(col) => {
                out.push(b'\r');
                push_csi(&mut out, col, b'C');
            }
            // The cursor is already where it belongs, at the end of the text.
            None => {}
        }

        self.rows = rows;
        self.rows_below_cursor = rows_below;
        out
    }

    /// Chooses which slice of the line to show when it cannot all fit.
    ///
    /// Returns `(start, end, pos)` as byte offsets, plus the visible width.
    /// Text is dropped from whichever side the cursor is further from, so the
    /// region being edited stays on screen.
    fn visible_window(
        &self,
        frame: &Frame<'_>,
        pwidth: usize,
        xn: usize,
        yn: usize,
    ) -> ((usize, usize, usize), usize) {
        let (mut width, has_wide) = if frame.mask {
            (frame.line.chars().count(), false)
        } else {
            display_width_parts(frame.line)
        };

        let mut start = 0usize;
        let mut end = frame.line.len();

        // Leave slack for a wide glyph that cannot straddle the right edge.
        let tn = xn.saturating_sub(if has_wide { 2 } else { 0 }).max(1);
        let capacity = tn.saturating_mul(yn);

        loop {
            if pwidth + width + 1 < capacity {
                break;
            }
            if start >= end || width < 2 {
                break;
            }
            // The prompt alone fills the screen; there is nothing to gain.
            if pwidth + 2 > capacity {
                break;
            }
            let visible = &frame.line[start..end];
            let dropped = if frame.pos.saturating_sub(start) > (end - start) / 2 {
                let c = visible.chars().next().unwrap_or('\0');
                start += c.len_utf8();
                c
            } else {
                let c = visible.chars().next_back().unwrap_or('\0');
                end -= c.len_utf8();
                c
            };
            width = width.saturating_sub(if frame.mask { 1 } else { char_width(dropped) });
        }

        let pos = frame.pos.clamp(start, end);
        ((start, end, pos), width)
    }
}

/// Appends `ESC [ <n> <final>` to `out`.
fn push_csi(out: &mut Vec<u8>, n: usize, final_byte: u8) {
    out.extend_from_slice(b"\x1b[");
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    let mut n = n;
    loop {
        i -= 1;
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[i..]);
    out.push(final_byte);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal terminal emulator: enough of VT100 to check what the user
    /// would actually see. Escape-sequence assertions alone cannot catch a
    /// cursor that lands one row too high.
    ///
    /// It implements *deferred* wrap, which is what real terminals do and what
    /// the renderer relies on: writing into the last column leaves the cursor
    /// there with a pending wrap, and a following `\r` cancels it rather than
    /// skipping a row.
    struct Screen {
        cells: Vec<Vec<char>>,
        row: usize,
        col: usize,
        cols: usize,
        pending_wrap: bool,
    }

    impl Screen {
        fn new(rows: usize, cols: usize) -> Self {
            Screen {
                cells: vec![vec![' '; cols]; rows],
                row: 0,
                col: 0,
                cols,
                pending_wrap: false,
            }
        }

        fn ensure_row(&mut self) {
            while self.row >= self.cells.len() {
                self.cells.push(vec![' '; self.cols]);
            }
        }

        fn wrap_now(&mut self) {
            self.row += 1;
            self.col = 0;
            self.pending_wrap = false;
            self.ensure_row();
        }

        fn put(&mut self, c: char) {
            let w = char_width(c);
            if w == 0 {
                return;
            }
            if self.pending_wrap {
                self.wrap_now();
            }
            // A double-width glyph will not be split across the right edge.
            if self.col + w > self.cols {
                self.wrap_now();
            }
            self.ensure_row();
            self.cells[self.row][self.col] = c;
            for k in 1..w {
                self.cells[self.row][self.col + k] = '\0';
            }
            self.col += w;
            if self.col >= self.cols {
                self.col = self.cols;
                self.pending_wrap = true;
            }
        }

        /// Applies one CSI sequence, given its numeric parameter and final byte.
        fn csi(&mut self, param: &str, final_byte: char) {
            let n: usize = param.parse().unwrap_or(1).max(1);
            self.pending_wrap = false;
            self.ensure_row();
            match final_byte {
                'A' => self.row = self.row.saturating_sub(n),
                'B' => {
                    self.row += n;
                    self.ensure_row();
                }
                'C' => self.col = (self.col + n).min(self.cols),
                'D' => self.col = self.col.saturating_sub(n),
                'K' => self.erase_to_end_of_line(),
                'J' => {
                    self.erase_to_end_of_line();
                    for row in self.cells[self.row + 1..].iter_mut() {
                        row.fill(' ');
                    }
                }
                // Bold on/off and private modes do not move the cursor.
                _ => {}
            }
        }

        fn erase_to_end_of_line(&mut self) {
            let col = self.col.min(self.cols);
            for c in self.cells[self.row][col..].iter_mut() {
                *c = ' ';
            }
        }

        fn feed(&mut self, bytes: &[u8]) {
            let chars: Vec<char> = String::from_utf8_lossy(bytes).chars().collect();
            let mut i = 0;
            while i < chars.len() {
                let c = chars[i];
                i += 1;
                match c {
                    '\r' => {
                        self.col = 0;
                        self.pending_wrap = false;
                    }
                    '\n' => {
                        self.row += 1;
                        self.pending_wrap = false;
                        self.ensure_row();
                    }
                    '\x1b' if chars.get(i) == Some(&'[') => {
                        i += 1;
                        let start = i;
                        i += chars[i..].iter().take_while(|c| is_csi_param(**c)).count();
                        let param: String = chars[start..i].iter().collect();
                        let final_byte = chars.get(i).copied().unwrap_or(' ');
                        i += 1;
                        self.csi(&param, final_byte);
                    }
                    // A lone ESC that is not a CSI introducer is ignored.
                    '\x1b' => {}
                    c => self.put(c),
                }
            }
        }

        fn line(&self, row: usize) -> String {
            self.cells
                .get(row)
                .map(|r| {
                    r.iter()
                        .filter(|c| **c != '\0')
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .unwrap_or_default()
        }

        /// Where the next character would be drawn.
        fn cursor(&self) -> (usize, usize) {
            if self.pending_wrap {
                (self.row + 1, 0)
            } else {
                (self.row, self.col)
            }
        }
    }

    /// Whether `c` is a CSI parameter or private-marker byte.
    fn is_csi_param(c: char) -> bool {
        c.is_ascii_digit() || c == ';' || c == '?'
    }

    fn renderer(cols: u16) -> Renderer {
        Renderer::new(WinSize { rows: 24, cols })
    }

    #[test]
    fn simple_line_places_the_cursor() {
        let mut r = renderer(80);
        let mut s = Screen::new(24, 80);
        s.feed(&r.render(&Frame::new("> ", "hello", 5)));
        assert_eq!(s.line(0), "> hello");
        assert_eq!(s.cursor(), (0, 7));
    }

    #[test]
    fn cursor_inside_the_line() {
        let mut r = renderer(80);
        let mut s = Screen::new(24, 80);
        s.feed(&r.render(&Frame::new("> ", "hello", 2)));
        assert_eq!(s.line(0), "> hello");
        assert_eq!(s.cursor(), (0, 4));
    }

    #[test]
    fn ansi_prompt_does_not_shift_the_cursor() {
        let mut r = renderer(80);
        let mut s = Screen::new(24, 80);
        // The colour codes must contribute zero columns.
        s.feed(&r.render(&Frame::new("\x1b[32m> \x1b[0m", "hi", 2)));
        assert_eq!(s.line(0), "> hi");
        assert_eq!(s.cursor(), (0, 4));
    }

    #[test]
    fn wrapped_line_cursor_at_end() {
        let mut r = renderer(20);
        let mut s = Screen::new(24, 20);
        let line = "a".repeat(30);
        s.feed(&r.render(&Frame::new("> ", &line, line.len())));
        assert_eq!(s.line(0), "> ".to_string() + &"a".repeat(18));
        assert_eq!(s.line(1), "a".repeat(12));
        assert_eq!(s.cursor(), (1, 12));
    }

    #[test]
    fn redraw_with_the_cursor_on_an_upper_row_stays_aligned() {
        // The regression test for the row arithmetic: after moving the cursor
        // onto the first row of a wrapped line, the next redraw must not walk
        // up past the prompt.
        let mut r = renderer(20);
        let mut s = Screen::new(24, 20);
        let line = "a".repeat(30);

        s.feed(&r.render(&Frame::new("> ", &line, line.len())));
        // Move the cursor to offset 5, which is on the first row.
        s.feed(&r.render(&Frame::new("> ", &line, 5)));
        assert_eq!(s.cursor(), (0, 7));

        // Now type a character. The prompt must still be on row 0.
        let mut edited = line.clone();
        edited.insert(5, 'Z');
        s.feed(&r.render(&Frame::new("> ", &edited, 6)));
        assert_eq!(&s.line(0)[..2], "> ");
        assert!(s.line(0).contains('Z'), "row 0 is {:?}", s.line(0));
        assert_eq!(s.cursor(), (0, 8));
    }

    #[test]
    fn many_redraws_never_drift() {
        let mut r = renderer(20);
        let mut s = Screen::new(24, 20);
        let line = "b".repeat(45);
        // Sweep the cursor across every position; the prompt must stay put.
        for pos in (0..=line.len()).rev() {
            s.feed(&r.render(&Frame::new("> ", &line, pos)));
            assert_eq!(&s.line(0)[..2], "> ", "prompt moved at pos {pos}");
        }
    }

    #[test]
    fn wide_characters_do_not_straddle_the_edge() {
        let mut r = renderer(10);
        let mut s = Screen::new(24, 10);
        // Prompt is 2 columns; five double-width glyphs need 10 more.
        let line = "中".repeat(5);
        s.feed(&r.render(&Frame::new("> ", &line, line.len())));
        // Row 0 holds the prompt plus four glyphs (8 columns); the fifth wraps.
        assert_eq!(s.line(0), "> 中中中中");
        assert_eq!(s.line(1), "中");
    }

    #[test]
    fn cursor_width_accounts_for_wide_glyphs() {
        let mut r = renderer(80);
        let mut s = Screen::new(24, 80);
        // é is 1 column, 中 is 2, so the cursor after both is at 2 + 3.
        s.feed(&r.render(&Frame::new("> ", "é中", "é中".len())));
        assert_eq!(s.cursor(), (0, 5));
    }

    #[test]
    fn shrinking_line_erases_the_tail() {
        let mut r = renderer(40);
        let mut s = Screen::new(24, 40);
        s.feed(&r.render(&Frame::new("> ", "hello world", 11)));
        s.feed(&r.render(&Frame::new("> ", "hi", 2)));
        assert_eq!(s.line(0), "> hi");
    }

    #[test]
    fn shrinking_from_two_rows_to_one_clears_the_second() {
        let mut r = renderer(20);
        let mut s = Screen::new(24, 20);
        let long = "x".repeat(30);
        s.feed(&r.render(&Frame::new("> ", &long, long.len())));
        assert!(!s.line(1).is_empty());
        s.feed(&r.render(&Frame::new("> ", "short", 5)));
        assert_eq!(s.line(0), "> short");
        assert_eq!(s.line(1), "");
    }

    #[test]
    fn mask_mode_hides_the_text() {
        let mut r = renderer(80);
        let mut s = Screen::new(24, 80);
        let mut frame = Frame::new("password: ", "hunter2", 7);
        frame.mask = true;
        s.feed(&r.render(&frame));
        assert_eq!(s.line(0), "password: *******");
        assert_eq!(s.cursor(), (0, 17));
    }

    #[test]
    fn mask_mode_masks_wide_characters_as_one_column() {
        let mut r = renderer(80);
        let mut s = Screen::new(24, 80);
        let mut frame = Frame::new("> ", "中中", 6);
        frame.mask = true;
        s.feed(&r.render(&frame));
        assert_eq!(s.line(0), "> **");
    }

    #[test]
    fn hint_is_drawn_and_the_cursor_stays_before_it() {
        let mut r = renderer(80);
        let mut s = Screen::new(24, 80);
        let mut frame = Frame::new("> ", "git", 3);
        frame.hint = Some(" status");
        s.feed(&r.render(&frame));
        assert_eq!(s.line(0), "> git status");
        // Cursor sits after "git", not after the hint.
        assert_eq!(s.cursor(), (0, 5));
    }

    #[test]
    fn hint_is_omitted_when_it_does_not_fit() {
        let mut r = renderer(12);
        let mut s = Screen::new(24, 12);
        let mut frame = Frame::new("> ", "git", 3);
        frame.hint = Some(" a very long hint indeed");
        s.feed(&r.render(&frame));
        assert_eq!(s.line(0), "> git");
    }

    #[test]
    fn finished_frame_drops_hint_and_highlight() {
        let mut r = renderer(80);
        let mut s = Screen::new(24, 80);
        let mut frame = Frame::new("> ", "(x)", 3);
        frame.hint = Some(" hint");
        frame.highlight = Some((0, 2));
        frame.finished = true;
        let bytes = r.render(&frame);
        s.feed(&bytes);
        assert_eq!(s.line(0), "> (x)");
        assert!(!bytes.windows(4).any(|w| w == b"\x1b[1m"));
    }

    #[test]
    fn highlight_emits_bold_around_the_pair() {
        let mut r = renderer(80);
        let mut frame = Frame::new("> ", "(x)", 3);
        frame.highlight = Some((0, 2));
        let bytes = r.render(&frame);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("\x1b[1m(\x1b[22m"));
        assert!(text.contains("\x1b[1m)\x1b[22m"));
    }

    #[test]
    fn over_long_line_keeps_the_cursor_visible() {
        // Two rows of 20 columns cannot hold 500 characters; whatever is shown
        // must include the cursor.
        let mut r = Renderer::new(WinSize { rows: 2, cols: 20 });
        let line = "c".repeat(500);
        for pos in [0, 250, 500] {
            let mut s = Screen::new(2, 20);
            s.feed(&r.render(&Frame::new("> ", &line, pos)));
            let (row, col) = s.cursor();
            assert!(row < 4, "cursor escaped the screen at pos {pos}");
            assert!(col <= 20, "cursor past the right edge at pos {pos}");
        }
    }

    #[test]
    fn narrow_terminal_does_not_panic() {
        for cols in 1..=6u16 {
            let mut r = renderer(cols);
            for pos in 0..=5 {
                let _ = r.render(&Frame::new(">>>> ", "hello", pos));
            }
        }
    }

    #[test]
    fn single_column_terminal_terminates() {
        let mut r = Renderer::new(WinSize { rows: 1, cols: 1 });
        let _ = r.render(&Frame::new("> ", "hello world", 4));
    }

    #[test]
    fn empty_line_renders_just_the_prompt() {
        let mut r = renderer(80);
        let mut s = Screen::new(24, 80);
        s.feed(&r.render(&Frame::new("> ", "", 0)));
        assert_eq!(s.line(0), ">");
        assert_eq!(s.cursor(), (0, 2));
    }

    #[test]
    fn finish_moves_below_the_line() {
        let mut r = renderer(20);
        let mut s = Screen::new(24, 20);
        let line = "a".repeat(30);
        // Cursor on row 0 of a two-row line.
        s.feed(&r.render(&Frame::new("> ", &line, 3)));
        assert_eq!(s.cursor().0, 0);
        s.feed(&r.finish());
        // finish() must land below both rows, not on top of row 1.
        assert_eq!(s.cursor(), (2, 0));
    }

    #[test]
    fn csi_encoding_is_correct() {
        let mut out = Vec::new();
        push_csi(&mut out, 0, b'A');
        push_csi(&mut out, 7, b'C');
        push_csi(&mut out, 123, b'B');
        assert_eq!(out, b"\x1b[0A\x1b[7C\x1b[123B");
    }
}
