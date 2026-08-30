use std::io;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RustlineError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Terminal error: {0}")]
    Terminal(String),

    #[error("Interrupted")]
    Interrupted,

    #[error("End of file")]
    Eof,

    #[error("Invalid UTF-8 sequence")]
    InvalidUtf8,

    #[error("History error: {0}")]
    History(String),

    #[error("Nix error: {0}")]
    Nix(#[from] nix::Error),
}

pub type Result<T> = std::result::Result<T, RustlineError>;
