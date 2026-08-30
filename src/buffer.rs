//! The editable line buffer and every operation that mutates it.
//!
//! The buffer holds UTF-8 text plus a cursor expressed as a byte offset that is
//! always on a character boundary. All movement helpers are ports of the
//! corresponding routines in bestline, so word, expression and paredit
//! semantics match the C implementation.

use crate::unicode::{
    is_separator, is_xeparator, mirror_left, mirror_right, to_lowercase, to_uppercase,
};

/// A line of text being edited, with a cursor and a mark.
#[derive(Debug, Clone, Default)]
pub struct LineBuffer {
    data: String,
    /// Byte offset of the cursor. Always on a character boundary.
    pos: usize,
    /// Byte offset of the mark set by `CTRL-SPACE`.
    mark: usize,
}

impl LineBuffer {
    /// Creates an empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ---------------------------------------------------------------- access

    /// The buffer contents.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.data
    }

    /// Byte offset of the cursor.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.pos
    }

    /// Whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Moves the cursor to `pos`, clamped to the buffer and snapped down to the
    /// nearest character boundary.
    pub fn set_cursor(&mut self, pos: usize) {
        let mut pos = pos.min(self.data.len());
        while pos > 0 && !self.data.is_char_boundary(pos) {
            pos -= 1;
        }
        self.pos = pos;
    }

    /// Empties the buffer and resets the cursor and mark.
    pub fn clear(&mut self) {
        self.data.clear();
        self.pos = 0;
        self.mark = 0;
    }

    /// Replaces the contents and places the cursor at the end.
    pub fn set_content(&mut self, content: &str) {
        self.data.clear();
        self.data.push_str(content);
        self.pos = self.data.len();
        self.mark = self.mark.min(self.data.len());
    }

    // ------------------------------------------------------- boundary helpers

    /// Byte offset of the character boundary before `pos`.
    #[must_use]
    pub fn backward(&self, pos: usize) -> usize {
        self.data[..pos]
            .chars()
            .next_back()
            .map_or(pos, |c| pos - c.len_utf8())
    }

    /// Byte offset of the character boundary after `pos`.
    #[must_use]
    pub fn forward(&self, pos: usize) -> usize {
        self.data[pos..]
            .chars()
            .next()
            .map_or(pos, |c| pos + c.len_utf8())
    }

    /// Character starting at `pos`, if any.
    #[must_use]
    pub fn char_at(&self, pos: usize) -> Option<char> {
        self.data.get(pos..).and_then(|s| s.chars().next())
    }

    /// Character ending at `pos`, if any.
    #[must_use]
    pub fn char_before(&self, pos: usize) -> Option<char> {
        self.data.get(..pos).and_then(|s| s.chars().next_back())
    }

    /// Scans left from `pos` while `pred` holds for the preceding character.
    fn backwards(&self, mut pos: usize, pred: fn(char) -> bool) -> usize {
        while let Some(c) = self.char_before(pos) {
            if !pred(c) {
                break;
            }
            pos = self.backward(pos);
        }
        pos
    }

    /// Scans right from `pos` while `pred` holds for the character at `pos`.
    fn forwards(&self, mut pos: usize, pred: fn(char) -> bool) -> usize {
        while let Some(c) = self.char_at(pos) {
            if !pred(c) {
                break;
            }
            pos = self.forward(pos);
        }
        pos
    }

    /// Offset one word to the right of `pos`: skip separators, then the word.
    fn forward_word(&self, pos: usize) -> usize {
        let pos = self.forwards(pos, is_separator);
        self.forwards(pos, |c| !is_separator(c))
    }

    /// Offset one word to the left of `pos`.
    fn backward_word(&self, pos: usize) -> usize {
        let pos = self.backwards(pos, is_separator);
        self.backwards(pos, |c| !is_separator(c))
    }

    /// Advances `i` until it sits on a word boundary, used by word transposition.
    fn escape_word(&self, mut i: usize) -> usize {
        while i != 0 && i < self.data.len() {
            if self.char_at(i).is_some_and(is_separator) {
                break;
            }
            if self.char_before(i).is_some_and(is_separator) {
                break;
            }
            i = self.forward(i);
        }
        i
    }

    // ------------------------------------------------------------ insertion

    /// Inserts `s` at the cursor and advances past it.
    pub fn insert(&mut self, s: &str) {
        self.data.insert_str(self.pos, s);
        self.pos += s.len();
    }

    /// Inserts a single character at the cursor.
    pub fn insert_char(&mut self, c: char) {
        self.data.insert(self.pos, c);
        self.pos += c.len_utf8();
    }

    /// Replaces `start..end` with `replacement`, keeping the cursor sensible.
    ///
    /// Does nothing if the range is invalid or not on character boundaries.
    pub fn replace_range(&mut self, start: usize, end: usize, replacement: &str) {
        if start > end
            || end > self.data.len()
            || !self.data.is_char_boundary(start)
            || !self.data.is_char_boundary(end)
        {
            return;
        }
        self.data.replace_range(start..end, replacement);
        if self.pos >= end {
            self.pos = self.pos - (end - start) + replacement.len();
        } else if self.pos > start {
            self.pos = start + replacement.len();
        }
        self.mark = self.mark.min(self.data.len());
    }

    // ------------------------------------------------------------- deletion

    /// Deletes the character at the cursor.
    pub fn delete(&mut self) {
        if self.pos == self.data.len() {
            return;
        }
        let end = self.forward(self.pos);
        self.data.replace_range(self.pos..end, "");
    }

    /// Deletes the character before the cursor.
    pub fn rubout(&mut self) {
        if self.pos == 0 {
            return;
        }
        let start = self.backward(self.pos);
        self.data.replace_range(start..self.pos, "");
        self.pos = start;
    }

    /// Deletes the word after the cursor, returning it for the kill ring.
    pub fn delete_word(&mut self) -> String {
        let end = self.forward_word(self.pos);
        let killed = self.data[self.pos..end].to_string();
        self.data.replace_range(self.pos..end, "");
        killed
    }

    /// Deletes the word before the cursor, returning it for the kill ring.
    pub fn rubout_word(&mut self) -> String {
        let start = self.backward_word(self.pos);
        let killed = self.data[start..self.pos].to_string();
        self.data.replace_range(start..self.pos, "");
        self.pos = start;
        killed
    }

    /// Deletes from the cursor to the end of the line.
    pub fn kill_to_end(&mut self) -> String {
        let killed = self.data[self.pos..].to_string();
        self.data.truncate(self.pos);
        killed
    }

    /// Deletes from the start of the line to the cursor.
    pub fn kill_to_start(&mut self) -> String {
        let killed = self.data[..self.pos].to_string();
        self.data.replace_range(..self.pos, "");
        self.pos = 0;
        killed
    }

    // ------------------------------------------------------------- movement

    /// Moves the cursor one character left.
    pub fn move_left(&mut self) {
        self.pos = self.backward(self.pos);
    }

    /// Moves the cursor one character right.
    pub fn move_right(&mut self) {
        self.pos = self.forward(self.pos);
    }

    /// Moves the cursor to the start of the line.
    pub fn move_home(&mut self) {
        self.pos = 0;
    }

    /// Moves the cursor to the end of the line.
    pub fn move_end(&mut self) {
        self.pos = self.data.len();
    }

    /// Moves the cursor one word left.
    pub fn move_word_left(&mut self) {
        self.pos = self.backward_word(self.pos);
    }

    /// Moves the cursor one word right.
    pub fn move_word_right(&mut self) {
        self.pos = self.forward_word(self.pos);
    }

    /// Moves the cursor left by one balanced expression.
    pub fn move_expr_left(&mut self) {
        self.pos = self.backwards(self.pos, is_xeparator);
        if let Some((start, _)) = self.mirror_left_match() {
            self.pos = start;
        } else {
            self.pos = self.backwards(self.pos, |c| !is_separator(c));
        }
    }

    /// Moves the cursor right by one balanced expression.
    pub fn move_expr_right(&mut self) {
        self.pos = self.forwards(self.pos, is_xeparator);
        if let Some((_, end)) = self.mirror_right_match() {
            self.pos = self.forward(end);
        } else {
            self.pos = self.forwards(self.pos, |c| !is_separator(c));
        }
    }

    // ----------------------------------------------------------------- mark

    /// Records the cursor position as the mark.
    pub fn set_mark(&mut self) {
        self.mark = self.pos;
    }

    /// Moves the cursor to the mark.
    pub fn goto_mark(&mut self) {
        if self.mark <= self.data.len() {
            self.set_cursor(self.mark);
        }
    }

    // ----------------------------------------------------------- transforms

    /// Swaps the two characters around the cursor and moves past them.
    pub fn transpose(&mut self) {
        let b = if self.pos == self.data.len() {
            self.backward(self.pos)
        } else {
            self.pos
        };
        let a = self.backward(b);
        let c = self.forward(b);
        if !(a < b && b < c) {
            return;
        }
        let swapped = format!("{}{}", &self.data[b..c], &self.data[a..b]);
        self.data.replace_range(a..c, &swapped);
        self.pos = c;
    }

    /// Swaps the word before the cursor with the word after it.
    pub fn transpose_words(&mut self) {
        let mut i = self.pos;
        if i == self.data.len() {
            i = self.backwards(i, is_separator);
            i = self.backwards(i, |c| !is_separator(c));
        }
        let pivot = self.escape_word(i);
        let xj = self.backwards(pivot, is_separator);
        let xi = self.backwards(xj, |c| !is_separator(c));
        let yi = self.forwards(pivot, is_separator);
        let yj = self.forwards(yi, |c| !is_separator(c));
        if !(xi < xj && xj < yi && yi < yj) {
            return;
        }
        let reordered = format!(
            "{}{}{}",
            &self.data[yi..yj],
            &self.data[xj..yi],
            &self.data[xi..xj]
        );
        self.data.replace_range(xi..yj, &reordered);
        self.pos = yj;
    }

    /// Applies `xlat` to each character of the word at or after the cursor.
    ///
    /// Characters that `xlat` leaves unchanged are copied verbatim, which
    /// avoids canonicalizing text the user did not ask to change.
    fn xlat_word(&mut self, mut xlat: impl FnMut(char) -> char) {
        let start = self.forwards(self.pos, is_separator);
        let end = self.forwards(start, |c| !is_separator(c));
        if start == end {
            return;
        }
        let translated: String = self.data[start..end].chars().map(&mut xlat).collect();
        self.data.replace_range(start..end, &translated);
        self.pos = start + translated.len();
    }

    /// Lowercases the next word.
    pub fn lowercase_word(&mut self) {
        self.xlat_word(to_lowercase);
    }

    /// Uppercases the next word.
    pub fn uppercase_word(&mut self) {
        self.xlat_word(to_uppercase);
    }

    /// Uppercases the first character of the next word.
    pub fn capitalize_word(&mut self) {
        let mut first = true;
        self.xlat_word(|c| {
            if std::mem::take(&mut first) {
                to_uppercase(c)
            } else {
                c
            }
        });
    }

    /// Collapses the run of separators surrounding the cursor.
    pub fn squeeze(&mut self) {
        let start = self.backwards(self.pos, is_separator);
        let end = self.forwards(self.pos, is_separator);
        if start >= end {
            return;
        }
        self.data.replace_range(start..end, "");
        self.pos = start;
    }

    // --------------------------------------------------------- bracket pairs

    /// Finds the bracket pair closing immediately before the cursor.
    ///
    /// Returns the byte offsets of the opening and closing brackets.
    #[must_use]
    pub fn mirror_left_match(&self) -> Option<(usize, usize)> {
        let index = self.backward(self.pos);
        if index == 0 {
            return None;
        }
        let right = self.char_at(index)?;
        let left = mirror_left(right)?;
        let mut depth = 0usize;
        let mut pos = index;
        loop {
            pos = self.backward(pos);
            let c = self.char_at(pos)?;
            if c == right {
                depth += 1;
            } else if c == left {
                match depth.checked_sub(1) {
                    Some(d) => depth = d,
                    None => return Some((pos, index)),
                }
            }
            if pos == 0 {
                return None;
            }
        }
    }

    /// Finds the bracket pair opening at the cursor.
    #[must_use]
    pub fn mirror_right_match(&self) -> Option<(usize, usize)> {
        let index = self.pos;
        let left = self.char_at(index)?;
        let right = mirror_right(left)?;
        let mut depth = 0usize;
        let mut pos = index;
        loop {
            pos = self.forward(pos);
            let c = self.char_at(pos)?;
            if c == left {
                depth += 1;
            } else if c == right {
                match depth.checked_sub(1) {
                    Some(d) => depth = d,
                    None => return Some((index, pos)),
                }
            }
            if self.forward(pos) >= self.data.len() {
                return None;
            }
        }
    }

    /// Bracket pair to highlight for the current cursor position, if any.
    #[must_use]
    pub fn mirror_match(&self) -> Option<(usize, usize)> {
        self.mirror_left_match()
            .or_else(|| self.mirror_right_match())
    }

    /// Moves the last item inside the enclosing s-expression to outside it.
    ///
    /// `(a| b c)` becomes `(a| b) c`.
    pub fn barf(&mut self) {
        let mut stack: Vec<char> = Vec::new();
        let mut pos = self.pos;

        // Walk right to the closing bracket of the enclosing expression.
        let end = loop {
            if pos == self.data.len() {
                return;
            }
            let Some(c) = self.char_at(pos) else { return };
            if let Some(&top) = stack.last() {
                if c == top {
                    stack.pop();
                }
            } else if let Some(rhs) = mirror_right(c) {
                stack.push(rhs);
            } else if mirror_left(c).is_some() {
                break pos;
            }
            pos = self.forward(pos);
        };

        // Walk back over one item.
        pos = self.backwards(pos, is_xeparator);
        loop {
            if pos == 0 {
                return;
            }
            let i = self.backward(pos);
            let Some(c) = self.char_at(i) else { return };
            if let Some(&top) = stack.last() {
                if c == top {
                    stack.pop();
                }
            } else if let Some(lhs) = mirror_left(c) {
                stack.push(lhs);
            } else if is_separator(c) {
                break;
            }
            pos = i;
        }
        pos = self.backwards(pos, is_xeparator);

        let Some(closer) = self.char_at(end) else {
            return;
        };
        self.data.remove(end);
        self.data.insert(pos, closer);
        if self.pos > pos {
            self.pos += closer.len_utf8();
        }
    }

    /// Moves the first item outside the enclosing s-expression to inside it.
    ///
    /// `(a| b) c d` becomes `(a| b c) d`.
    pub fn slurp(&mut self) {
        let mut stack: Vec<char> = Vec::new();
        let mut pos = self.pos;
        let mut point = None;

        // Walk right to the closing bracket of the enclosing expression.
        while pos < self.data.len() {
            let Some(c) = self.char_at(pos) else { return };
            if let Some(&top) = stack.last() {
                if c == top {
                    stack.pop();
                }
            } else if let Some(rhs) = mirror_right(c) {
                stack.push(rhs);
            } else if mirror_left(c).is_some() {
                point = Some(pos);
                pos = self.forward(pos);
                break;
            }
            pos = self.forward(pos);
        }
        let Some(point) = point else { return };
        let start = pos;

        // Walk forward over one item.
        pos = self.forwards(pos, is_xeparator);
        while pos < self.data.len() {
            let Some(c) = self.char_at(pos) else { break };
            if let Some(&top) = stack.last() {
                if c == top {
                    stack.pop();
                }
            } else if let Some(rhs) = mirror_right(c) {
                stack.push(rhs);
            } else if is_separator(c) {
                break;
            }
            pos = self.forward(pos);
        }

        let closer = self.data[point..start].to_string();
        self.data.replace_range(point..start, "");
        let insert_at = pos - (start - point);
        self.data.insert_str(insert_at, &closer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str, pos: usize) -> LineBuffer {
        let mut b = LineBuffer::new();
        b.set_content(text);
        b.set_cursor(pos);
        b
    }

    /// Renders the buffer with `|` marking the cursor, for readable assertions.
    fn render(b: &LineBuffer) -> String {
        let mut s = b.as_str().to_string();
        s.insert(b.cursor(), '|');
        s
    }

    #[test]
    fn insert_and_delete() {
        let mut b = LineBuffer::new();
        b.insert("hello");
        assert_eq!(render(&b), "hello|");
        b.rubout();
        assert_eq!(render(&b), "hell|");
        b.move_home();
        b.delete();
        assert_eq!(render(&b), "|ell");
    }

    #[test]
    fn utf8_movement_lands_on_boundaries() {
        let mut b = buf("é中😀", 0);
        b.move_right();
        assert_eq!(b.cursor(), 2);
        b.move_right();
        assert_eq!(b.cursor(), 5);
        b.move_right();
        assert_eq!(b.cursor(), 9);
        b.move_right();
        assert_eq!(b.cursor(), 9);
        b.move_left();
        assert_eq!(b.cursor(), 5);
    }

    #[test]
    fn word_movement() {
        let mut b = buf("foo bar  baz", 12);
        b.move_word_left();
        assert_eq!(render(&b), "foo bar  |baz");
        b.move_word_left();
        assert_eq!(render(&b), "foo |bar  baz");
        b.move_word_right();
        assert_eq!(render(&b), "foo bar|  baz");
    }

    #[test]
    fn kill_operations_return_text() {
        let mut b = buf("foo bar", 7);
        assert_eq!(b.rubout_word(), "bar");
        assert_eq!(render(&b), "foo |");

        let mut b = buf("foo bar", 3);
        assert_eq!(b.kill_to_end(), " bar");
        assert_eq!(render(&b), "foo|");

        let mut b = buf("foo bar", 4);
        assert_eq!(b.kill_to_start(), "foo ");
        assert_eq!(render(&b), "|bar");

        let mut b = buf("foo bar", 3);
        assert_eq!(b.delete_word(), " bar");
        assert_eq!(render(&b), "foo|");
    }

    #[test]
    fn transpose_chars() {
        let mut b = buf("abc", 1);
        b.transpose();
        assert_eq!(render(&b), "ba|c");

        // At end of line, transpose the final pair.
        let mut b = buf("abc", 3);
        b.transpose();
        assert_eq!(render(&b), "acb|");

        // Multi-byte characters transpose whole, not per byte.
        let mut b = buf("aé", 3);
        b.transpose();
        assert_eq!(b.as_str(), "éa");
    }

    #[test]
    fn transpose_word_pair() {
        let mut b = buf("foo bar", 7);
        b.transpose_words();
        assert_eq!(render(&b), "bar foo|");
    }

    #[test]
    fn case_operations() {
        let mut b = buf("hello world", 0);
        b.uppercase_word();
        assert_eq!(render(&b), "HELLO| world");

        let mut b = buf("HELLO world", 0);
        b.lowercase_word();
        assert_eq!(render(&b), "hello| world");

        let mut b = buf("hello world", 0);
        b.capitalize_word();
        assert_eq!(render(&b), "Hello| world");

        // Leading separators are skipped.
        let mut b = buf("  hi", 0);
        b.uppercase_word();
        assert_eq!(render(&b), "  HI|");
    }

    #[test]
    fn squeeze_whitespace() {
        let mut b = buf("foo     bar", 5);
        b.squeeze();
        assert_eq!(render(&b), "foo|bar");
    }

    #[test]
    fn mark_and_goto() {
        let mut b = buf("hello", 2);
        b.set_mark();
        b.move_end();
        assert_eq!(b.cursor(), 5);
        b.goto_mark();
        assert_eq!(b.cursor(), 2);
    }

    #[test]
    fn bracket_matching() {
        // Cursor just past the closing paren.
        let b = buf("(a b)", 5);
        assert_eq!(b.mirror_left_match(), Some((0, 4)));

        // Cursor on the opening paren.
        let b = buf("(a b)", 0);
        assert_eq!(b.mirror_right_match(), Some((0, 4)));

        // Nested.
        let b = buf("((a))", 5);
        assert_eq!(b.mirror_left_match(), Some((0, 4)));

        let b = buf("no brackets", 5);
        assert_eq!(b.mirror_match(), None);
    }

    #[test]
    fn expression_movement() {
        let mut b = buf("(a b) c", 7);
        b.move_expr_left();
        assert_eq!(render(&b), "(a b) |c");
        b.move_expr_left();
        assert_eq!(render(&b), "|(a b) c");

        let mut b = buf("(a b) c", 0);
        b.move_expr_right();
        assert_eq!(render(&b), "(a b)| c");
    }

    #[test]
    fn barf_moves_item_out() {
        let mut b = buf("(a b c)", 2);
        b.barf();
        assert_eq!(b.as_str(), "(a b) c");
    }

    #[test]
    fn slurp_moves_item_in() {
        let mut b = buf("(a b) c d", 2);
        b.slurp();
        assert_eq!(b.as_str(), "(a b c) d");
    }

    #[test]
    fn barf_and_slurp_are_inverses() {
        let mut b = buf("(a b c) d", 2);
        b.barf();
        assert_eq!(b.as_str(), "(a b) c d");
        b.slurp();
        assert_eq!(b.as_str(), "(a b c) d");
    }

    #[test]
    fn barf_and_slurp_on_unbalanced_text_do_nothing() {
        for text in ["", "no parens", "(unclosed", "closed)"] {
            let mut b = buf(text, 0);
            b.barf();
            b.slurp();
            // The only requirement is that nothing panics and text stays valid.
            assert!(b.as_str().is_char_boundary(b.cursor()));
        }
    }

    #[test]
    fn replace_range_keeps_cursor_valid() {
        let mut b = buf("hello world", 11);
        b.replace_range(0, 5, "goodbye");
        assert_eq!(render(&b), "goodbye world|");

        // Invalid ranges are ignored rather than panicking.
        let mut b = buf("é", 0);
        b.replace_range(0, 1, "x");
        assert_eq!(b.as_str(), "é");
        b.replace_range(5, 9, "x");
        assert_eq!(b.as_str(), "é");
    }

    #[test]
    fn set_cursor_snaps_to_boundary() {
        let mut b = buf("é中", 0);
        b.set_cursor(1);
        assert_eq!(b.cursor(), 0);
        b.set_cursor(3);
        assert_eq!(b.cursor(), 2);
        b.set_cursor(999);
        assert_eq!(b.cursor(), 5);
    }

    /// Every mutating operation, applied by index so a property test can
    /// generate an arbitrary sequence of them.
    fn apply(b: &mut LineBuffer, op: u8, text: &str) {
        match op % 22 {
            0 => b.insert(text),
            1 => b.delete(),
            2 => b.rubout(),
            3 => b.move_left(),
            4 => b.move_right(),
            5 => b.move_home(),
            6 => b.move_end(),
            7 => b.move_word_left(),
            8 => b.move_word_right(),
            9 => b.move_expr_left(),
            10 => b.move_expr_right(),
            11 => drop(b.delete_word()),
            12 => drop(b.rubout_word()),
            13 => drop(b.kill_to_end()),
            14 => drop(b.kill_to_start()),
            15 => b.transpose(),
            16 => b.transpose_words(),
            17 => b.uppercase_word(),
            18 => b.lowercase_word(),
            19 => b.squeeze(),
            20 => b.barf(),
            _ => b.slurp(),
        }
    }

    /// The invariant every operation must preserve: the cursor and the mark
    /// stay on character boundaries inside the buffer.
    fn check_invariants(b: &LineBuffer) {
        assert!(
            b.cursor() <= b.as_str().len(),
            "cursor {} past end of {:?}",
            b.cursor(),
            b.as_str(),
        );
        assert!(
            b.as_str().is_char_boundary(b.cursor()),
            "cursor {} splits a character in {:?}",
            b.cursor(),
            b.as_str(),
        );
        assert!(b.mark <= b.as_str().len(), "mark escaped the buffer");
    }

    proptest::proptest! {
        /// No sequence of edits over arbitrary text may panic or leave the
        /// cursor off a character boundary.
        #[test]
        fn arbitrary_edits_preserve_invariants(
            initial in ".{0,40}",
            ops in proptest::collection::vec((0u8..22, ".{0,4}"), 0..60),
        ) {
            let mut b = LineBuffer::new();
            b.set_content(&initial);
            check_invariants(&b);

            for (op, text) in ops {
                apply(&mut b, op, &text);
                check_invariants(&b);
            }
        }

        /// Moving the cursor never changes the text.
        #[test]
        fn movement_never_edits(
            initial in ".{0,60}",
            ops in proptest::collection::vec(3u8..11, 0..40),
        ) {
            let mut b = LineBuffer::new();
            b.set_content(&initial);
            for op in ops {
                apply(&mut b, op, "");
            }
            proptest::prop_assert_eq!(b.as_str(), initial.as_str());
        }

        /// Inserting text and then removing exactly that much restores the
        /// original buffer.
        #[test]
        fn insert_then_rubout_is_identity(
            initial in ".{0,30}",
            inserted in ".{0,10}",
        ) {
            let mut b = LineBuffer::new();
            b.set_content(&initial);
            let before = b.as_str().to_string();
            let cursor = b.cursor();

            b.insert(&inserted);
            for _ in inserted.chars() {
                b.rubout();
            }
            proptest::prop_assert_eq!(b.as_str(), before.as_str());
            proptest::prop_assert_eq!(b.cursor(), cursor);
        }

        /// Whatever a kill removes is exactly what disappears from the text.
        #[test]
        fn kills_return_what_they_remove(
            initial in ".{0,40}",
            position in 0usize..40,
        ) {
            let mut b = LineBuffer::new();
            b.set_content(&initial);
            b.set_cursor(position);
            let before = b.as_str().to_string();
            let cursor = b.cursor();

            let killed = b.kill_to_end();
            proptest::prop_assert_eq!(&killed, &before[cursor..]);
            proptest::prop_assert_eq!(b.as_str(), &before[..cursor]);
        }
    }

    #[test]
    fn operations_on_empty_buffer_are_safe() {
        let mut b = LineBuffer::new();
        b.delete();
        b.rubout();
        b.move_left();
        b.move_right();
        b.move_word_left();
        b.move_word_right();
        b.move_expr_left();
        b.move_expr_right();
        b.transpose();
        b.transpose_words();
        b.uppercase_word();
        b.squeeze();
        b.barf();
        b.slurp();
        assert_eq!(b.as_str(), "");
        assert_eq!(b.cursor(), 0);
    }
}
