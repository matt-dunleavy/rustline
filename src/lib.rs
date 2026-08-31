#![warn(missing_docs)]
#![warn(clippy::doc_markdown)]

//! A line editor for interactive terminal programs.
//!
//! Rustline is a Rust port of [bestline], which is itself a fork of linenoise.
//! It offers Emacs-style editing, reverse history search, completion, hints and
//! UTF-8 editing over ANSI X3.64 escape sequences, with no terminfo dependency.
//!
//! # Quick start
//!
//! ```no_run
//! # fn main() -> rustline::Result<()> {
//! let mut rl = rustline::Rustline::new();
//! loop {
//!     match rl.readline("> ") {
//!         Ok(line) => {
//!             rl.add_history_entry(&line);
//!             println!("{line}");
//!         }
//!         Err(rustline::RustlineError::Interrupted) => continue,
//!         Err(rustline::RustlineError::Eof) => break,
//!         Err(e) => return Err(e),
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Key bindings
//!
//! ```text
//! CTRL-A / HOME     start of line        CTRL-T          transpose chars
//! CTRL-E / END      end of line          ALT-T           transpose words
//! CTRL-B / LEFT     back one char        ALT-U           uppercase word
//! CTRL-F / RIGHT    forward one char     ALT-L           lowercase word
//! ALT-B             back one word        ALT-C           capitalize word
//! ALT-F             forward one word     ALT-\           squeeze whitespace
//! ALT-LEFT          back one expr        CTRL-K          kill to end
//! ALT-RIGHT         forward one expr     CTRL-U          kill to start
//! CTRL-P / UP       previous history     CTRL-W / ALT-H  kill word backwards
//! CTRL-N / DOWN     next history         ALT-D           kill word forwards
//! ALT-<             oldest history       CTRL-Y          yank
//! ALT->             newest history       ALT-Y           rotate ring and yank
//! CTRL-R            search history       CTRL-SPACE      set mark
//! CTRL-G            cancel search        CTRL-X CTRL-X   go to mark
//! CTRL-H / BKSP     backspace            CTRL-L          clear screen
//! CTRL-D            delete, or EOF       CTRL-C          interrupt
//! TAB               complete             CTRL-Z          suspend
//! ALT-SHIFT-B       barf expression      CTRL-\          quit
//! ALT-SHIFT-S       slurp expression     CTRL-S / CTRL-Q flow control
//! CTRL-J            new line             CTRL-Q          escaped insert
//! ```
//!
//! [bestline]: https://github.com/jart/bestline

mod buffer;
mod completion;
mod display;
mod edit;
mod error;
mod hints;
mod history;
mod parser;
mod state;
mod terminal;
mod unicode;

pub use completion::{
    CommandCompleter, CompletionProvider, Completions, FileCompleter, NoCompleter, format_columns,
    word_start,
};
pub use error::{Result, RustlineError};
pub use hints::{Hint, HintProvider, NoHinter};
pub use history::History;
pub use terminal::WinSize;
pub use unicode::{char_width, display_width, is_separator};

use parser::{KeySeq, Parser};
use state::EditorState;
use std::collections::VecDeque;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::PathBuf;
use terminal::{RawMode, is_tty, is_unsupported_term, read_input, terminal_size, write_all};

