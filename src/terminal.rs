//! Terminal control: raw mode, signals, window size, and interruptible I/O.

use crate::error::{Result, RustlineError};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::termios::{self, Termios};
use nix::unistd;
use std::os::unix::io::{BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the `SIGWINCH` handler when the terminal is resized.
static GOT_SIGWINCH: AtomicBool = AtomicBool::new(false);
/// Set by the `SIGCONT` handler when the process is foregrounded.
static GOT_SIGCONT: AtomicBool = AtomicBool::new(false);

/// Terminals that cannot handle the escape sequences this crate emits.
const UNSUPPORTED_TERMS: &[&str] = &["dumb", "cons25", "emacs"];

/// Fallback size when the terminal will not report its own.
const DEFAULT_ROWS: u16 = 24;
/// Fallback width when the terminal will not report its own.
const DEFAULT_COLS: u16 = 80;

/// Size of the terminal window in character cells.
///
/// Both fields are guaranteed non-zero: a terminal reporting zero is clamped to
/// the conventional 80x24, because the renderer divides by the column count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinSize {
    /// Number of rows.
    pub rows: u16,
    /// Number of columns.
    pub cols: u16,
}

impl Default for WinSize {
    fn default() -> Self {
        WinSize {
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
        }
    }
}

impl WinSize {
    /// Replaces any zero dimension with the conventional default.
    #[must_use]
    fn clamped(self) -> Self {
        WinSize {
            rows: if self.rows == 0 {
                DEFAULT_ROWS
            } else {
                self.rows
            },
            cols: if self.cols == 0 {
                DEFAULT_COLS
            } else {
                self.cols
            },
        }
    }

    /// Columns as a `usize`, never zero.
    #[must_use]
    pub fn cols(self) -> usize {
        self.cols as usize
    }

    /// Rows as a `usize`, never zero.
    #[must_use]
    pub fn rows(self) -> usize {
        self.rows as usize
    }
}

/// Outcome of waiting for input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Input is available to read.
    Ready,
    /// The timeout elapsed.
    Timeout,
    /// A signal arrived. The caller should handle it and wait again.
    Interrupted,
}

/// RAII guard that puts a terminal into raw mode and restores it on drop.
///
/// Restoring covers the termios settings *and* the `SIGWINCH` / `SIGCONT`
/// handlers, which are installed here and would otherwise leak into whatever
/// the program does next.
pub struct RawMode {
    fd: RawFd,
    original: Termios,
    prev_winch: Option<SigAction>,
    prev_cont: Option<SigAction>,
    active: bool,
}

impl RawMode {
    /// Puts `fd` into raw mode and installs the resize and resume handlers.
    pub fn enable(fd: RawFd) -> Result<Self> {
        let borrowed = borrow(fd);
        let original = termios::tcgetattr(borrowed)
            .map_err(|e| RustlineError::Terminal(format!("cannot read attributes: {e}")))?;

        let mut guard = RawMode {
            fd,
            original,
            prev_winch: None,
            prev_cont: None,
            active: false,
        };
        guard.apply()?;

        let action = SigAction::new(
            SigHandler::Handler(on_sigwinch),
            // No SA_RESTART: an interrupted read is how we learn to redraw.
            SaFlags::empty(),
            SigSet::empty(),
        );
        // SAFETY: `on_sigwinch` and `on_sigcont` only store into a `static`
        // `AtomicBool`, which is async-signal-safe.
        guard.prev_winch = Some(unsafe { signal::sigaction(Signal::SIGWINCH, &action)? });

        let action = SigAction::new(
            SigHandler::Handler(on_sigcont),
            SaFlags::empty(),
            SigSet::empty(),
        );
        // SAFETY: see above.
        guard.prev_cont = Some(unsafe { signal::sigaction(Signal::SIGCONT, &action)? });

        GOT_SIGWINCH.store(false, Ordering::SeqCst);
        GOT_SIGCONT.store(false, Ordering::SeqCst);
        Ok(guard)
    }

    /// Writes the raw termios settings to the terminal.
    fn apply(&mut self) -> Result<()> {
        let mut raw = self.original.clone();
        raw.input_flags.remove(
            termios::InputFlags::BRKINT
                | termios::InputFlags::ICRNL
                | termios::InputFlags::INPCK
                | termios::InputFlags::ISTRIP
                | termios::InputFlags::IXON,
        );
        raw.input_flags.insert(termios::InputFlags::IUTF8);
        raw.local_flags.remove(
            termios::LocalFlags::ECHO
                | termios::LocalFlags::ICANON
                | termios::LocalFlags::IEXTEN
                | termios::LocalFlags::ISIG,
        );
        raw.control_flags |= termios::ControlFlags::CS8;
        raw.control_chars[termios::SpecialCharacterIndices::VMIN as usize] = 1;
        raw.control_chars[termios::SpecialCharacterIndices::VTIME as usize] = 0;
        termios::tcsetattr(borrow(self.fd), termios::SetArg::TCSANOW, &raw)
            .map_err(|e| RustlineError::Terminal(format!("cannot enter raw mode: {e}")))?;
        self.active = true;
        Ok(())
    }

