//! Per-session editor state.

use crate::buffer::LineBuffer;
use crate::display::Renderer;
use crate::terminal::WinSize;
use std::os::unix::io::RawFd;

/// Number of kill-ring slots, matching bestline's `BESTLINE_MAX_RING`.
const RING_SIZE: usize = 8;

/// A ring of killed text, cycled through with `ALT-Y`.
#[derive(Debug, Clone)]
pub struct KillRing {
    slots: Vec<Option<String>>,
    index: usize,
}

impl Default for KillRing {
    fn default() -> Self {
        Self::new()
    }
}

impl KillRing {
    /// Creates an empty ring.
    #[must_use]
    pub fn new() -> Self {
        KillRing {
            slots: vec![None; RING_SIZE],
            index: 0,
        }
    }

    /// Stores `text` in the next slot. Empty text is ignored.
    pub fn push(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.index = (self.index + 1) % RING_SIZE;
        self.slots[self.index] = Some(text);
    }

    /// The text that `CTRL-Y` would insert.
    #[must_use]
    pub fn yank(&self) -> Option<&str> {
        self.slots[self.index].as_deref()
    }

    /// Steps back to the previous occupied slot.
    pub fn rotate(&mut self) {
        for _ in 0..RING_SIZE {
            self.index = (self.index + RING_SIZE - 1) % RING_SIZE;
            if self.slots[self.index].is_some() {
                return;
            }
        }
    }
}

/// An in-progress tab completion, so repeated `TAB` presses can cycle.
#[derive(Debug, Clone)]
pub struct CompletionCycle {
    /// Byte offset where candidates begin replacing the line.
    pub start: usize,
    /// Byte offset where the currently inserted candidate ends.
    pub end: usize,
    /// The candidates being cycled.
    pub candidates: Vec<String>,
    /// Index of the displayed candidate, or `None` before the first cycle.
    pub index: Option<usize>,
}

/// Everything that belongs to one call to `readline`.
pub struct EditorState {
    /// The line under the cursor.
    pub buffer: LineBuffer,
    /// Killed text available for yanking.
    pub kill_ring: KillRing,
    /// Draws the line and tracks where the cursor was left.
    pub renderer: Renderer,
    /// The prompt currently displayed, which changes for continuation lines.
    pub prompt: String,
    /// Prompt used for continuation lines.
    pub continuation: String,
    /// Lines already accepted as part of a multi-line entry, newline separated
    /// and ending with a newline when non-empty.
    pub accumulated: String,
    /// Position in history, where 0 is the line being typed and 1 is the
    /// newest stored entry.
    ///
    /// The line being typed deliberately lives here rather than in a scratch
    /// slot at the end of the [`crate::History`]: a slot pushed onto a full
    /// history evicts the oldest entry, and popping it again does not bring
    /// that entry back, so every line the caller chose not to store used to
    /// cost one old one.
    pub history_index: usize,
    /// The line being typed, saved while [`EditorState::history_index`] points
    /// at a stored entry.
    pub scratch: String,
    /// Byte range of the most recent yank, so `ALT-Y` can replace it.
    pub last_yank: Option<(usize, usize)>,
    /// Input descriptor.
    pub ifd: RawFd,
    /// Output descriptor.
    pub ofd: RawFd,
    /// Inside a bracketed paste.
    pub paste_mode: bool,
    /// Output is stopped by `CTRL-S`.
    pub paused: bool,
    /// Draw the line as asterisks.
    pub mask: bool,
    /// The next key is inserted literally rather than acted on.
    pub escape_next: bool,
    /// `CTRL-X` was pressed and is waiting for the second key of the chord.
    pub ctrl_x_pending: bool,
    /// State for cycling completions on repeated `TAB`.
    pub completion: Option<CompletionCycle>,
    /// The screen no longer matches the buffer and must be redrawn.
    ///
    /// Set by anything that changes what should be on screen and cleared by
    /// the renderer, so a batch of keystrokes costs one redraw rather than one
    /// per key, and a handler that already redrew is not redrawn again.
    pub dirty: bool,
}

impl EditorState {
    /// Creates state for a session on `ifd`/`ofd` with the given prompt.
    #[must_use]
    pub fn new(
        prompt: &str,
        continuation: &str,
        ifd: RawFd,
        ofd: RawFd,
        win_size: WinSize,
    ) -> Self {
        EditorState {
            buffer: LineBuffer::new(),
            kill_ring: KillRing::new(),
            renderer: Renderer::new(win_size),
            prompt: prompt.to_string(),
            continuation: continuation.to_string(),
            accumulated: String::new(),
            history_index: 0,
            scratch: String::new(),
            last_yank: None,
            ifd,
            ofd,
            paste_mode: false,
            paused: false,
            mask: false,
            escape_next: false,
            ctrl_x_pending: false,
            completion: None,
            dirty: true,
        }
    }

    /// The full text entered so far, including the line being edited.
    #[must_use]
    pub fn full_text(&self) -> String {
        let mut text = self.accumulated.clone();
        text.push_str(self.buffer.as_str());
        text
    }

    /// Moves the current line into the accumulated text and starts a new one.
    pub fn begin_continuation(&mut self) {
        self.accumulated.push_str(self.buffer.as_str());
        self.accumulated.push('\n');
        self.buffer.clear();
        self.prompt = self.continuation.clone();
        self.completion = None;
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_yanks_the_most_recent_kill() {
        let mut ring = KillRing::new();
        assert_eq!(ring.yank(), None);

        ring.push("one".into());
        assert_eq!(ring.yank(), Some("one"));
        ring.push("two".into());
        assert_eq!(ring.yank(), Some("two"));
    }

    #[test]
    fn ring_ignores_empty_kills() {
        let mut ring = KillRing::new();
        ring.push(String::new());
        assert_eq!(ring.yank(), None);
    }

    #[test]
    fn rotate_walks_backwards_through_kills() {
        let mut ring = KillRing::new();
        for text in ["a", "b", "c"] {
            ring.push(text.into());
        }
        assert_eq!(ring.yank(), Some("c"));
        ring.rotate();
        assert_eq!(ring.yank(), Some("b"));
        ring.rotate();
        assert_eq!(ring.yank(), Some("a"));
        // Wraps back around to the newest, skipping the empty slots.
        ring.rotate();
        assert_eq!(ring.yank(), Some("c"));
    }

    #[test]
    fn rotate_on_an_empty_ring_is_harmless() {
        let mut ring = KillRing::new();
        ring.rotate();
        assert_eq!(ring.yank(), None);
    }

    #[test]
    fn ring_evicts_after_eight_kills() {
        let mut ring = KillRing::new();
        for i in 0..10 {
            ring.push(format!("kill{i}"));
        }
        assert_eq!(ring.yank(), Some("kill9"));
        // Rotating all the way around returns to the newest.
        for _ in 0..RING_SIZE {
            ring.rotate();
        }
        assert_eq!(ring.yank(), Some("kill9"));
    }

    #[test]
    fn continuation_accumulates_lines() {
        let mut state = EditorState::new("> ", "... ", 0, 1, WinSize::default());
        state.buffer.insert("first");
        state.begin_continuation();
        assert_eq!(state.prompt, "... ");
        assert_eq!(state.buffer.as_str(), "");

        state.buffer.insert("second");
        assert_eq!(state.full_text(), "first\nsecond");
        assert!(state.dirty, "a new continuation line needs drawing");
    }
}
