//! The editing loop and key dispatch.
//!
//! `Rustline::edit` owns the read/dispatch/redraw cycle for one call to
//! `readline`. Keys that arrive together are all dispatched before the line is
//! redrawn, and any keys left over when a line is submitted stay queued on the
//! [`Rustline`] value so the next call consumes them rather than dropping them.

use crate::Rustline;
use crate::completion::format_columns;
use crate::display::Frame;
use crate::error::{Result, RustlineError};
use crate::parser::KeySeq;
use crate::state::{CompletionCycle, EditorState};
use crate::terminal::{
    self, RawMode, Wait, clear_screen, read_input, take_sigcont, take_sigwinch, terminal_size,
    wait_for_input, write_all,
};
use nix::sys::signal::Signal;

/// What a dispatched key did to the session.
enum Flow {
    /// Keep editing.
    Continue,
    /// The user submitted this text.
    Accept(String),
    /// End of input.
    Eof,
    /// The user pressed `CTRL-C`.
    Interrupted,
}

/// Returns whether every `(` in `text` is closed.
///
/// Unmatched `)` is ignored rather than counted negative, matching bestline's
/// `IsBalanced`, so `)(` reads as unbalanced instead of cancelling out.
fn is_balanced(text: &str) -> bool {
    let mut depth = 0usize;
    for c in text.chars() {
        if c == '(' {
            depth += 1;
        } else if c == ')' && depth > 0 {
            depth -= 1;
        }
    }
    depth == 0
}

/// Renders a key as literal text for `CTRL-Q` escaped insert.
fn escape_key(key: &KeySeq) -> String {
    fn hex(byte: u8) -> String {
        format!("\\x{byte:02x}")
    }
    match key {
        KeySeq::Char(c) => c.to_string(),
        KeySeq::Tab => "\\t".to_string(),
        KeySeq::Enter => "\\r".to_string(),
        KeySeq::LineFeed => "\\n".to_string(),
        KeySeq::Backspace => hex(0x7f),
        KeySeq::Ctrl('@') => hex(0),
        KeySeq::Ctrl(c) if c.is_ascii_lowercase() => hex(*c as u8 - b'a' + 1),
        KeySeq::Ctrl(c) => hex((*c as u8).wrapping_sub(0x40)),
        KeySeq::Alt(c) => format!("\\e{c}"),
        KeySeq::AltBackspace => format!("\\e{}", hex(0x7f)),
        KeySeq::CtrlAlt(c) if c.is_ascii_lowercase() => {
            format!("\\e{}", hex(*c as u8 - b'a' + 1))
        }
        KeySeq::Up => "\\e[A".to_string(),
        KeySeq::Down => "\\e[B".to_string(),
        KeySeq::Right => "\\e[C".to_string(),
        KeySeq::Left => "\\e[D".to_string(),
        KeySeq::Home => "\\e[H".to_string(),
        KeySeq::End => "\\e[F".to_string(),
        KeySeq::Delete => "\\e[3~".to_string(),
        other => format!("{other:?}"),
    }
}

/// Builds the `reverse-i-search` prompt, underlining the matched prefix.
fn search_prompt(failed: bool, needle: &str, matched: usize) -> String {
    let matched = matched.min(needle.len());
    format!(
        "({}reverse-i-search `\x1b[4m{}\x1b[24m{}') ",
        if failed { "failed " } else { "" },
        &needle[..matched],
        &needle[matched..],
    )
}

