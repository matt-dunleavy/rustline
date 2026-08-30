use crate::error::Result;
use crate::terminal::WinSize;
use std::io::Write;
use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};
use unicode_width::UnicodeWidthStr;

pub struct DisplayBuffer {
    buffer: Vec<u8>,
}

impl DisplayBuffer {
    pub fn new() -> Self {
        DisplayBuffer {
            buffer: Vec::with_capacity(1024),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub fn push_str(&mut self, s: &str) {
        self.push(s.as_bytes());
    }

    pub fn push_byte(&mut self, b: u8) {
        self.buffer.push(b);
    }

    pub fn push_esc(&mut self, seq: &str) {
        self.push_byte(0x1b);
        self.push_str(seq);
    }

    pub fn move_to_col(&mut self, col: usize) {
        if col == 0 {
            self.push_byte(b'\r');
        } else {
            self.push_str(&format!("\r\x1b[{}C", col));
        }
    }

    pub fn clear_to_eos(&mut self) {
        self.push_esc("[J");
    }

    pub fn move_up(&mut self, n: usize) {
        if n > 0 {
            self.push_str(&format!("\x1b[{}A", n));
        }
    }

    pub fn flush(&mut self, fd: RawFd) -> Result<()> {
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };

        file.write_all(&self.buffer)?;

        let _ = file.into_raw_fd();

        self.buffer.clear();
        Ok(())
    }
}

pub struct Display {
    pub win_size: WinSize,
    pub rows_used: usize,
    pub cursor_row: usize,
}

impl Display {
    pub fn new(win_size: WinSize) -> Self {
        Display {
            win_size,
            rows_used: 1,
            cursor_row: 0,
        }
    }

    pub fn calculate_metrics(&mut self, prompt: &str, line: &str, cursor_pos: usize) {
        let prompt_width = prompt.width();
        let line_width = line.width();
        let cursor_width = line[..cursor_pos].width();

        let cols = self.win_size.cols as usize;

        self.rows_used = ((prompt_width + line_width) / cols) + 1;

        let abs_cursor = prompt_width + cursor_width;
        self.cursor_row = self.rows_used - (abs_cursor / cols) - 1;
    }
}