    /// Restores the terminal's original settings without consuming the guard.
    ///
    /// Used before suspending the process, so the shell inherits a sane tty.
    pub fn disable(&mut self) -> Result<()> {
        if self.active {
            termios::tcsetattr(borrow(self.fd), termios::SetArg::TCSANOW, &self.original)
                .map_err(|e| RustlineError::Terminal(format!("cannot leave raw mode: {e}")))?;
            self.active = false;
        }
        Ok(())
    }

    /// Re-enters raw mode after [`RawMode::disable`] or a `SIGCONT`.
    pub fn reenable(&mut self) -> Result<()> {
        if !self.active {
            self.apply()?;
        }
        Ok(())
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = self.disable();
        if let Some(prev) = self.prev_winch.take() {
            // SAFETY: restoring a handler captured from `sigaction` in `enable`.
            let _ = unsafe { signal::sigaction(Signal::SIGWINCH, &prev) };
        }
        if let Some(prev) = self.prev_cont.take() {
            // SAFETY: see above.
            let _ = unsafe { signal::sigaction(Signal::SIGCONT, &prev) };
        }
    }
}

extern "C" fn on_sigwinch(_: i32) {
    GOT_SIGWINCH.store(true, Ordering::SeqCst);
}

extern "C" fn on_sigcont(_: i32) {
    GOT_SIGCONT.store(true, Ordering::SeqCst);
}

/// Returns and clears the "terminal was resized" flag.
pub fn take_sigwinch() -> bool {
    GOT_SIGWINCH.swap(false, Ordering::SeqCst)
}

/// Returns and clears the "process was foregrounded" flag.
pub fn take_sigcont() -> bool {
    GOT_SIGCONT.swap(false, Ordering::SeqCst)
}

/// Borrows a raw descriptor for the duration of one call.
///
/// The descriptor is owned by the caller of `readline` (normally stdin or
/// stdout) and must outlive every use here, which it does because the editor
/// never outlives the call it was given the descriptor by.
fn borrow(fd: RawFd) -> BorrowedFd<'static> {
    // SAFETY: `fd` is a descriptor the caller owns and keeps open for the whole
    // editing session; the returned borrow never escapes this module's calls.
    unsafe { BorrowedFd::borrow_raw(fd) }
}

/// Returns `true` if `term` names a terminal that cannot render escape codes.
///
/// Split out from [`is_unsupported_term`] so the rule can be tested without
/// writing to the environment: `set_var` is unsound while any other thread is
/// *reading* the environment, and this crate's other tests read `$HOME`.
#[must_use]
fn is_unsupported_term_name(term: &str) -> bool {
    UNSUPPORTED_TERMS
        .iter()
        .any(|t| t.eq_ignore_ascii_case(term))
}

/// Returns `true` if `$TERM` names a terminal that cannot render escape codes.
#[must_use]
pub fn is_unsupported_term() -> bool {
    std::env::var("TERM").is_ok_and(|term| is_unsupported_term_name(&term))
}

/// Returns `true` if `fd` refers to a terminal.
#[must_use]
pub fn is_tty(fd: RawFd) -> bool {
    unistd::isatty(borrow(fd)).unwrap_or(false)
}

/// Determines the terminal size, falling back through several strategies.
///
/// 1. `TIOCGWINSZ`, which works for pseudoterminals.
/// 2. `$COLUMNS` / `$ROWS`, which Emacs and some CI runners set.
/// 3. A cursor-position report, which works over a pipe or serial line.
/// 4. 80x24.
///
/// The result always has non-zero dimensions.
#[must_use]
pub fn terminal_size(ifd: RawFd, ofd: RawFd) -> WinSize {
    let mut size = ioctl_size(ofd).unwrap_or(WinSize { rows: 0, cols: 0 });

    if size.rows == 0 {
        if let Some(rows) = env_dimension("ROWS") {
            size.rows = rows;
        }
    }
    if size.cols == 0 {
        if let Some(cols) = env_dimension("COLUMNS") {
            size.cols = cols;
        }
    }

    if (size.rows == 0 || size.cols == 0) && is_tty(ifd) {
        if let Some(reported) = query_size(ifd, ofd) {
            if size.rows == 0 {
                size.rows = reported.rows;
            }
            if size.cols == 0 {
                size.cols = reported.cols;
            }
        }
    }

    size.clamped()
}