impl Rustline {
    /// Runs one editing session, returning the submitted text.
    pub(crate) fn edit(
        &mut self,
        state: &mut EditorState,
        raw: &mut RawMode,
        init: &str,
    ) -> Result<String> {
        // A scratch slot at the newest end of history holds the line being
        // typed, so moving up and back down restores it.
        self.history.push(String::new());
        state.history_index = 0;

        if !init.is_empty() {
            state.buffer.insert(init);
        }
        state.dirty = true;

        loop {
            while let Some(key) = self.pending.pop_front() {
                match self.dispatch(state, raw, key)? {
                    Flow::Continue => {}
                    Flow::Accept(line) => {
                        self.history.pop();
                        return Ok(line);
                    }
                    Flow::Eof => {
                        self.history.pop();
                        return Err(RustlineError::Eof);
                    }
                    Flow::Interrupted => {
                        self.history.pop();
                        return Err(RustlineError::Interrupted);
                    }
                }
            }

            if state.dirty {
                self.refresh(state, false)?;
            }

            if !self.read_more(state, raw)? {
                self.history.pop();
                // An empty buffer with no input left is end of file; otherwise
                // treat the pending text as if the user had pressed enter.
                let text = state.full_text();
                if text.is_empty() {
                    return Err(RustlineError::Eof);
                }
                return Ok(text);
            }
        }
    }

    /// Draws the current line.
    ///
    /// `finished` suppresses hints and bracket highlighting so the terminal is
    /// left showing exactly the text the caller receives.
    fn refresh(&self, state: &mut EditorState, finished: bool) -> Result<()> {
        if state.paused {
            return Ok(());
        }
        state.dirty = false;

        let hint = if self.config.enable_hints && !finished && !state.mask {
            self.hinter
                .hint(state.buffer.as_str(), state.buffer.cursor())
                .map(|h| h.render())
        } else {
            None
        };

        let highlight = if self.config.highlight_brackets && !finished && !state.mask {
            state.buffer.mirror_match()
        } else {
            None
        };

        let bytes = {
            let EditorState {
                buffer,
                renderer,
                prompt,
                mask,
                ..
            } = &mut *state;
            let mut frame = Frame::new(prompt.as_str(), buffer.as_str(), buffer.cursor());
            frame.hint = hint.as_deref();
            frame.mask = *mask;
            frame.highlight = highlight;
            frame.finished = finished;
            renderer.render(&frame)
        };
        write_all(state.ofd, &bytes)
    }

    /// Blocks until at least one key is queued.
    ///
    /// Returns `false` at end of input. Signals interrupt the wait rather than
    /// aborting it: a resize updates the geometry and redraws, then waits again.
    fn read_more(&mut self, state: &mut EditorState, raw: &mut RawMode) -> Result<bool> {
        loop {
            match wait_for_input(state.ifd, None)? {
                Wait::Ready => {}
                Wait::Timeout => continue,
                Wait::Interrupted => {
                    self.handle_signals(state, raw)?;
                    continue;
                }
            }

            let mut buf = [0u8; 1024];
            match read_input(state.ifd, &mut buf)? {
                None => {
                    self.handle_signals(state, raw)?;
                    continue;
                }
                Some(0) => return Ok(false),
                Some(n) => {
                    self.pending.extend(self.parser.parse(&buf[..n]));
                    if self.pending.is_empty() {
                        debug_assert!(
                            !self.parser.is_idle(),
                            "input produced neither a key nor a partial sequence",
                        );
                        // Only a partial escape sequence so far; wait for more.
                        continue;
                    }
                    return Ok(true);
                }
            }
        }
    }

    /// Reacts to signals that arrived while waiting for input.
    fn handle_signals(&self, state: &mut EditorState, raw: &mut RawMode) -> Result<()> {
        let mut redraw = false;

        if take_sigcont() {
            // The shell reset the terminal when it foregrounded us.
            raw.reenable()?;
            redraw = true;
        }
        if take_sigwinch() {
            state
                .renderer
                .set_win_size(terminal_size(state.ifd, state.ofd));
            redraw = true;
        }
        if redraw {
            self.refresh(state, false)?;
        }
        Ok(())
    }

    /// Returns the next key, reading more input if the queue is empty.
    fn next_key(&mut self, state: &mut EditorState, raw: &mut RawMode) -> Result<Option<KeySeq>> {
        loop {
            if let Some(key) = self.pending.pop_front() {
                return Ok(Some(key));
            }
            if !self.read_more(state, raw)? {
                return Ok(None);
            }
        }
    }

