//! Error type returned by the editor.

use std::io;
use std::path::PathBuf;
use thiserror::Error;

/// Errors produced while reading a line.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum RustlineError {
    /// An underlying I/O operation failed.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),

    /// The terminal could not be configured.
    ///
    /// Raised when raw mode cannot be entered or left, which is what happens
    /// when the descriptor handed to [`crate::Rustline::readline_raw`] turns
    /// out not to be a terminal after all.
    #[error("terminal error: {0}")]
    Terminal(String),

    /// The user pressed `CTRL-C`.
    ///
    /// The partially typed line is discarded. Callers that want shell-like
    /// behaviour should print a newline and prompt again.
    #[error("interrupted")]
    Interrupted,

    /// End of input: `CTRL-D` on an empty line, or the input stream closed.
    #[error("end of file")]
    Eof,

    /// A history file could not be read or written.
    ///
    /// Distinct from [`RustlineError::Io`] so that a caller can tell "your
    /// history file is unreadable", which is usually recoverable, from an I/O
    /// failure on the terminal itself, which is not.
    #[error("history file {}: {source}", path.display())]
    History {
        /// The file that could not be read or written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },

    /// A system call failed.
    #[error("system error: {0}")]
    Nix(#[from] nix::Error),
}

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, RustlineError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_convert() {
        let err: RustlineError = io::Error::new(io::ErrorKind::BrokenPipe, "gone").into();
        assert!(matches!(err, RustlineError::Io(_)));
        assert!(err.to_string().contains("gone"));
    }

    #[test]
    fn messages_are_lowercase_and_terse() {
        assert_eq!(RustlineError::Eof.to_string(), "end of file");
        assert_eq!(RustlineError::Interrupted.to_string(), "interrupted");
    }

    #[test]
    fn a_history_error_names_the_file_and_keeps_the_cause() {
        let err = RustlineError::History {
            path: PathBuf::from("/tmp/.demo_history"),
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        };
        assert!(err.to_string().contains("/tmp/.demo_history"));
        assert!(std::error::Error::source(&err).is_some());
    }
}
