use crate::error::Result;
use nix::poll::{PollFd, PollFlags, poll};
use nix::sys::signal::{self, SigHandler, Signal};
use nix::sys::termios::{self, Termios};
use nix::unistd;
use std::os::unix::io::{BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};

static GOT_SIGWINCH: AtomicBool = AtomicBool::new(false);
static GOT_SIGCONT: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, Default)]
#[allow(unused)]
pub struct WinSize {
    pub rows: u16,
    pub cols: u16,
}

pub struct RawMode {
    fd: RawFd,
    orig_termios: Termios,
}

impl RawMode {
    pub fn enable(fd: RawFd) -> Result<Self> {
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(fd) };
        let orig_termios = termios::tcgetattr(borrowed_fd)?;
        let mut raw = orig_termios.clone();

        raw.input_flags.remove(
            termios::InputFlags::BRKINT
                | termios::InputFlags::ICRNL
                | termios::InputFlags::INPCK
                | termios::InputFlags::ISTRIP
                | termios::InputFlags::IXON,
        );
        raw.local_flags.remove(
            termios::LocalFlags::ECHO
                | termios::LocalFlags::ICANON
                | termios::LocalFlags::IEXTEN
                | termios::LocalFlags::ISIG,
        );
        raw.control_flags |= termios::ControlFlags::CS8;
        raw.control_chars[termios::SpecialCharacterIndices::VMIN as usize] = 1;
        raw.control_chars[termios::SpecialCharacterIndices::VTIME as usize] = 0;

        termios::tcsetattr(borrowed_fd, termios::SetArg::TCSANOW, &raw)?;

        unsafe {
            signal::signal(Signal::SIGWINCH, SigHandler::Handler(sigwinch_handler))?;
            signal::signal(Signal::SIGCONT, SigHandler::Handler(sigcont_handler))?;
        }

        Ok(RawMode { fd, orig_termios })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(self.fd) };
        let _ = termios::tcsetattr(borrowed_fd, termios::SetArg::TCSANOW, &self.orig_termios);
    }
}

extern "C" fn sigwinch_handler(_: i32) {
    GOT_SIGWINCH.store(true, Ordering::SeqCst);
}

extern "C" fn sigcont_handler(_: i32) {
    GOT_SIGCONT.store(true, Ordering::SeqCst);
}

pub fn get_terminal_size(ifd: RawFd, ofd: RawFd) -> Result<WinSize> {
    use nix::libc::{TIOCGWINSZ, ioctl, winsize};

    let mut ws = winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    unsafe {
        if ioctl(ofd, TIOCGWINSZ, &mut ws) == 0 {
            return Ok(WinSize {
                rows: ws.ws_row,
                cols: ws.ws_col,
            });
        }
    }

    detect_size_ansi(ifd, ofd)
}

fn detect_size_ansi(ifd: RawFd, ofd: RawFd) -> Result<WinSize> {
    let query = "\x1b7\x1b[9979;9979H\x1b[6n\x1b8";
    let borrowed_ofd = unsafe { BorrowedFd::borrow_raw(ofd) };
    unistd::write(borrowed_ofd, query.as_bytes())?;

    let mut buf = [0u8; 32];
    let borrowed_ifd = unsafe { BorrowedFd::borrow_raw(ifd) };
    let n = unistd::read(borrowed_ifd, &mut buf)?;

    if n > 0 && buf[0] == b'\x1b' && buf[1] == b'[' {
        let response = std::str::from_utf8(&buf[2..n]).ok();
        if let Some(resp) = response {
            if let Some(r_pos) = resp.find('R') {
                let coords = &resp[..r_pos];
                let parts: Vec<&str> = coords.split(';').collect();
                if parts.len() == 2 {
                    let rows = parts[0].parse().unwrap_or(24);
                    let cols = parts[1].parse().unwrap_or(80);
                    return Ok(WinSize { rows, cols });
                }
            }
        }
    }

    Ok(WinSize { rows: 24, cols: 80 })
}

#[allow(unused)]
pub fn check_sigwinch() -> bool {
    GOT_SIGWINCH.swap(false, Ordering::SeqCst)
}

#[allow(unused)]
pub fn check_sigcont() -> bool {
    GOT_SIGCONT.swap(false, Ordering::SeqCst)
}

pub fn wait_for_input(fd: RawFd, timeout_ms: i32) -> Result<bool> {
    let borrowed_fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let mut fds = [PollFd::new(borrowed_fd, PollFlags::POLLIN)];

    let timeout = if timeout_ms < 0 {
        nix::poll::PollTimeout::NONE
    } else {
        nix::poll::PollTimeout::try_from(timeout_ms).unwrap_or(nix::poll::PollTimeout::NONE)
    };

    let n = poll(&mut fds, timeout)?;
    Ok(n > 0
        && fds[0]
            .revents()
            .unwrap_or(PollFlags::empty())
            .contains(PollFlags::POLLIN))
}

pub fn clear_screen(fd: RawFd) -> Result<()> {
    let borrowed_fd = unsafe { BorrowedFd::borrow_raw(fd) };
    unistd::write(borrowed_fd, b"\x1b[H\x1b[2J")?;
    Ok(())
}