    /// Acts on one key.
    #[allow(clippy::too_many_lines)]
    fn dispatch(
        &mut self,
        state: &mut EditorState,
        raw: &mut RawMode,
        key: KeySeq,
    ) -> Result<Flow> {
        // Inside a bracketed paste, only text and newlines are honoured.
        // Interpreting pasted control characters as commands is exactly what
        // bracketed paste exists to prevent.
        state.dirty = true;

        if state.paste_mode {
            return self.dispatch_pasted(state, key);
        }

        // CTRL-Q arms a literal insert of whatever comes next.
        if std::mem::take(&mut state.escape_next) {
            let text = escape_key(&key);
            state.buffer.insert(&text);
            return Ok(Flow::Continue);
        }

        // CTRL-X is the first half of a chord. `take` must run either way, so
        // that an unrecognized second key clears the pending state and is then
        // treated normally.
        if std::mem::take(&mut state.ctrl_x_pending) && key == KeySeq::Ctrl('x') {
            state.buffer.goto_mark();
            return Ok(Flow::Continue);
        }

        // Repeated TAB cycles completions; anything else ends the cycle.
        if key != KeySeq::Tab {
            state.completion = None;
        }
        // ALT-Y only rotates a yank that CTRL-Y or ALT-Y just made.
        if !matches!(key, KeySeq::Ctrl('y') | KeySeq::Alt('y')) {
            state.last_yank = None;
        }

        match key {
            // ------------------------------------------------------ movement
            KeySeq::Ctrl('a') | KeySeq::Home => state.buffer.move_home(),
            KeySeq::Ctrl('e') | KeySeq::End => state.buffer.move_end(),
            KeySeq::Ctrl('b') | KeySeq::Left => state.buffer.move_left(),
            KeySeq::Ctrl('f') | KeySeq::Right => state.buffer.move_right(),
            KeySeq::Alt('b') => state.buffer.move_word_left(),
            KeySeq::Alt('f') => state.buffer.move_word_right(),
            KeySeq::CtrlAlt('b') | KeySeq::AltLeft => state.buffer.move_expr_left(),
            KeySeq::CtrlAlt('f') | KeySeq::AltRight => state.buffer.move_expr_right(),

            // ------------------------------------------------------- history
            KeySeq::Ctrl('p') | KeySeq::Up => self.history_move(state, 1),
            KeySeq::Ctrl('n') | KeySeq::Down => self.history_move(state, -1),
            KeySeq::Alt('<') => {
                let oldest = self.history.len().saturating_sub(1);
                self.history_goto(state, oldest);
            }
            KeySeq::Alt('>') => self.history_goto(state, 0),
            KeySeq::Ctrl('r') => {
                if let Some(key) = self.search(state, raw)? {
                    self.pending.push_front(key);
                }
            }

            // ------------------------------------------------------- deletion
            KeySeq::Ctrl('h') | KeySeq::Backspace => state.buffer.rubout(),
            KeySeq::Delete => state.buffer.delete(),
            KeySeq::Ctrl('d') => {
                if state.buffer.is_empty() {
                    return Ok(Flow::Eof);
                }
                state.buffer.delete();
            }
            KeySeq::Ctrl('k') => {
                let killed = state.buffer.kill_to_end();
                state.kill_ring.push(killed);
            }
            KeySeq::Ctrl('u') => {
                let killed = state.buffer.kill_to_start();
                state.kill_ring.push(killed);
            }
            KeySeq::Ctrl('w') | KeySeq::Alt('h') | KeySeq::CtrlAlt('h') | KeySeq::AltBackspace => {
                let killed = state.buffer.rubout_word();
                state.kill_ring.push(killed);
            }
            KeySeq::Alt('d') => {
                let killed = state.buffer.delete_word();
                state.kill_ring.push(killed);
            }

            // ---------------------------------------------------- kill ring
            KeySeq::Ctrl('y') => self.yank(state),
            KeySeq::Alt('y') => {
                if let Some((start, end)) = state.last_yank {
                    state.buffer.replace_range(start, end, "");
                    state.buffer.set_cursor(start);
                    state.kill_ring.rotate();
                    self.yank(state);
                }
            }

            // ---------------------------------------------------- transforms
            KeySeq::Ctrl('t') => state.buffer.transpose(),
            KeySeq::Alt('t') => state.buffer.transpose_words(),
            KeySeq::Alt('u') => state.buffer.uppercase_word(),
            KeySeq::Alt('l') => state.buffer.lowercase_word(),
            KeySeq::Alt('c') => state.buffer.capitalize_word(),
            KeySeq::Alt('\\') => state.buffer.squeeze(),

            // ------------------------------------------------------- paredit
            KeySeq::Alt('B') => state.buffer.barf(),
            KeySeq::Alt('S') => state.buffer.slurp(),
            // Bestline defines raise as a no-op; kept so the binding is inert
            // rather than falling through to "unknown key".
            KeySeq::Alt('R') => {}

            // ---------------------------------------------------------- mark
            KeySeq::Ctrl('@') => state.buffer.set_mark(),
            KeySeq::Ctrl('x') => state.ctrl_x_pending = true,

            // -------------------------------------------------------- screen
            KeySeq::Ctrl('l') => {
                clear_screen(state.ofd)?;
                state.renderer.reset();
            }

            // ------------------------------------------------------- process
            KeySeq::Ctrl('c') => return Ok(Flow::Interrupted),
            KeySeq::Ctrl('\\') => {
                raw.disable()?;
                terminal::raise_default(Signal::SIGQUIT)?;
            }
            KeySeq::Ctrl('z') => {
                raw.disable()?;
                write_all(state.ofd, b"\r\n")?;
                terminal::suspend()?;
                raw.reenable()?;
                state.renderer.reset();
            }
            KeySeq::Ctrl('s') => {
                terminal::pause_output(state.ofd)?;
                state.paused = true;
            }
            KeySeq::Ctrl('q') => {
                if state.paused {
                    terminal::resume_output(state.ofd)?;
                    state.paused = false;
                } else {
                    state.escape_next = true;
                }
            }

            // ---------------------------------------------------- submission
            KeySeq::Enter => return self.accept(state),
            KeySeq::LineFeed => self.continue_line(state)?,

            // ---------------------------------------------------- completion
            KeySeq::Tab => {
                if self.config.enable_completion {
                    self.complete(state)?;
                } else {
                    state.buffer.insert_char('\t');
                }
            }

            // --------------------------------------------------------- paste
            KeySeq::BracketedPasteStart => state.paste_mode = true,
            KeySeq::BracketedPasteEnd => state.paste_mode = false,

            // ---------------------------------------------------------- text
            KeySeq::Char(c) => {
                let c = self.xlat.as_ref().map_or(c, |f| f(c));
                state.buffer.insert_char(c);
            }

            // Unbound keys are ignored, never inserted as garbage.
            _ => {}
        }

        Ok(Flow::Continue)
    }

