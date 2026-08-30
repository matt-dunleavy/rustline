//! Shared helpers for the integration tests: a real pseudoterminal to drive the
//! editor through, and a small terminal emulator to check what it drew.
//!
//! The editor's whole job is to produce a byte stream that makes a terminal
//! show the right thing. Unit tests cannot see that stream in context, so the
//! regressions that matter most are checked here, against a real pty.

#![allow(dead_code)]

use nix::libc;
use nix::pty::openpty;
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Locates a built example binary.
///
/// Test binaries live in `<target>/<profile>/deps`, so the examples that
/// `cargo test` builds alongside them are two directories up.
fn example_path(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("test binary path");
    let profile_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("target/<profile>");
    let path = profile_dir.join("examples").join(name);
    assert!(
        path.exists(),
        "example `{name}` not built at {}; run `cargo build --examples`",
        path.display(),
    );
    path
}

/// A child process attached to its own pseudoterminal.
pub struct Pty {
    master: OwnedFd,
    child: Child,
    output: Vec<u8>,
}

impl Pty {
    /// Spawns the `harness` example with `args` on a `rows` x `cols` terminal.
    pub fn spawn(args: &[&str], rows: u16, cols: u16) -> Pty {
        let pty = openpty(None, None).expect("openpty");
        let master = pty.master;
        let slave = pty.slave;

        let mut command = Command::new(example_path("harness"));
        command
            .args(args)
            .env("TERM", "xterm-256color")
            // Keep the tests away from the developer's real history file.
            .env("HOME", std::env::temp_dir())
            .stdin(Stdio::from(slave.try_clone().expect("dup slave")))
            .stdout(Stdio::from(slave.try_clone().expect("dup slave")))
            .stderr(Stdio::from(slave.try_clone().expect("dup slave")));

        // SAFETY: `setsid` and `ioctl` are async-signal-safe and allocate
        // nothing, which is the requirement for a `pre_exec` callback. This
        // makes the pty the child's controlling terminal so that resizing it
        // actually delivers SIGWINCH.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setsid().map_err(std::io::Error::from)?;
                if libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = command.spawn().expect("spawn harness");
        drop(slave);

        let mut pty = Pty {
            master,
            child,
            output: Vec::new(),
        };
        pty.resize(rows, cols);
        // Let the child reach its first prompt before any keys arrive.
        pty.settle();
        pty
    }