fn ioctl_size(fd: RawFd) -> Option<WinSize> {
    use nix::libc::{TIOCGWINSZ, ioctl, winsize};
    let mut ws = winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `TIOCGWINSZ` writes a `winsize` through the pointer, which points
    // at a live, correctly typed local.
    let rc = unsafe { ioctl(fd, TIOCGWINSZ, &raw mut ws) };
    if rc == 0 {
        Some(WinSize {
            rows: ws.ws_row,
            cols: ws.ws_col,
        })
    } else {
        None
    }
}

fn env_dimension(name: &str) -> Option<u16> {
    std::env::var(name)
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|n| *n > 0)
}

/// Asks the terminal where the cursor is after moving it to the far corner.
///
/// Bounded by a short poll timeout so a terminal that never answers costs one
/// tenth of a second rather than hanging the prompt forever.
fn query_size(ifd: RawFd, ofd: RawFd) -> Option<WinSize> {
    const QUERY: &[u8] = b"\x1b7\x1b[9979;9979H\x1b[6n\x1b8";
    write_all(ofd, QUERY).ok()?;

    let mut buf = [0u8; 32];
    let mut len = 0;
    // The reply is `ESC [ rows ; cols R`, possibly split across reads.
    while len < buf.len() {
        match wait_for_input(ifd, Some(100)) {
            Ok(Wait::Ready) => {}
            Ok(Wait::Interrupted) => continue,
            _ => break,
        }
        match read_input(ifd, &mut buf[len..]) {
            Ok(Some(0)) | Err(_) => break,
            Ok(Some(n)) => len += n,
            Ok(None) => continue,
        }
        if buf[..len].contains(&b'R') {
            break;
        }
    }

    let reply = std::str::from_utf8(&buf[..len]).ok()?;
    let body = reply.strip_prefix("\x1b[")?;
    let body = body.split('R').next()?;
    let mut parts = body.split(';');
    let rows = parts.next()?.trim().parse().ok()?;
    let cols = parts.next()?.trim().parse().ok()?;
    Some(WinSize { rows, cols })
}

/// Waits until `fd` has input, the timeout expires, or a signal arrives.
///
/// `timeout_ms` of `None` waits indefinitely. `EINTR` is reported as
/// [`Wait::Interrupted`] rather than retried internally, so the caller can
/// react to a resize before waiting again.
pub fn wait_for_input(fd: RawFd, timeout_ms: Option<u16>) -> Result<Wait> {
    let borrowed = borrow(fd);
    let mut fds = [PollFd::new(borrowed, PollFlags::POLLIN)];
    let timeout = timeout_ms.map_or(PollTimeout::NONE, PollTimeout::from);

    match poll(&mut fds, timeout) {
        Ok(0) => Ok(Wait::Timeout),
        Ok(_) => {
            let revents = fds[0].revents().unwrap_or_else(PollFlags::empty);
            if revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR) {
                Ok(Wait::Ready)
            } else {
                Ok(Wait::Timeout)
            }
        }
        Err(nix::errno::Errno::EINTR) => Ok(Wait::Interrupted),
        Err(e) => Err(e.into()),
    }
}