    /// Handles a key that arrived inside a bracketed paste.
    fn dispatch_pasted(&mut self, state: &mut EditorState, key: KeySeq) -> Result<Flow> {
        match key {
            KeySeq::BracketedPasteEnd => state.paste_mode = false,
            KeySeq::Char(c) => state.buffer.insert_char(c),
            KeySeq::Tab => state.buffer.insert_char('\t'),
            // A newline in pasted text is a real newline, not a submission.
            KeySeq::Enter | KeySeq::LineFeed => self.continue_line(state)?,
            _ => {}
        }
        Ok(Flow::Continue)
    }

    /// Handles `ENTER`: submit, unless the line is still open.
    fn accept(&mut self, state: &mut EditorState) -> Result<Flow> {
        let finished = !self.config.balance_pairs || is_balanced(&state.full_text());
        if !finished {
            self.continue_line(state)?;
            return Ok(Flow::Continue);
        }

        state.buffer.move_end();
        self.refresh(state, true)?;
        Ok(Flow::Accept(state.full_text()))
    }

    /// Ends the current line and starts a continuation line.
    fn continue_line(&mut self, state: &mut EditorState) -> Result<()> {
        state.buffer.move_end();
        self.refresh(state, true)?;
        let bytes = state.renderer.finish();
        write_all(state.ofd, &bytes)?;
        state.begin_continuation();
        Ok(())
    }