/// Editor behaviour that a caller may want to change.
///
/// Construct with [`Config::default`] and override what you need, so that
/// fields added in future releases keep compiling:
///
/// ```
/// let config = rustline::Config {
///     balance_pairs: true,
///     ..Default::default()
/// };
/// # assert!(config.balance_pairs);
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    /// Maximum number of history entries retained.
    pub history_max_size: usize,
    /// Draw hints supplied by the [`HintProvider`].
    pub enable_hints: bool,
    /// Let `TAB` run the [`CompletionProvider`]. When disabled, `TAB` inserts a
    /// literal tab character.
    pub enable_completion: bool,
    /// Allow an entry to span several lines. When disabled, `CTRL-J` submits
    /// the line like `ENTER`, [`Config::balance_pairs`] has no effect, and a
    /// newline inside a bracketed paste becomes a space.
    pub enable_multiline: bool,
    /// Ask the terminal to bracket pasted text, so a paste containing newlines
    /// or control characters is inserted rather than executed.
    pub enable_bracketed_paste: bool,
    /// Keep reading continuation lines until every `(` is closed.
    pub balance_pairs: bool,
    /// Embolden the bracket matching the one at the cursor.
    pub highlight_brackets: bool,
    /// Draw the line as asterisks, for password entry.
    pub mask_mode: bool,
    /// Prompt used for continuation lines.
    pub continuation_prompt: String,
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
            highlight_brackets: true,
            mask_mode: false,
            continuation_prompt: "... ".to_string(),
        }
    }
}

/// A line editor.
///
/// One instance holds the history, the completion and hint providers, and any
/// input left over from a previous call, so keys typed ahead are never lost
/// between calls to [`Rustline::readline`].
pub struct Rustline {
    pub(crate) config: Config,
    pub(crate) completer: Box<dyn CompletionProvider>,
    pub(crate) hinter: Box<dyn HintProvider>,
    pub(crate) xlat: Option<Box<dyn Fn(char) -> char>>,
    pub(crate) history: History,
    /// Retains a partially received escape sequence across reads.
    pub(crate) parser: Parser,
    /// Keys read but not yet acted on, including any that arrived in the same
    /// batch as a submitted line.
    pub(crate) pending: VecDeque<KeySeq>,
}

impl Default for Rustline {
    fn default() -> Self {
        Self::new()
    }
}

