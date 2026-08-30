//! The simplest useful rustline program.
//!
//! Run with `cargo run --example basic`. Demonstrates reading lines, keeping
//! history, and handling interrupt and end of file.

use rustline::{FileCompleter, Rustline, RustlineError};

fn main() {
    let mut rl = Rustline::new();
    rl.set_completer(Box::new(FileCompleter::new()));

    println!("basic rustline example -- 'help' for commands, 'exit' to quit");

    loop {
        match rl.readline(">> ") {
            Ok(line) => {
                let input = line.trim();
                if input.is_empty() {
                    continue;
                }
                rl.add_history_entry(&line);

                match input {
                    "help" => print_help(),
                    "exit" | "quit" => break,
                    "history" => print_history(&rl),
                    "clear" => {
                        let _ = rustline::clear_screen();
                    }
                    other => println!("you typed: {other}"),
                }
            }
            // CTRL-C abandons the line but keeps the session.
            Err(RustlineError::Interrupted) => println!("interrupted"),
            // CTRL-D on an empty line, or end of piped input.
            Err(RustlineError::Eof) => break,
            Err(e) => {
                eprintln!("error: {e}");
                break;
            }
        }
    }

    println!("goodbye");
}

fn print_help() {
    println!(
        "\
  help      show this message
  history   list previous lines
  clear     clear the screen
  exit      quit

  TAB completes file names; CTRL-R searches history."
    );
}

fn print_history(rl: &Rustline) {
    let history = rl.history();
    if history.is_empty() {
        println!("(no history yet)");
        return;
    }
    for (i, entry) in history.iter().enumerate() {
        println!("  {:3}: {entry}", i + 1);
    }
}