/// Reads from `fd`, returning `Ok(None)` if the read was interrupted.
pub fn read_input(fd: RawFd, buf: &mut [u8]) -> Result<Option<usize>> {
    match unistd::read(borrow(fd), buf) {
        Ok(n) => Ok(Some(n)),
        Err(nix::errno::Errno::EINTR) => Ok(None),
        // A non-blocking descriptor with nothing ready is not an error; the
        // caller will poll again.
        Err(nix::errno::Errno::EAGAIN) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Writes all of `buf` to `fd`, retrying short writes and `EINTR`.
pub fn write_all(fd: RawFd, buf: &[u8]) -> Result<()> {
    let mut written = 0;
    while written < buf.len() {
        match unistd::write(borrow(fd), &buf[written..]) {
            Ok(0) => {
                return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into());
            }
            Ok(n) => written += n,
            Err(nix::errno::Errno::EINTR) => {}
            Err(nix::errno::Errno::EAGAIN) => {
                // Descriptor is non-blocking and full; wait for it to drain.
                let borrowed = borrow(fd);
                let mut fds = [PollFd::new(borrowed, PollFlags::POLLOUT)];
                let _ = poll(&mut fds, PollTimeout::from(100u16));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Clears the screen and homes the cursor.
pub fn clear_screen(fd: RawFd) -> Result<()> {
    write_all(fd, b"\x1b[H\x1b[2J")
}

/// Stops terminal output, as `CTRL-S` does under flow control.
pub fn pause_output(fd: RawFd) -> Result<()> {
    termios::tcflow(borrow(fd), termios::FlowArg::TCOOFF)?;
    Ok(())
}

/// Resumes terminal output stopped by [`pause_output`].
pub fn resume_output(fd: RawFd) -> Result<()> {
    termios::tcflow(borrow(fd), termios::FlowArg::TCOON)?;
    Ok(())
}

/// Suspends the process, as `CTRL-Z` does.
///
/// The caller is responsible for leaving raw mode first and re-entering it
/// afterwards; [`crate::edit`] does both around this call.
pub fn suspend() -> Result<()> {
    signal::raise(Signal::SIGTSTP)?;
    Ok(())
}

/// Re-raises `signal` with the default disposition, terminating the process.
///
/// Used for `CTRL-\`, which must behave like the tty's `VQUIT` character.
pub fn raise_default(sig: Signal) -> Result<()> {
    // SAFETY: installing the default disposition for a signal is always sound;
    // the process is about to be terminated by it.
    unsafe {
        signal::signal(sig, SigHandler::SigDfl)?;
    }
    signal::raise(sig)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winsize_never_reports_zero() {
        let clamped = WinSize { rows: 0, cols: 0 }.clamped();
        assert_eq!(clamped.rows, DEFAULT_ROWS);
        assert_eq!(clamped.cols, DEFAULT_COLS);
        assert!(clamped.cols() > 0);
        assert!(clamped.rows() > 0);

        let partial = WinSize { rows: 0, cols: 100 }.clamped();
        assert_eq!(partial.rows, DEFAULT_ROWS);
        assert_eq!(partial.cols, 100);
    }

    #[test]
    fn default_winsize_is_usable() {
        let d = WinSize::default();
        assert_eq!((d.rows, d.cols), (DEFAULT_ROWS, DEFAULT_COLS));
    }

    #[test]
    fn terminal_size_on_a_pipe_falls_back_without_hanging() {
        // A pipe has no window size and will never answer a cursor query, so
        // this exercises the timeout path.
        let (r, w) = nix::unistd::pipe().unwrap();
        use std::os::fd::AsRawFd;
        let size = terminal_size(r.as_raw_fd(), w.as_raw_fd());
        assert!(size.cols > 0 && size.rows > 0);
    }

    #[test]
    fn unsupported_terminals_are_recognized() {
        assert!(is_unsupported_term_name("dumb"));
        assert!(is_unsupported_term_name("DUMB"));
        assert!(is_unsupported_term_name("cons25"));
        assert!(is_unsupported_term_name("emacs"));

        assert!(!is_unsupported_term_name("xterm-256color"));
        assert!(!is_unsupported_term_name("dumb-but-not-really"));
        assert!(!is_unsupported_term_name(""));

        // Whatever the environment says, reading it must not panic. This used
        // to be a `set_var` test, which is unsound while another thread reads
        // any environment variable, and the history tests read `$HOME`.
        let _ = is_unsupported_term();
    }

    /// A descriptor that is not a terminal must say so as a terminal error,
    /// not as a bare errno.
    #[test]
    fn raw_mode_on_a_pipe_is_a_terminal_error() {
        use std::os::fd::AsRawFd;
        let (r, _w) = nix::unistd::pipe().unwrap();
        let Err(err) = RawMode::enable(r.as_raw_fd()) else {
            panic!("a pipe was accepted as a terminal");
        };
        assert!(
            matches!(err, RustlineError::Terminal(_)),
            "expected a terminal error, got {err:?}",
        );
        assert!(err.to_string().starts_with("terminal error:"));
    }

    #[test]
    fn write_all_handles_a_pipe() {
        use std::io::Read;
        use std::os::fd::AsRawFd;
        let (r, w) = nix::unistd::pipe().unwrap();
        write_all(w.as_raw_fd(), b"hello").unwrap();
        drop(w);
        let mut out = String::new();
        std::fs::File::from(r).read_to_string(&mut out).unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn signal_flags_latch_and_clear() {
        GOT_SIGWINCH.store(true, Ordering::SeqCst);
        assert!(take_sigwinch());
        assert!(!take_sigwinch());
        GOT_SIGCONT.store(true, Ordering::SeqCst);
        assert!(take_sigcont());
        assert!(!take_sigcont());
    }

    #[test]
    fn wait_reports_ready_and_timeout() {
        use std::os::fd::AsRawFd;
        let (r, w) = nix::unistd::pipe().unwrap();
        assert_eq!(
            wait_for_input(r.as_raw_fd(), Some(1)).unwrap(),
            Wait::Timeout
        );
        write_all(w.as_raw_fd(), b"x").unwrap();
        assert_eq!(
            wait_for_input(r.as_raw_fd(), Some(50)).unwrap(),
            Wait::Ready
        );

        let mut buf = [0u8; 4];
        assert_eq!(read_input(r.as_raw_fd(), &mut buf).unwrap(), Some(1));
        assert_eq!(&buf[..1], b"x");
    }
}