impl Rustline {
    /// Creates an editor with the default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(Config::default())
    }

    /// Creates an editor with the given configuration.
    #[must_use]
    pub fn with_config(config: Config) -> Self {
        let history = History::new(config.history_max_size);
        Rustline {
            config,
            completer: Box::new(NoCompleter),
            hinter: Box::new(NoHinter),
            xlat: None,
            history,
            parser: Parser::new(),
            pending: VecDeque::new(),
        }
    }

    /// The current configuration.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Replaces the configuration.
    ///
    /// Changing [`Config::history_max_size`] resizes the history, keeping the
    /// newest entries.
    pub fn set_config(&mut self, config: Config) {
        if config.history_max_size != self.history.max_size() {
            let mut resized = History::new(config.history_max_size);
            for entry in self.history.iter() {
                resized.push(entry.to_string());
            }
            self.history = resized;
        }
        self.config = config;
    }

    /// Installs the completion provider used by `TAB`.
    pub fn set_completer(&mut self, completer: Box<dyn CompletionProvider>) {
        self.completer = completer;
    }

    /// Installs the hint provider.
    pub fn set_hinter(&mut self, hinter: Box<dyn HintProvider>) {
        self.hinter = hinter;
    }

    /// Installs a transliteration function applied to every typed character.
    ///
    /// This is the hook for input methods that map one keyboard to another
    /// script, such as typing Russian on a Latin keyboard.
    pub fn set_xlat(&mut self, xlat: Box<dyn Fn(char) -> char>) {
        self.xlat = Some(xlat);
    }

    /// Removes any transliteration function.
    pub fn clear_xlat(&mut self) {
        self.xlat = None;
    }

    /// The history.
    #[must_use]
    pub fn history(&self) -> &History {
        &self.history
    }

    /// The history, mutably.
    pub fn history_mut(&mut self) -> &mut History {
        &mut self.history
    }

    /// Replaces the history.
    pub fn set_history(&mut self, history: History) {
        self.history = history;
    }

    /// Appends `line` to the history unless it repeats the previous entry.
    ///
    /// Returns whether an entry was added.
    pub fn add_history_entry(&mut self, line: &str) -> bool {
        self.history.add(line)
    }

    /// Loads history from `path`, replacing what is in memory.
    ///
    /// A missing file is not an error.
    pub fn load_history(&mut self, path: impl AsRef<std::path::Path>) -> Result<()> {
        self.history.load(path)
    }

    /// Writes history to `path` with mode `0600`.
    pub fn save_history(&self, path: impl AsRef<std::path::Path>) -> Result<()> {
        self.history.save(path)
    }

    /// Reads one line from the terminal.
    pub fn readline(&mut self, prompt: &str) -> Result<String> {
        self.readline_with_init(prompt, "")
    }

    /// Reads one line, pre-filling the buffer with `init`.
    ///
    /// The initial text is ignored when input is not a terminal.
    pub fn readline_with_init(&mut self, prompt: &str, init: &str) -> Result<String> {
        let ifd = std::io::stdin().as_raw_fd();
        let ofd = std::io::stdout().as_raw_fd();
        self.readline_raw(prompt, init, ifd, ofd)
    }

    /// Reads one line as a masked password, without recording it in history.
    ///
    /// Completion and history search are both suppressed while the input is
    /// masked, since either would print what was typed in plaintext.
    ///
    /// Two limits are worth knowing about. The password is an ordinary
    /// `String` and is never zeroized, so it stays in the heap until the
    /// allocator reuses that memory; use a crate such as `zeroize` if that
    /// matters. And [`Config::mask_mode`] is restored on the normal path only,
    /// so if the editor panics the value is left in mask mode.
    pub fn read_password(&mut self, prompt: &str) -> Result<String> {
        let was_masked = self.config.mask_mode;
        self.config.mask_mode = true;
        let result = self.readline(prompt);
        self.config.mask_mode = was_masked;
        result
    }

    /// Reads one line from explicit descriptors.
    ///
    /// Falls back to plain line reading when either descriptor is not a
    /// terminal, or when `$TERM` names a terminal that cannot render escape
    /// sequences.
    pub fn readline_raw(
        &mut self,
        prompt: &str,
        init: &str,
        ifd: RawFd,
        ofd: RawFd,
    ) -> Result<String> {
        // The editor writes to the descriptor directly, bypassing the buffer
        // behind `std::io::stdout()`. Anything the caller printed without a
        // trailing newline is still sitting in that buffer, so flush it now or
        // it will surface later, interleaved with a redraw. bestline does the
        // same before entering raw mode.
        let _ = std::io::Write::flush(&mut std::io::stdout());

        if !is_tty(ifd) || !is_tty(ofd) || is_unsupported_term() {
            if !prompt.is_empty() && is_tty(ofd) {
                write_all(ofd, prompt.as_bytes())?;
            }
            return read_plain_line(ifd);
        }

        let mut raw = RawMode::enable(ifd)?;
        let win = terminal_size(ifd, ofd);
        let mut state = EditorState::new(prompt, &self.config.continuation_prompt, ifd, ofd, win);
        state.mask = self.config.mask_mode;

        if self.config.enable_bracketed_paste {
            // A terminal that does not understand this simply ignores it, and a
            // failure here must not cost the caller their line.
            let _ = write_all(ofd, b"\x1b[?2004h");
        }

        let result = self.edit(&mut state, &mut raw, init);

        if self.config.enable_bracketed_paste {
            let _ = write_all(ofd, b"\x1b[?2004l");
        }
        if state.paused {
            let _ = terminal::resume_output(ofd);
        }
        let trailing = state.renderer.finish();
        let _ = write_all(ofd, &trailing);

        result
    }
}

/// Clears the terminal and moves the cursor to the top left.
///
/// Prefer this to printing the escape sequence yourself: it writes straight to
/// the descriptor, so it cannot be left sitting in the buffer behind
/// [`std::io::stdout`] the way a `print!` without a trailing newline would be.
///
/// This is what an application's own `clear` command should call. It is
/// unrelated to `CTRL-L`, which the editor handles internally.
pub fn clear_screen() -> Result<()> {
    let ofd = std::io::stdout().as_raw_fd();
    terminal::clear_screen(ofd)
}