    /// Inserts the current kill-ring entry and records its extent.
    fn yank(&mut self, state: &mut EditorState) {
        let Some(text) = state.kill_ring.yank() else {
            return;
        };
        let text = text.to_string();
        let start = state.buffer.cursor();
        state.buffer.insert(&text);
        state.last_yank = Some((start, start + text.len()));
    }

    // ------------------------------------------------------------- history

    /// Converts a newest-relative history index to a storage slot.
    fn slot_of(&self, index: usize) -> Option<usize> {
        self.history.len().checked_sub(1)?.checked_sub(index)
    }

    /// Converts a storage slot back to a newest-relative index.
    fn index_of(&self, slot: usize) -> usize {
        self.history.len().saturating_sub(1).saturating_sub(slot)
    }

    /// Moves to history entry `index`, counted from the newest.
    ///
    /// The working buffer is written back into the entry being left, so an edit
    /// to a recalled line survives moving away and coming back.
    fn history_goto(&mut self, state: &mut EditorState, index: usize) {
        if self.history.len() <= 1 || index > self.history.len() - 1 {
            return;
        }
        if let Some(slot) = self.slot_of(state.history_index) {
            self.history.set(slot, state.buffer.as_str());
        }
        state.history_index = index;
        if let Some(slot) = self.slot_of(index) {
            if let Some(line) = self.history.get(slot) {
                state.buffer.set_content(line);
            }
        }
    }

    /// Moves `delta` entries through history; positive is older.
    fn history_move(&mut self, state: &mut EditorState, delta: isize) {
        let Some(index) = state.history_index.checked_add_signed(delta) else {
            return;
        };
        self.history_goto(state, index);
    }

    // -------------------------------------------------------------- search

    /// Runs an incremental reverse history search.
    ///
    /// Returns the key that ended the search, for the caller to re-dispatch.
    fn search(&mut self, state: &mut EditorState, raw: &mut RawMode) -> Result<Option<KeySeq>> {
        if self.history.len() <= 1 {
            return Ok(None);
        }

        let saved_prompt = std::mem::take(&mut state.prompt);
        let saved_pos = state.buffer.cursor();
        let saved_index = state.history_index;

        let mut needle = String::new();
        let mut matched = 0usize;
        let mut failed = false;
        let mut terminator = None;

        loop {
            state.prompt = search_prompt(failed, &needle, matched);
            self.refresh(state, false)?;

            let Some(key) = self.next_key(state, raw)? else {
                break;
            };

            // Where to resume scanning: the entry we are on and how far into it.
            let mut slot = self.slot_of(state.history_index).unwrap_or(0);
            let mut limit = state.buffer.cursor();
            let mut added = 0usize;

            match key {
                KeySeq::Backspace | KeySeq::Ctrl('h') => {
                    needle.pop();
                    matched = matched.min(needle.len());
                }
                KeySeq::Ctrl('r') => {
                    // Step past the current match to find an earlier one.
                    if limit > 0 {
                        limit -= 1;
                    } else if let Some(older) = slot.checked_sub(1) {
                        slot = older;
                        limit = usize::MAX;
                    }
                }
                KeySeq::Ctrl('g') => {
                    self.history_goto(state, saved_index);
                    state.buffer.set_cursor(saved_pos);
                    break;
                }
                KeySeq::Char(c) => {
                    needle.push(c);
                    added = c.len_utf8();
                }
                other => {
                    terminator = Some(other);
                    break;
                }
            }

            if needle.is_empty() {
                failed = false;
                continue;
            }

            let limit = limit.saturating_add(needle.len());
            match self.history.search_backward(&needle, slot, limit) {
                Some((found, offset)) => {
                    let index = self.index_of(found);
                    self.history_goto(state, index);
                    state.buffer.set_cursor(offset);
                    matched += added;
                    failed = false;
                }
                None => failed = true,
            }
        }

        state.prompt = saved_prompt;
        self.refresh(state, false)?;
        Ok(terminator)
    }

    // ---------------------------------------------------------- completion

