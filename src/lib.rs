mod buffer;
mod completion;
mod display;
mod error;
mod history;
mod parser;
mod state;
mod terminal;
mod unicode;

pub use completion::{CompletionProvider, Completions, FileCompleter, NoCompleter};
use display::DisplayBuffer;
pub use error::{Result, RustlineError};
pub use history::History;

use parser::{KeySeq, Parser};
use state::EditorState;
use std::io::{self, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};
use terminal::{RawMode, clear_screen, get_terminal_size, wait_for_input};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone)]
pub struct Config {
    pub history_max_size: usize,
    pub enable_hints: bool,
    pub enable_completion: bool,
    pub enable_multiline: bool,
    pub enable_bracketed_paste: bool,
    pub balance_pairs: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            history_max_size: 1024,
            enable_hints: true,
            enable_completion: true,
            enable_multiline: true,
            enable_bracketed_paste: true,
            balance_pairs: false,
        }
    }
}

pub struct Rustline {
    config: Config,
    completer: Box<dyn CompletionProvider>,
    history: Option<History>,
}

impl Default for Rustline {
    fn default() -> Self {
        Self::new()
    }
}

impl Rustline {
    pub fn new() -> Self {
        Rustline {
            config: Config::default(),
            completer: Box::new(NoCompleter),
            history: None,
        }
    }

    pub fn with_config(config: Config) -> Self {
        Rustline {
            config,
            completer: Box::new(NoCompleter),
            history: None,
        }
    }

    pub fn set_completer(&mut self, completer: Box<dyn CompletionProvider>) {
        self.completer = completer;
    }

    pub fn set_history(&mut self, history: History) {
        self.history = Some(history);
    }

    pub fn history(&self) -> Option<&History> {
        self.history.as_ref()
    }

    pub fn history_mut(&mut self) -> Option<&mut History> {
        self.history.as_mut()
    }

    pub fn readline(&mut self, prompt: &str) -> Result<String> {
        let stdin = io::stdin().as_raw_fd();
        let stdout = io::stdout().as_raw_fd();

        if unsafe { libc::isatty(stdin) } == 0 {
            return self.read_non_interactive();
        }

        let _raw_guard = RawMode::enable(stdin)?;

        let win_size = get_terminal_size(stdin, stdout)?;

        let mut state = EditorState::new(prompt, stdin, stdout, win_size);

        if let Some(history) = self.history.take() {
            state.history = history;
        }

        if self.config.enable_bracketed_paste {
            let mut stdout_file = unsafe { std::fs::File::from_raw_fd(stdout) };
            stdout_file.write_all(b"\x1b[?2004h")?;
            let _ = stdout_file.into_raw_fd();
        }

        let result = self.edit_loop(&mut state);

        self.history = Some(state.history);

        if self.config.enable_bracketed_paste {
            let mut stdout_file = unsafe { std::fs::File::from_raw_fd(stdout) };
            stdout_file.write_all(b"\x1b[?2004l")?;
            let _ = stdout_file.into_raw_fd();
        }

        let mut stdout_file = unsafe { std::fs::File::from_raw_fd(stdout) };
        stdout_file.write_all(b"\r\n")?;
        let _ = stdout_file.into_raw_fd();

        result
    }

    fn edit_loop(&mut self, state: &mut EditorState) -> Result<String> {
        let mut parser = Parser::new();
        let mut paste_mode = false;

        self.refresh_line(state)?;

        loop {
            if !wait_for_input(state.ifd, -1)? {
                continue;
            }

            let mut buf = [0u8; 1024];
            let n = unsafe {
                let mut file = std::fs::File::from_raw_fd(state.ifd);
                let result = std::io::Read::read(&mut file, &mut buf);
                let _ = file.into_raw_fd();
                result?
            };
            if n == 0 {
                return Err(RustlineError::Eof);
            }

            let keys = parser.parse(&buf[..n]);
            if let Some(line) = self.process_keys(state, keys, &mut paste_mode)? {
                return Ok(line);
            }

            self.refresh_line(state)?;
        }
    }

    fn process_keys(
        &mut self,
        state: &mut EditorState,
        keys: Vec<KeySeq>,
        paste_mode: &mut bool,
    ) -> Result<Option<String>> {
        for key in keys {
            match key {
                KeySeq::BracketedPasteStart => *paste_mode = true,
                KeySeq::BracketedPasteEnd => *paste_mode = false,
                key if *paste_mode => self.handle_paste_char(state, key),
                key => {
                    if let Some(line) = self.handle_key(state, key)? {
                        return Ok(Some(line));
                    }
                }
            }
        }
        Ok(None)
    }