/// Reads bytes from `fd` up to the next newline.
///
/// Reads one byte at a time so that no input beyond the newline is consumed,
/// which matters when the caller goes on to read the same descriptor itself.
fn read_plain_line(fd: RawFd) -> Result<String> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    let mut saw_newline = false;

    loop {
        match read_input(fd, &mut byte)? {
            // Interrupted by a signal; try again.
            None => continue,
            Some(0) => break,
            Some(_) => {
                if byte[0] == b'\n' {
                    saw_newline = true;
                    break;
                }
                bytes.push(byte[0]);
            }
        }
    }

    // A CRLF line ending leaves the carriage return behind.
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }

    // A blank line is an empty string; only running out of input without
    // seeing a newline at all is end of file.
    if bytes.is_empty() && !saw_newline {
        return Err(RustlineError::Eof);
    }

    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Reads one line using a temporary editor.
///
/// Convenient for one-off prompts; use [`Rustline`] directly when you want
/// history or completion.
pub fn readline(prompt: &str) -> Result<String> {
    Rustline::new().readline(prompt)
}

/// The current user's home directory.
///
/// `$HOME` when it is set and non-empty, falling back to the passwd entry that
/// a login shell would have taken `$HOME` from in the first place.
///
/// This is deliberately not the `dirs` crate. Its only use here was this one
/// function, and it reached it through `dirs-sys` and the MPL-2.0 `option-ext`,
/// which put an MPL entry into the licence audit of every crate that depends on
/// rustline. `nix` is already a dependency and answers the same question.
pub(crate) fn home_dir() -> Option<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => Some(PathBuf::from(home)),
        _ => nix::unistd::User::from_uid(nix::unistd::Uid::current())
            .ok()
            .flatten()
            .map(|user| user.dir),
    }
}

/// Derives the conventional history path for a program name.
///
/// A `prog` containing `/` or `.` is taken to be the path itself; otherwise the
/// result is `~/.{prog}_history`, as bestline does.
#[must_use]
pub fn history_path(prog: &str) -> PathBuf {
    if prog.contains('/') || prog.contains('.') {
        return PathBuf::from(prog);
    }
    let name = format!(".{prog}_history");
    match home_dir() {
        Some(home) => home.join(name),
        None => PathBuf::from(name),
    }
}