    /// Handles `TAB`.
    fn complete(&mut self, state: &mut EditorState) -> Result<()> {
        let pos = state.buffer.cursor();

        // A second TAB with the line untouched cycles to the next candidate.
        if let Some(cycle) = state.completion.take() {
            if cycle.end == pos && !cycle.candidates.is_empty() {
                let next = cycle.index.map_or(0, |i| (i + 1) % cycle.candidates.len());
                let candidate = cycle.candidates[next].clone();
                state
                    .buffer
                    .replace_range(cycle.start, cycle.end, &candidate);
                state.completion = Some(CompletionCycle {
                    start: cycle.start,
                    end: cycle.start + candidate.len(),
                    candidates: cycle.candidates,
                    index: Some(next),
                });
                return Ok(());
            }
        }

        let line = state.buffer.as_str().to_string();
        let completions = self.completer.complete(&line, pos);
        if completions.is_empty() {
            return Ok(());
        }

        let start = completions.start();
        // A provider that reports a range outside the line is a bug in the
        // provider; ignore it rather than slicing at an invalid offset.
        if start > pos || !line.is_char_boundary(start) {
            return Ok(());
        }
        let typed = &line[start..pos];
        let candidates = completions.candidates().to_vec();

        if candidates.len() == 1 {
            state.buffer.replace_range(start, pos, &candidates[0]);
            return Ok(());
        }

        let common = completions.common_prefix_len();
        let prefix = &candidates[0][..common];
        if common > typed.len() && prefix.starts_with(typed) {
            let prefix = prefix.to_string();
            state.buffer.replace_range(start, pos, &prefix);
            state.completion = Some(CompletionCycle {
                start,
                end: start + prefix.len(),
                candidates,
                index: None,
            });
            return Ok(());
        }

        // Nothing more to insert: show what the choices are.
        self.list_completions(state, &candidates)?;
        state.completion = Some(CompletionCycle {
            start,
            end: pos,
            candidates,
            index: None,
        });
        Ok(())
    }

    /// Prints the candidate list below the prompt and redraws.
    fn list_completions(&self, state: &mut EditorState, candidates: &[String]) -> Result<()> {
        let width = state.renderer.win_size().cols();
        let mut out = state.renderer.finish();
        for line in format_columns(candidates, width) {
            out.extend_from_slice(line.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        write_all(state.ofd, &out)?;
        self.refresh(state, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_matches_bestline() {
        assert!(is_balanced(""));
        assert!(is_balanced("(a)"));
        assert!(is_balanced("((a))"));
        assert!(!is_balanced("(a"));
        assert!(is_balanced("a)"));
        // Unmatched ")" must not cancel a later "(": bestline clamps at zero.
        assert!(!is_balanced(")("));
        assert!(is_balanced("(a\nb)"));
    }

    #[test]
    fn escaped_keys_are_readable() {
        assert_eq!(escape_key(&KeySeq::Char('a')), "a");
        assert_eq!(escape_key(&KeySeq::Ctrl('a')), "\\x01");
        assert_eq!(escape_key(&KeySeq::Ctrl('@')), "\\x00");
        assert_eq!(escape_key(&KeySeq::Tab), "\\t");
        assert_eq!(escape_key(&KeySeq::Enter), "\\r");
        assert_eq!(escape_key(&KeySeq::Backspace), "\\x7f");
        assert_eq!(escape_key(&KeySeq::Alt('x')), "\\ex");
        assert_eq!(escape_key(&KeySeq::Up), "\\e[A");
    }

    #[test]
    fn search_prompt_underlines_the_match() {
        let p = search_prompt(false, "git", 3);
        assert!(p.contains("reverse-i-search"));
        assert!(p.contains("\x1b[4mgit\x1b[24m"));
        assert!(!p.contains("failed"));

        let p = search_prompt(true, "gitx", 3);
        assert!(p.contains("failed"));
        assert!(p.contains("\x1b[4mgit\x1b[24mx"));
    }

    #[test]
    fn search_prompt_handles_multibyte_needles() {
        // `matched` is always a character boundary, so slicing is safe.
        let p = search_prompt(false, "日本", "日".len());
        assert!(p.contains("\x1b[4m日\x1b[24m本"));
    }
}
