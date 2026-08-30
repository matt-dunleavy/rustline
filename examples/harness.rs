//! Test harness driven by the integration tests.
//!
//! Reads lines and reports each result on its own line, prefixed with a marker
//! and escaped with `Debug` so that embedded newlines never split the report:
//!
//! ```text
//! @@LINE@@ "echo hi"
//! @@EOF@@
//! @@INT@@
//! ```
//!
//! Behaviour is selected by command-line flags so one binary covers every
//! scenario the tests need.

use rustline::{
    CommandCompleter, Config, FileCompleter, Hint, HintProvider, Rustline, RustlineError,
};

const COMMANDS: &[&str] = &["help", "history", "hello", "exit"];

struct Hinter;

impl HintProvider for Hinter {
    fn hint(&self, line: &str, _pos: usize) -> Option<Hint> {
        COMMANDS
            .iter()
            .find(|c| c.starts_with(line) && **c != line && !line.is_empty())
            .map(|c| Hint::new(&c[line.len()..]))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    let config = Config {
        balance_pairs: has("--balance"),
        enable_multiline: !has("--no-multiline"),
        enable_hints: has("--hints"),
        mask_mode: has("--mask"),
        highlight_brackets: !has("--no-highlight"),
        ..Default::default()
    };

    let mut rl = Rustline::with_config(config);

    if has("--files") {
        rl.set_completer(Box::new(FileCompleter::new()));
    }
    if has("--commands") {
        rl.set_completer(Box::new(CommandCompleter::new(
            COMMANDS.iter().copied(),
            FileCompleter::new(),
        )));
    }
    if has("--hints") {
        rl.set_hinter(Box::new(Hinter));
    }
    if has("--upcase") {
        rl.set_xlat(Box::new(|c| c.to_ascii_uppercase()));
    }

    let prompt = args
        .iter()
        .find_map(|a| a.strip_prefix("--prompt="))
        .unwrap_or("> ")
        .to_string();

    loop {
        match rl.readline(&prompt) {
            Ok(line) => {
                println!("@@LINE@@ {line:?}");
                rl.add_history_entry(&line);
            }
            Err(RustlineError::Interrupted) => println!("@@INT@@"),
            Err(RustlineError::Eof) => {
                println!("@@EOF@@");
                break;
            }
            Err(e) => {
                println!("@@ERR@@ {e}");
                break;
            }
        }
    }
}
