//! Basic readline usage example
//!
//! This example demonstrates the simplest use case for Rustline:
//! reading lines from the user with a prompt.

use rustline::{ReadlineError, Rustline};
use std::io;

fn main() -> io::Result<()> {
    println!("Basic Rustline Example");
    println!("Type 'help' for commands, 'exit' to quit\n");

    // Create a new Rustline instance with default configuration
    let mut rl = Rustline::new();

    // Simple command loop
    loop {
        // Read a line with a prompt
        match rl.readline(">> ") {
            Ok(line) => {
                // Trim whitespace
                let input = line.trim();

                // Skip empty lines
                if input.is_empty() {
                    continue;
                }

                // Add non-empty lines to history
                rl.add_history_entry(&line);

                // Process commands
                match input {
                    "help" => print_help(),
                    "exit" | "quit" => {
                        println!("Goodbye!");
                        break;
                    }
                    "history" => print_history(&rl),
                    "clear" => clear_screen(),
                    _ => {
                        // Echo the input
                        println!("You typed: {}", input);
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                // Handle Ctrl-C
                println!("\nInterrupted (Ctrl-C). Type 'exit' to quit.");
            }
            Err(ReadlineError::Eof) => {
                // Handle Ctrl-D
                println!("\nEOF (Ctrl-D). Exiting...");
                break;
            }
            Err(err) => {
                // Handle other errors
                eprintln!("Error: {:?}", err);
                break;
            }
        }
    }

    Ok(())
}

fn print_help() {
    println!(
        r#"
Available commands:
  help     - Show this help message
  history  - Show command history
  clear    - Clear the screen
  exit     - Exit the program
  quit     - Exit the program

Any other input will be echoed back.

Keyboard shortcuts:
  Ctrl-C   - Cancel current line
  Ctrl-D   - Exit (EOF)
  Up/Down  - Navigate history
  Tab      - Completion (if configured)
"#
    );
}

fn print_history(rl: &Rustline) {
    println!("\nCommand History:");

    // Get history iterator
    if let Some(history) = rl.history() {
        for (index, entry) in history.iter().enumerate() {
            println!("  {}: {}", index + 1, entry);
        }

        if history.is_empty() {
            println!("  (empty)");
        }
    } else {
        println!("  (history not available)");
    }

    println!();
}

fn clear_screen() {
    // ANSI escape code to clear screen and move cursor to top
    print!("\x1b[2J\x1b[H");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_functionality() {
        // This is where you would add tests
        // For a real implementation, you might want to:
        // - Test readline with mock input
        // - Test history functionality
        // - Test error handling
        assert!(true);
    }
}
