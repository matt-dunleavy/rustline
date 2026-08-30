use crate::unicode::is_separator;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone)]
pub struct LineBuffer {
    data: String,
    cursor: usize,
}

impl LineBuffer {
    pub fn new() -> Self {
        LineBuffer {
            data: String::new(),
            cursor: 0,
        }
    }

    pub fn insert(&mut self, s: &str) {
        self.data.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn delete(&mut self) {
        if self.cursor < self.data.len() {
            let ch_len = self.char_at_cursor().map(|c| c.len_utf8()).unwrap_or(1);
            self.data.drain(self.cursor..self.cursor + ch_len);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev_pos = self.prev_char_boundary();
            self.data.drain(prev_pos..self.cursor);
            self.cursor = prev_pos;
        }
    }

    pub fn move_left(&mut self) {
        self.cursor = self.prev_char_boundary();
    }

    pub fn move_right(&mut self) {
        self.cursor = self.next_char_boundary();
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.data.len();
    }

    pub fn move_word_left(&mut self) {
        while self.cursor > 0 && self.char_before_cursor().is_some_and(is_separator) {
            self.move_left();
        }

        while self.cursor > 0 && !self.char_before_cursor().is_some_and(is_separator) {
            self.move_left();
        }
    }

    pub fn move_word_right(&mut self) {
        while self.cursor < self.data.len() && !self.char_at_cursor().is_some_and(is_separator) {
            self.move_right();
        }

        while self.cursor < self.data.len() && self.char_at_cursor().is_some_and(is_separator) {
            self.move_right();
        }
    }

    pub fn delete_word_left(&mut self) -> String {
        let start = self.cursor;
        self.move_word_left();
        let deleted = self.data[self.cursor..start].to_string();
        self.data.drain(self.cursor..start);
        deleted
    }

    pub fn delete_word_right(&mut self) -> String {
        let start = self.cursor;
        let saved_cursor = self.cursor;
        self.move_word_right();
        let end = self.cursor;
        self.cursor = saved_cursor;
        let deleted = self.data[start..end].to_string();
        self.data.drain(start..end);
        deleted
    }

    pub fn kill_to_end(&mut self) -> String {
        let killed = self.data[self.cursor..].to_string();
        self.data.truncate(self.cursor);
        killed
    }

    pub fn kill_to_start(&mut self) -> String {
        let killed = self.data[..self.cursor].to_string();
        self.data.drain(..self.cursor);
        self.cursor = 0;
        killed
    }

    pub fn as_str(&self) -> &str {
        &self.data
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn cursor_width(&self) -> usize {
        self.data[..self.cursor].width()
    }

    pub fn clear(&mut self) {
        self.data.clear();
        self.cursor = 0;
    }

    pub fn set_content(&mut self, content: &str) {
        self.data = content.to_string();
        self.cursor = self.data.len();
    }

    pub fn replace_range(&mut self, start: usize, end: usize, replacement: &str) {
        if start <= end && end <= self.data.len() {
            self.data.replace_range(start..end, replacement);

            if self.cursor >= end {
                self.cursor = self.cursor - (end - start) + replacement.len();
            } else if self.cursor > start {
                self.cursor = start + replacement.len();
            }
        }
    }

    fn prev_char_boundary(&self) -> usize {
        let mut pos = self.cursor;
        while pos > 0 && !self.data.is_char_boundary(pos) {
            pos -= 1;
        }
        if pos > 0 {
            pos = self.data[..pos]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
        pos
    }

    fn next_char_boundary(&self) -> usize {
        let mut pos = self.cursor;
        while pos < self.data.len() && !self.data.is_char_boundary(pos) {
            pos += 1;
        }
        if pos < self.data.len() {
            pos = self.data[pos..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| pos + i)
                .unwrap_or(self.data.len());
        }
        pos
    }

    fn char_at_cursor(&self) -> Option<char> {
        self.data[self.cursor..].chars().next()
    }

    fn char_before_cursor(&self) -> Option<char> {
        self.data[..self.cursor].chars().last()
    }
}
