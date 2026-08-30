//! Error type returned by the editor.

use std::io;
use thiserror::Error;

/// Errors produced while reading a line.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum RustlineError {
    /// An underlying I/O operation failed.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),

    /// The terminal could not be configured.
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

    /// A history operation failed.
    #[error("history error: {0}")]
    History(String),

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
}
