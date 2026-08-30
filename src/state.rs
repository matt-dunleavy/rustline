use crate::buffer::LineBuffer;
use crate::display::Display;
use crate::history::History;
use crate::terminal::WinSize;
use std::os::unix::io::RawFd;

pub struct KillRing {
    entries: Vec<String>,
    current: usize,
    max_size: usize,
}

impl KillRing {
    pub fn new(max_size: usize) -> Self {
        KillRing {
            entries: Vec::new(),
            current: 0,
            max_size,
        }
    }

    pub fn push(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.entries.push(text);
        if self.entries.len() > self.max_size {
            self.entries.remove(0);
        }
        self.current = self.entries.len().saturating_sub(1);
    }

    pub fn yank(&self) -> Option<&str> {
        self.entries.get(self.current).map(|s| s.as_str())
    }

    pub fn rotate(&mut self) {
        if !self.entries.is_empty() {
            self.current = (self.current + self.entries.len() - 1) % self.entries.len();
        }
    }
}

pub struct YankState {
    pub start_pos: usize,
    pub length: usize,
}

pub struct EditorState {
    pub buffer: LineBuffer,
    pub history: History,
    pub kill_ring: KillRing,
    pub display: Display,
    pub prompt: String,
    pub ifd: RawFd,
    pub ofd: RawFd,
    pub multiline_buffer: Vec<String>,
    pub last_yank: Option<YankState>,
}

impl EditorState {
    pub fn new(prompt: &str, ifd: RawFd, ofd: RawFd, win_size: WinSize) -> Self {
        EditorState {
            buffer: LineBuffer::new(),
            history: History::new(1024),
            kill_ring: KillRing::new(8),
            display: Display::new(win_size),
            prompt: prompt.to_string(),
            ifd,
            ofd,
            multiline_buffer: Vec::new(),
            last_yank: None,
        }
    }
}
