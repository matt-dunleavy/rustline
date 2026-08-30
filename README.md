# Rustline

A line editor for interactive terminal programs — a Rust port of [bestline](https://github.com/jart/bestline), which is itself a fork of [linenoise](https://github.com/antirez/linenoise).

Emacs-style editing, reverse history search, completion, hints, and UTF-8 editing over ANSI X3.64 escape sequences. No terminfo, no ncurses, no terminal
capability database.

```rust
use rustline::{Rustline, RustlineError};

fn main() -> rustline::Result<()> {
    let mut rl = Rustline::new();
    loop {
        match rl.readline("> ") {
            Ok(line) => {
                rl.add_history_entry(&line);
                println!("{line}");
            }
            Err(RustlineError::Interrupted) => continue,
            Err(RustlineError::Eof) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
```

Run `cargo run --example repl` for a REPL exercising every feature, or `cargo run --example basic` for the shortest useful program.

## Key bindings

```
CTRL-A / HOME     start of line        CTRL-T          transpose chars
CTRL-E / END      end of line          ALT-T           transpose words
CTRL-B / LEFT     back one char        ALT-U           uppercase word
CTRL-F / RIGHT    forward one char     ALT-L           lowercase word
ALT-B             back one word        ALT-C           capitalize word
ALT-F             forward one word     ALT-\           squeeze whitespace
ALT-LEFT          back one expr        CTRL-K          kill to end
ALT-RIGHT         forward one expr     CTRL-U          kill to start
CTRL-P / UP       previous history     CTRL-W / ALT-H  kill word backwards
CTRL-N / DOWN     next history         ALT-D           kill word forwards
ALT-<             oldest history       CTRL-Y          yank
ALT->             newest history       ALT-Y           rotate ring and yank
CTRL-R            search history       CTRL-SPACE      set mark
CTRL-G            cancel search        CTRL-X CTRL-X   go to mark
CTRL-H / BKSP     backspace            CTRL-L          clear screen
CTRL-D            delete, or EOF       CTRL-C          interrupt
TAB               complete             CTRL-Z          suspend
ALT-SHIFT-B       barf expression      CTRL-\          quit
ALT-SHIFT-S       slurp expression     CTRL-S / CTRL-Q flow control
                                       CTRL-Q          escaped insert
```

## Features

- **Emacs bindings**, including the word, case, kill-ring and mark commands.
- **Reverse incremental search** (`CTRL-R`), with the matched prefix underlined
  in the prompt.
- **Completion** through the `CompletionProvider` trait. A provider reports the
  byte range it replaces, so the editor never has to guess a word boundary.
  `FileCompleter` and `CommandCompleter` are included.
- **Hints**: advisory text drawn after the cursor that is never part of the line.
- **Mask mode** for passwords, via `Rustline::read_password`.
- **Multiline entry**, either explicitly with `CTRL-J` or automatically until
  parentheses balance.
- **Bracketed paste**: pasted newlines become real newlines and pasted control
  characters are never executed.
- **UTF-8 throughout**, with correct display widths for wide and zero-width
  characters, and correct wrapping at the right edge of the screen.
- **Terminal resizing** is handled while the prompt is up.
- **Suspend and resume**: `CTRL-Z` restores the terminal, and raw mode is
  re-entered on `SIGCONT`.
- **Paredit** barf and slurp for editing s-expressions.
- **Transliteration hook** for input methods that map one keyboard to another
  script.

## Differences from bestline

Deliberate divergences, all of them in Rustline's favour:

- **No global state.** bestline keeps history, the kill ring, raw-mode state and
  the mode flags in file-scope statics, which limits it to one prompt per
  process. Everything here belongs to a `Rustline` value.
- **Completion providers are traits**, not a single C function pointer, and they
  return a replacement range rather than a whole replacement line.
- **Raw mode is an RAII guard**, so the terminal is restored on unwind as well as
  on a normal return. bestline needs an `atexit` hook. The `SIGWINCH` and
  `SIGCONT` handlers are restored too.
- **Control characters inside a bracketed paste are inserted or ignored, never
  obeyed.** bestline runs its full key dispatch during a paste, so a pasted tab
  triggers completion.
- **`TAB` inserts the common prefix first**, then lists, then cycles — the
  behaviour readline and bash have trained everyone to expect. bestline cycles
  immediately.
- Unicode display width comes from `unicode-width` rather than a hand-maintained
  codepoint table.

## Testing

```
make test      # unit, property and pseudoterminal integration tests
make ci        # what CI runs: fmt, clippy, tests, docs
```

The integration suite in `tests/regressions.rs` drives the editor through a real pseudoterminal and asserts on the resulting screen using a small VT100 model. That is the only way to catch the failures that matter most here: a cursor one row too high, a wide glyph split by the right edge, or a redraw that overwrites the scrollback.

`cargo test` builds the examples first, because the integration tests execute `examples/harness`.

## License

BSD-2-Clause. See `LICENSE` for the full copyright chain back through bestline and linenoise.