/// Reads one line, loading and saving history at `~/.{prog}_history`.
///
/// History is reloaded immediately before saving, so several sessions of the
/// same program running at once each pick up the others' entries instead of
/// overwriting them.
pub fn readline_with_history(prompt: &str, prog: &str) -> Result<String> {
    let path = history_path(prog);
    let mut rl = Rustline::new();
    rl.load_history(&path)?;

    let line = rl.readline(prompt)?;

    if !line.is_empty() {
        rl.load_history(&path)?;
        rl.add_history_entry(&line);
        rl.save_history(&path)?;
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_sane() {
        let c = Config::default();
        assert_eq!(c.history_max_size, 1024);
        assert!(c.enable_completion);
        assert!(!c.balance_pairs);
        assert_eq!(c.continuation_prompt, "... ");
    }

    #[test]
    fn config_history_size_is_honoured() {
        // The old implementation hardcoded 1024 regardless of the config.
        let rl = Rustline::with_config(Config {
            history_max_size: 7,
            ..Config::default()
        });
        assert_eq!(rl.history().max_size(), 7);
    }

    #[test]
    fn resizing_history_keeps_the_newest_entries() {
        let mut rl = Rustline::new();
        for i in 0..10 {
            rl.add_history_entry(&format!("line{i}"));
        }
        rl.set_config(Config {
            history_max_size: 3,
            ..Config::default()
        });
        assert_eq!(rl.history().len(), 3);
        assert_eq!(rl.history().get(2), Some("line9"));
    }

    #[test]
    fn history_entries_round_trip_through_the_api() {
        let mut rl = Rustline::new();
        assert!(rl.add_history_entry("one"));
        assert!(!rl.add_history_entry("one"));
        assert!(rl.add_history_entry("two"));
        assert_eq!(rl.history().iter().collect::<Vec<_>>(), ["one", "two"]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h");
        rl.save_history(&path).unwrap();

        let mut other = Rustline::new();
        other.load_history(&path).unwrap();
        assert_eq!(other.history().iter().collect::<Vec<_>>(), ["one", "two"]);
    }

    #[test]
    fn home_dir_resolves_to_an_absolute_path() {
        // Deliberately no `set_var` here: mutating the environment races every
        // other thread that reads it, which is why `is_unsupported_term_name`
        // was split out. Whatever the environment holds, the answer must be
        // absolute, because it gets a file name joined onto it.
        if let Some(home) = home_dir() {
            assert!(home.is_absolute(), "home_dir returned {home:?}");
        }
    }

    #[test]
    fn history_path_rules() {
        assert_eq!(history_path("/tmp/h"), PathBuf::from("/tmp/h"));
        assert_eq!(history_path("h.txt"), PathBuf::from("h.txt"));
        let derived = history_path("foo");
        assert!(derived.to_string_lossy().ends_with(".foo_history"));
    }

    #[test]
    fn plain_line_reading_from_a_pipe() {
        use std::io::Write;
        use std::os::fd::AsRawFd;

        let (r, mut w) = {
            let (r, w) = nix::unistd::pipe().unwrap();
            (r, std::fs::File::from(w))
        };
        w.write_all(b"hello\nworld\n").unwrap();
        drop(w);

        assert_eq!(read_plain_line(r.as_raw_fd()).unwrap(), "hello");
        assert_eq!(read_plain_line(r.as_raw_fd()).unwrap(), "world");
        assert!(matches!(
            read_plain_line(r.as_raw_fd()),
            Err(RustlineError::Eof)
        ));
    }

    #[test]
    fn plain_line_reading_treats_a_blank_line_as_empty_not_eof() {
        use std::io::Write;
        use std::os::fd::AsRawFd;

        let (r, w) = nix::unistd::pipe().unwrap();
        let mut w = std::fs::File::from(w);
        w.write_all(b"\n\n").unwrap();
        drop(w);

        assert_eq!(read_plain_line(r.as_raw_fd()).unwrap(), "");
        assert_eq!(read_plain_line(r.as_raw_fd()).unwrap(), "");
        assert!(matches!(
            read_plain_line(r.as_raw_fd()),
            Err(RustlineError::Eof)
        ));
    }

    #[test]
    fn plain_line_reading_strips_crlf_and_handles_a_missing_newline() {
        use std::io::Write;
        use std::os::fd::AsRawFd;

        let (r, w) = nix::unistd::pipe().unwrap();
        let mut w = std::fs::File::from(w);
        w.write_all(b"windows\r\nno-newline").unwrap();
        drop(w);

        assert_eq!(read_plain_line(r.as_raw_fd()).unwrap(), "windows");
        assert_eq!(read_plain_line(r.as_raw_fd()).unwrap(), "no-newline");
    }

    #[test]
    fn readline_falls_back_when_input_is_not_a_terminal() {
        use std::io::Write;
        use std::os::fd::AsRawFd;

        let (r, w) = nix::unistd::pipe().unwrap();
        let mut w = std::fs::File::from(w);
        w.write_all(b"piped input\n").unwrap();
        drop(w);

        let mut rl = Rustline::new();
        let line = rl
            .readline_raw("> ", "", r.as_raw_fd(), std::io::stdout().as_raw_fd())
            .unwrap();
        assert_eq!(line, "piped input");
    }

    #[test]
    fn xlat_is_installed_and_cleared() {
        let mut rl = Rustline::new();
        rl.set_xlat(Box::new(|c| c.to_ascii_uppercase()));
        assert!(rl.xlat.is_some());
        rl.clear_xlat();
        assert!(rl.xlat.is_none());
    }
}