    /// Resizes the terminal, which sends `SIGWINCH` to the child.
    pub fn resize(&self, rows: u16, cols: u16) {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `TIOCSWINSZ` reads a `winsize` through the pointer, which
        // points at a live, correctly typed local.
        let rc = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &raw const ws) };
        assert_eq!(rc, 0, "TIOCSWINSZ failed");
    }

    /// Sends keystrokes.
    pub fn send(&mut self, bytes: &[u8]) {
        nix::unistd::write(self.master.as_fd(), bytes).expect("write to pty");
    }

    /// Sends keystrokes and waits for the output to stop.
    pub fn type_keys(&mut self, bytes: &[u8]) {
        self.send(bytes);
        self.settle();
    }

    /// Reads until the child stops producing output.
    pub fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut quiet_since: Option<Instant> = None;

        while Instant::now() < deadline {
            if self.read_once(Duration::from_millis(60)) {
                quiet_since = None;
                continue;
            }
            match quiet_since {
                // Two consecutive quiet polls means the child is idle.
                Some(t) if t.elapsed() >= Duration::from_millis(120) => return,
                Some(_) => {}
                None => quiet_since = Some(Instant::now()),
            }
        }
    }

    /// Polls once, appending whatever arrived. Returns whether anything did.
    fn read_once(&mut self, timeout: Duration) -> bool {
        use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

        let mut fds = [PollFd::new(self.master.as_fd(), PollFlags::POLLIN)];
        let ms = u16::try_from(timeout.as_millis()).unwrap_or(u16::MAX);
        match poll(&mut fds, PollTimeout::from(ms)) {
            Ok(0) | Err(_) => return false,
            Ok(_) => {}
        }

        let mut buf = [0u8; 8192];
        // The master reports EIO rather than EOF once the child is gone.
        match nix::unistd::read(self.master.as_fd(), &mut buf) {
            Ok(0) | Err(_) => false,
            Ok(n) => {
                self.output.extend_from_slice(&buf[..n]);
                true
            }
        }
    }

    /// Everything the child has written so far.
    pub fn output(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }

    /// Discards the output captured so far.
    pub fn clear(&mut self) {
        self.output.clear();
    }

    /// Every result the harness has reported, in order.
    ///
    /// A submitted line appears as its `Debug` form, so an embedded newline is
    /// visible as `\n` rather than splitting the report across lines.
    pub fn results(&self) -> Vec<String> {
        let text = self.output();
        let mut found = Vec::new();
        for marker in ["@@LINE@@ ", "@@EOF@@", "@@INT@@", "@@ERR@@ "] {
            for (index, _) in text.match_indices(marker) {
                let rest = &text[index..];
                let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
                found.push((index, rest[..end].trim().to_string()));
            }
        }
        found.sort_by_key(|(index, _)| *index);
        found.into_iter().map(|(_, text)| text).collect()
    }

    /// The submitted lines, unquoted.
    pub fn lines(&self) -> Vec<String> {
        self.results()
            .into_iter()
            .filter_map(|r| {
                let quoted = r.strip_prefix("@@LINE@@ ")?;
                Some(unquote(quoted))
            })
            .collect()
    }

    /// Whether the child panicked.
    pub fn panicked(&self) -> bool {
        self.output().contains("panicked at")
    }

    /// Whether the child is still running.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Renders the captured output onto a screen of the given size.
    pub fn screen(&self, rows: usize, cols: usize) -> Screen {
        let mut screen = Screen::new(rows, cols);
        screen.feed(&self.output);
        screen
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Reverses the `Debug` escaping the harness applies to a submitted line.
fn unquote(quoted: &str) -> String {
    let inner = quoted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(quoted);

    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// A minimal VT100 screen, enough to check what the user would see.
///
/// Implements deferred wrap, as real terminals do: a character written into the
/// last column leaves the cursor there with a pending wrap, and a following
/// `\r` cancels it instead of skipping a row.
pub struct Screen {
    cells: Vec<Vec<char>>,
    row: usize,
    col: usize,
    cols: usize,
    pending_wrap: bool,
}

impl Screen {
    pub fn new(rows: usize, cols: usize) -> Self {
        Screen {
            cells: vec![vec![' '; cols]; rows.max(1)],
            row: 0,
            col: 0,
            cols,
            pending_wrap: false,
        }
    }

    fn ensure_row(&mut self) {
        while self.row >= self.cells.len() {
            self.cells.push(vec![' '; self.cols]);
        }
    }

    fn wrap_now(&mut self) {
        self.row += 1;
        self.col = 0;
        self.pending_wrap = false;
        self.ensure_row();
    }

    fn put(&mut self, c: char) {
        let w = rustline::char_width(c);
        if w == 0 {
            return;
        }
        if self.pending_wrap || self.col + w > self.cols {
            self.wrap_now();
        }
        self.ensure_row();
        self.cells[self.row][self.col] = c;
        for k in 1..w {
            self.cells[self.row][self.col + k] = '\0';
        }
        self.col += w;
        if self.col >= self.cols {
            self.col = self.cols;
            self.pending_wrap = true;
        }
    }

    fn erase_to_end_of_line(&mut self) {
        let col = self.col.min(self.cols);
        for c in self.cells[self.row][col..].iter_mut() {
            *c = ' ';
        }
    }

    fn csi(&mut self, param: &str, final_byte: char) {
        let n: usize = param.parse().unwrap_or(1).max(1);
        self.pending_wrap = false;
        self.ensure_row();
        match final_byte {
            'A' => self.row = self.row.saturating_sub(n),
            'B' => {
                self.row += n;
                self.ensure_row();
            }
            'C' => self.col = (self.col + n).min(self.cols),
            'D' => self.col = self.col.saturating_sub(n),
            'H' => {
                self.row = 0;
                self.col = 0;
            }
            'K' => self.erase_to_end_of_line(),
            'J' => {
                if param == "2" {
                    for row in self.cells.iter_mut() {
                        row.fill(' ');
                    }
                } else {
                    self.erase_to_end_of_line();
                    for row in self.cells[self.row + 1..].iter_mut() {
                        row.fill(' ');
                    }
                }
            }
            _ => {}
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let chars: Vec<char> = String::from_utf8_lossy(bytes).chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            i += 1;
            match c {
                '\r' => {
                    self.col = 0;
                    self.pending_wrap = false;
                }
                '\n' => {
                    self.row += 1;
                    self.pending_wrap = false;
                    self.ensure_row();
                }
                '\x1b' if chars.get(i) == Some(&'[') => {
                    i += 1;
                    let start = i;
                    i += chars[i..]
                        .iter()
                        .take_while(|c| c.is_ascii_digit() || **c == ';' || **c == '?')
                        .count();
                    let param: String = chars[start..i].iter().collect();
                    let final_byte = chars.get(i).copied().unwrap_or(' ');
                    i += 1;
                    self.csi(&param, final_byte);
                }
                '\x1b' => {}
                c => self.put(c),
            }
        }
    }

    /// The visible text of one row, trailing blanks removed.
    pub fn line(&self, row: usize) -> String {
        self.cells
            .get(row)
            .map(|r| {
                r.iter()
                    .filter(|c| **c != '\0')
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .unwrap_or_default()
    }

    /// Where the next character would be drawn.
    pub fn cursor(&self) -> (usize, usize) {
        if self.pending_wrap {
            (self.row + 1, 0)
        } else {
            (self.row, self.col)
        }
    }
}

/// Reads the whole of `child`'s remaining output. Used only for diagnostics.
pub fn drain_reader(mut reader: impl Read) -> String {
    let mut buf = String::new();
    let _ = reader.read_to_string(&mut buf);
    buf
}