    fn handle_paste_char(&mut self, state: &mut EditorState, key: KeySeq) {
        if let KeySeq::Char(ch) = key {
            state.buffer.insert(&ch.to_string());
        }
    }

    fn handle_key(&mut self, state: &mut EditorState, key: KeySeq) -> Result<Option<String>> {
        match key {
            KeySeq::Ctrl('y') | KeySeq::Alt('y') => {}
            _ => {
                state.last_yank = None;
            }
        }

        match key {
            KeySeq::Char(ch) => {
                state.buffer.insert(&ch.to_string());
            }
            KeySeq::Ctrl('a') => state.buffer.move_home(),
            KeySeq::Ctrl('e') => state.buffer.move_end(),
            KeySeq::Ctrl('b') | KeySeq::Left => state.buffer.move_left(),
            KeySeq::Ctrl('f') | KeySeq::Right => state.buffer.move_right(),
            KeySeq::Alt('b') => state.buffer.move_word_left(),
            KeySeq::Alt('f') => state.buffer.move_word_right(),
            KeySeq::Ctrl('h') | KeySeq::Backspace => state.buffer.backspace(),
            KeySeq::Ctrl('d') | KeySeq::Delete => {
                if state.buffer.as_str().is_empty() {
                    return Err(RustlineError::Eof);
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
            KeySeq::Ctrl('w') => {
                let killed = state.buffer.delete_word_left();
                state.kill_ring.push(killed);
            }
            KeySeq::Alt('d') => {
                let killed = state.buffer.delete_word_right();
                state.kill_ring.push(killed);
            }
            KeySeq::Ctrl('y') => {
                if let Some(text) = state.kill_ring.yank() {
                    let start_pos = state.buffer.cursor();
                    let length = text.len();
                    state.buffer.insert(text);

                    state.last_yank = Some(state::YankState { start_pos, length });
                }
            }
            KeySeq::Alt('y') => {
                state.kill_ring.rotate();

                if let Some(ref yank_state) = state.last_yank {
                    if let Some(text) = state.kill_ring.yank() {
                        let start = yank_state.start_pos;
                        let end = start + yank_state.length;
                        state.buffer.replace_range(start, end, text);

                        state.last_yank = Some(state::YankState {
                            start_pos: start,
                            length: text.len(),
                        });
                    }
                }
            }
            KeySeq::Ctrl('l') => {
                clear_screen(state.ofd)?;
                self.refresh_line(state)?;
            }
            KeySeq::Ctrl('p') | KeySeq::Up => {
                if let Some(line) = state.history.previous(state.buffer.as_str()) {
                    state.buffer.set_content(line);
                }
            }
            KeySeq::Ctrl('n') | KeySeq::Down => {
                if let Some(line) = state.history.next_entry() {
                    state.buffer.set_content(line);
                }
            }
            KeySeq::Ctrl('c') => {
                return Err(RustlineError::Interrupted);
            }
            KeySeq::Enter => {
                let line = state.buffer.as_str().to_string();

                if self.config.enable_multiline && self.should_continue_multiline(&line) {
                    state.multiline_buffer.push(line);
                    state.buffer.clear();
                    state.prompt = "... ".to_string();

                    let mut buf = DisplayBuffer::new();
                    buf.push_byte(b'\r');
                    buf.push_byte(b'\n');
                    buf.flush(state.ofd)?;
                } else {
                    let final_line = if state.multiline_buffer.is_empty() {
                        line
                    } else {
                        state.multiline_buffer.push(line);
                        state.multiline_buffer.join("\n")
                    };

                    state.history.add(&final_line);

                    return Ok(Some(final_line));
                }
            }
            KeySeq::Tab => {
                if self.config.enable_completion {
                    self.handle_tab_completion(state)?;
                }
            }
            _ => {}
        }

        Ok(None)
    }

    fn handle_tab_completion(&mut self, state: &mut EditorState) -> Result<()> {
        let line = state.buffer.as_str().to_string();
        let pos = state.buffer.cursor();
        let completions = self.completer.complete(&line, pos);

        if completions.is_empty() {
            return Ok(());
        }

        let candidates = completions.candidates().to_vec();

        if candidates.len() == 1 {
            self.insert_single_completion(state, &candidates[0], &line, pos);
        } else {
            let common_len = self.find_common_prefix_length(&candidates);

            if common_len > 0 {
                let start = line[..pos]
                    .rfind(|c: char| c.is_whitespace())
                    .map(|i| i + 1)
                    .unwrap_or(0);

                let current = &line[start..pos];
                let common_prefix = &candidates[0][..common_len];

                if common_prefix.len() > current.len() {
                    let suffix = &common_prefix[current.len()..];
                    state.buffer.insert(suffix);
                    return Ok(());
                }
            }

            self.display_completion_options(state, &candidates)?;
        }

        Ok(())
    }

    fn insert_single_completion(
        &mut self,
        state: &mut EditorState,
        completion: &str,
        line: &str,
        pos: usize,
    ) {
        let start = line[..pos]
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);

        let current = &line[start..pos];
        let suffix = &completion[current.len()..];
        state.buffer.insert(suffix);
    }

    fn find_common_prefix_length(&self, candidates: &[String]) -> usize {
        let mut common_len = 0;

        let first = match candidates.first() {
            Some(first_str) => first_str,
            None => return 0,
        };

        'outer: for (i, c) in first.chars().enumerate() {
            for candidate in &candidates[1..] {
                let other_char = match candidate.chars().nth(i) {
                    Some(ch) => ch,
                    None => break 'outer,
                };

                if other_char != c {
                    break 'outer;
                }
            }
            common_len = i + 1;
        }

        common_len
    }

    fn display_completion_options(
        &self,
        state: &mut EditorState,
        candidates: &[String],
    ) -> Result<()> {
        let mut buf = DisplayBuffer::new();
        buf.push_byte(b'\r');
        buf.push_byte(b'\n');

        for candidate in candidates {
            buf.push_str(candidate);
            buf.push_str("  ");
        }

        buf.push_byte(b'\r');
        buf.push_byte(b'\n');

        buf.flush(state.ofd)?;

        self.refresh_line(state)?;

        Ok(())
    }

    fn refresh_line(&self, state: &mut EditorState) -> Result<()> {
        let mut buf = DisplayBuffer::new();

        buf.push_byte(b'\r');

        if state.display.rows_used > 1 {
            buf.move_up(state.display.rows_used - 1);
        }

        buf.push_str(&state.prompt);

        buf.push_str(state.buffer.as_str());

        buf.clear_to_eos();

        state.display.calculate_metrics(
            &state.prompt,
            state.buffer.as_str(),
            state.buffer.cursor(),
        );

        let prompt_width = state.prompt.width();
        let cursor_width = state.buffer.cursor_width();
        let abs_cursor = prompt_width + cursor_width;
        let cols = state.display.win_size.cols as usize;

        let cursor_row = abs_cursor / cols;
        let cursor_col = abs_cursor % cols;

        if state.display.rows_used > cursor_row + 1 {
            buf.move_up(state.display.rows_used - cursor_row - 1);
        }
        buf.move_to_col(cursor_col);

        buf.flush(state.ofd)?;
        Ok(())
    }

    fn should_continue_multiline(&self, line: &str) -> bool {
        if !self.config.balance_pairs {
            return false;
        }

        let mut depth = 0;
        for ch in line.chars() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
        }
        depth > 0
    }

    fn read_non_interactive(&self) -> Result<String> {
        use std::io::{BufRead, stdin};
        let stdin = stdin();
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }
        Ok(line)
    }
}

pub fn readline(prompt: &str) -> Result<String> {
    let mut rl = Rustline::new();
    rl.readline(prompt)
}

pub fn readline_with_history(prompt: &str, history_file: &str) -> Result<String> {
    let mut rl = Rustline::new();

    let mut history = History::new(rl.config.history_max_size);

    if let Err(e) = history.load(history_file) {
        match e {
            RustlineError::Io(ref io_err) if io_err.kind() != std::io::ErrorKind::NotFound => {
                return Err(e);
            }
            RustlineError::Io(_) => {}
            _ => return Err(e),
        }
    }

    rl.set_history(history);

    let line = rl.readline(prompt)?;

    if let Some(ref history) = rl.history {
        history.save(history_file)?;
    }

    Ok(line)
}
