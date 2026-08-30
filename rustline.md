# Rustline — Review & Implementation Record

**Reviewed:** 2026-08-30 · **Implemented:** 2026-08-30
**Reference:** `ref/bestline-master/bestline.c` (4,101 lines) + `bestline.h`

Every item in the original review has been implemented. This document is now the
record of what was found, what was done about it, and what was deliberately left
different from bestline.

## Status

| | Before | After |
|---|---|---|
| `cargo test` | did not build | 167 pass, 0 fail |
| `cargo clippy -D warnings` | 1 warning | clean |
| `cargo fmt --check` | 16 nightly-only warnings | clean |
| `cargo doc` | no docs at all | clean, every public item documented |
| Library source | 2,541 lines | 4,876 lines |
| Tests | 6 unit | 119 unit + property, 46 pty integration |
| Keybindings working | 11 | 46 |
| Version control | none | `git init`, baseline committed |

Test breakdown: 119 unit and property tests in `src/`, 46 pseudoterminal
integration tests in `tests/regressions.rs`, 2 doctests.

---

## How the review was done

Findings marked **[verified]** were reproduced by driving `target/debug/rustline`
inside a real pseudoterminal (`pty.fork()` + `TIOCSWINSZ`) and capturing the raw
byte stream. That harness found six defects in about twenty minutes that no unit
test would have caught, which is why it is now part of the test suite as
`tests/common/mod.rs`.

---

## P0 — Blockers ✅ all fixed

- [x] **Ctrl-key case mismatch disabled every control binding.**
      `parser.rs` emitted `Ctrl((byte + b'@') as char)`, so `0x01` became
      `Ctrl('A')` — uppercase — while every dispatch arm matched `Ctrl('a')`.
      Ctrl-A/B/C/D/E/F/H/K/L/N/P/U/W/Y were all dead. **[verified]**
      **Fixed:** `ctrl_char` in `src/parser.rs:68` normalizes `0x01..=0x1a` to
      lowercase. Guarded by `control_chords_are_lowercase`,
      `every_control_byte_round_trips` (parser) and
      `the_full_control_key_map_works` (pty).

- [x] **Resizing the terminal terminated the program.** `poll` is not restartable
      via `SA_RESTART`, so `SIGWINCH` produced `Err(EINTR)` straight out of
      `readline`. **[verified]**
      **Fixed:** `wait_for_input` returns `Wait::Interrupted` rather than an
      error (`src/terminal.rs`), and `Rustline::handle_signals`
      (`src/edit.rs`) re-queries the window size, redraws, and waits again.
      `SIGCONT` re-enters raw mode on the same path.
      Guarded by `resizing_the_terminal_does_not_end_the_session` and
      `repeated_resizes_are_absorbed`.

- [x] **Two panics in tab completion.** A char count used as a byte index, and a
      completion assumed longer than the typed word. **[verified]**
      **Fixed** by changing the API rather than patching the arithmetic:
      `Completions` now carries the byte range it replaces
      (`Completions::start`), so the editor never re-derives a word boundary the
      provider already knew. `common_prefix_len` walks characters and returns a
      byte length that is a boundary by construction.
      Guarded by `completion_with_a_multibyte_common_prefix_does_not_panic`,
      `completion_shorter_than_the_typed_word_does_not_panic`, and a unit test
      asserting the prefix length is a char boundary.

- [x] **Divide by zero when the terminal reported 0 columns.** **[verified]**
      **Fixed:** `WinSize::clamped` in `src/terminal.rs` guarantees non-zero
      dimensions, and `terminal_size` now follows bestline's full ladder —
      `TIOCGWINSZ`, then `$COLUMNS`/`$ROWS`, then a cursor-position report
      *with a 100 ms timeout*, then 80x24.
      Guarded by `a_zero_size_terminal_does_not_panic` and
      `a_one_column_terminal_does_not_panic`.

- [x] **Input was dropped when several keys arrived in one read.** One write of
      `echo one\recho two\r` ran only the first. **[verified]**
      **Fixed:** `Rustline` owns a `pending: VecDeque<KeySeq>` queue *and* the
      `Parser`, so both unconsumed keys and a half-received escape sequence
      survive between calls to `readline`.
      Guarded by `keys_batched_with_a_submission_are_not_lost` and
      `a_batch_may_contain_editing_keys`.

- [x] **Bracketed paste discarded newlines.** **[verified]**
      **Fixed:** `dispatch_pasted` in `src/edit.rs` turns `\r`/`\n` into real
      continuation lines. It also, deliberately, ignores control characters
      during a paste rather than obeying them — see *Deliberate divergences*.
      Guarded by `bracketed_paste_preserves_newlines` and
      `bracketed_paste_does_not_execute_control_characters`.

---

## P1 — Correctness ✅ all fixed

- [x] **`refresh_line` moved the cursor up by the wrong amount.** It always
      emitted `move_up(rows_used - 1)`, assuming the cursor sat on the last row.
      `Display::cursor_row` computed the right value and was never read.
      **Fixed:** `src/display.rs` is now a port of bestline's
      `bestlineRefreshLineImpl`. It walks the line character by character
      tracking the column, records `rows_below_cursor` where the cursor actually
      landed, and returns there on the next frame.
      Guarded by `redraw_with_the_cursor_on_an_upper_row_stays_aligned` and
      `many_redraws_never_drift`.

- [x] **`FileCompleter` read the wrong directory for any path with a separator.**
      **[verified]** **Fixed:** `word_start` stops at whitespace only; the word
      is then split at its last `/`. `~` expansion added.
      Guarded by `completion_reads_the_directory_in_the_typed_path`.

- [x] **Prompt width counted ANSI escape bytes as columns.**
      **Fixed:** `unicode::display_width` is a port of bestline's
      `GetMonospaceWidth` — a state machine that skips CSI sequences.
      Guarded by `an_ansi_prompt_does_not_displace_the_cursor`.

- [x] **The renderer relied on terminal auto-wrap and missed the last-column
      case.** **Fixed** as part of the renderer port: explicit `ESC [ K` + `\r\n`
      at each wrap point, per-cell column tracking so a wide glyph never
      straddles the edge, and the `\n\r` emission when the cursor lands in the
      final column. Guarded by `wide_characters_wrap_whole` and
      `wide_characters_do_not_straddle_the_edge`.

- [x] **Alt + non-ASCII parsed as a plain character.** **Fixed:** the parser
      tracks the escape depth explicitly (`esc_count`) instead of inferring it
      from buffer contents. This also gained `ESC ESC [ C` and `CSI 1;3 C` for
      ALT-arrows, and `CtrlAlt` chords.

- [x] **Invalid UTF-8 lead bytes were silently swallowed.** **Fixed:**
      `begin_utf8` emits `KeySeq::Unknown` and a truncated sequence reprocesses
      the offending byte from the ground state, so no input vanishes.
      Guarded by `invalid_utf8_input_is_discarded`.

- [x] **`Config` fields that did nothing.** `history_max_size` was hardcoded to
      1024; `enable_hints` had no implementation.
      **Fixed:** the history is sized from the config (and resized by
      `set_config`), and hints are fully implemented via `HintProvider`.
      Guarded by `config_history_size_is_honoured`,
      `resizing_history_keeps_the_newest_entries`, `hints_are_advisory_only`.

- [x] **A write error closed stdin/stdout.** The `from_raw_fd … into_raw_fd`
      dance had `?` between the two calls, so an error dropped the `File` and
      closed fd 1. **Fixed:** every raw-fd use is gone. I/O goes through
      `terminal::write_all` / `read_input` on a `BorrowedFd`, which also retry
      short writes, `EINTR` and `EAGAIN`.

- [x] **History was lost if the bracketed-paste enable write failed.**
      **Fixed:** the history now lives on `Rustline` for the whole session and
      is never moved out, so no error path can drop it. The paste-mode writes
      are `let _ =` — a terminal that ignores the sequence must not cost the
      caller their line.

- [x] **`History::load` held `max_size - 1` entries and was O(n²).**
      **Fixed:** `VecDeque`, load reads then keeps the newest `max_size`.
      Guarded by `capacity_is_exactly_max_size` and
      `load_keeps_the_newest_entries_and_matches_add_capacity`.

- [x] **History files were written with default permissions.**
      **Fixed:** `OpenOptionsExt::mode(0o600)`.
      Guarded by `save_round_trips_and_is_private`, which asserts the mode.

- [x] **`should_continue_multiline` diverged from `IsBalanced`.** It counted only
      the current line and let the depth go negative.
      **Fixed:** `is_balanced` in `src/edit.rs` runs over the whole accumulated
      entry and clamps at zero. Guarded by `balance_matches_bestline` (unit) and
      `balance_mode_clamps_at_zero` (pty).

- [x] **`Ctrl-J` was conflated with `Ctrl-M`.** **Fixed:** `KeySeq::LineFeed` and
      `KeySeq::Enter` are distinct; `Ctrl-J` always starts a continuation line.
      Guarded by `linefeed_always_continues`.

- [x] **Dead error branch in `readline_with_history`.** **Fixed:** removed; the
      whole function was rewritten to derive `~/.{prog}_history` via
      `history_path` and to reload before saving so concurrent sessions merge
      instead of overwriting, as bestline does.

- [x] **`Alt-Y` rotated the kill ring without a prior yank.**
      **Fixed:** the rotate now lives inside the `last_yank` check.
      Guarded by `alt_y_without_a_yank_is_inert`.

- [x] **Signal handlers were installed and never restored.** **Fixed:**
      `RawMode` saves the previous `SigAction` for `SIGWINCH` and `SIGCONT` and
      restores both in `Drop`, alongside the termios.

- [x] **`SIGCONT` was tracked and ignored.** **Fixed:** `handle_signals` calls
      `RawMode::reenable` and redraws. `CTRL-Z` leaves raw mode, raises
      `SIGTSTP`, and re-enters on return.
      Guarded by `suspend_leaves_the_editor_usable`.

- [x] **`detect_size_ansi` could block forever and eat a keystroke.**
      **Fixed:** the DSR query is polled with a 100 ms timeout and is only
      reached after `TIOCGWINSZ` and the environment have both failed.
      Guarded by `terminal_size_on_a_pipe_falls_back_without_hanging`.

---

## P2 — Feature parity ✅ complete

### Keybindings

Every binding in bestline's README table is now implemented. 46 bindings, up
from the 11 that worked before.

| Binding | Before | Now | |
|---|---|---|---|
| Printable / UTF-8 insert, arrows, backspace, delete, TAB | ✅ | ✅ | |
| Ctrl-A/E/B/F/H/D/K/U/W/Y/L/N/P/C | ❌ dead | ✅ | P0 #1 |
| Home / End | ❌ no handler | ✅ | |
| Alt-B / Alt-F / Alt-D | ✅ | ✅ | |
| **Ctrl-R** reverse-i-search | ❌ | ✅ | full port incl. the underlined-prefix prompt |
| **Ctrl-G** cancel search | ❌ | ✅ | restores the pre-search line and cursor |
| **Alt-< / Alt->** history ends | ❌ | ✅ | |
| **Ctrl-T** transpose chars | ❌ | ✅ | multi-byte safe, unlike the C |
| **Alt-T** transpose words | ❌ | ✅ | |
| **Alt-U / Alt-L / Alt-C** case ops | ❌ | ✅ | |
| **Alt-\\** squeeze whitespace | ❌ | ✅ | |
| **Alt-H / Ctrl-Alt-H / Alt-Backspace** | ❌ | ✅ | |
| **Ctrl-Alt-B/F, Alt-←/→** expr movement | ❌ | ✅ | both `ESC ESC [ C` and `CSI 1;3 C` |
| **Ctrl-Space** set mark, **Ctrl-X Ctrl-X** goto | ❌ | ✅ | |
| **Ctrl-Z** suspend | ❌ | ✅ | |
| **Ctrl-\\** quit | ❌ | ✅ | restores the tty, then re-raises `SIGQUIT` |
| **Ctrl-S / Ctrl-Q** flow control | ❌ | ✅ | `tcflow` |
| **Ctrl-Q** escaped insert | ❌ | ✅ | |
| **Alt-Shift-B / Alt-Shift-S** barf / slurp | ❌ | ✅ | |
| Alt-Shift-R raise | ❌ | ✅ inert | bestline defines it as a no-op too |

### Library API

| Feature | Before | Now |
|---|---|---|
| Completion callback | ✅ trait | ✅ with explicit replacement ranges |
| History load / add / save | ✅ | ✅ `0600`, in-place editing of recalled entries |
| Bracketed paste | ⚠️ lost newlines | ✅ |
| Balance mode | ⚠️ diverged | ✅ matches `IsBalanced` |
| **Hints callback** | ❌ | ✅ `HintProvider`, also implemented for closures |
| **Mask mode** | ❌ | ✅ `Config::mask_mode`, `Rustline::read_password` |
| **Xlat callback** | ❌ | ✅ `Rustline::set_xlat` |
| **Paren mirror highlighting** | ❌ | ✅ `Config::highlight_brackets` |
| **Init string** | ❌ | ✅ `readline_with_init` |
| **Explicit fds** (`bestlineRaw`) | ❌ | ✅ `readline_raw` |
| **Unsupported-term fallback** | ❌ | ✅ `TERM` in `{dumb, cons25, emacs}` → plain reading |
| **Terminal resize handling** | ❌ | ✅ |
| **Editing history entries in place** | ❌ | ✅ full `bestlineEditHistoryGoto` semantics |

Two bestline entry points were **not** ported, for reasons rather than
oversight:

- **`bestlineUserIO`** — a hook to replace `read`/`write`/`poll` with your own
  function pointers. `readline_raw` taking explicit descriptors covers the real
  use case (driving the editor over a socket or a second tty) without a second
  indirection layer.
- **Llama mode** — recognizing `"""…"""` heredocs. That is application-specific
  syntax for one program, and `balance_pairs` plus `CTRL-J` covers multiline
  entry generally.

---

## P3 — Code quality ✅ all addressed

- [x] **`examples/basic.rs` did not compile**, which is why plain `cargo test`
      failed. Rewritten against the current API; both examples build in CI.
- [x] **Demo REPL moved out of the crate.** `src/main.rs` (838 lines) is now
      `examples/repl.rs`. It no longer keeps a second parallel history, persists
      to `~/.rustline_history`, exits via a return value instead of
      `process::exit`, and demonstrates hints, mask mode and multiline.
- [x] **Documentation.** `#![warn(missing_docs)]` at the crate root; every
      public item, module and non-obvious private helper documented.
      `cargo doc` is warning-free.
- [x] **`unsafe` blocks.** All 14 raw-fd blocks deleted outright. The five that
      remain (signal handling, two ioctls, one `pre_exec`) each carry a
      `// SAFETY:` comment, enforced by
      `clippy::undocumented_unsafe_blocks = "warn"` and
      `unsafe_op_in_unsafe_fn = "deny"` in `Cargo.toml`.
- [x] **`src/unicode.rs` dead code.** Everything is now wired up: `mirror_left`
      and `mirror_right` drive bracket highlighting and expression movement,
      the case helpers drive `ALT-U/L/C`. `is_separator` kept as the reasonable
      simplification of bestline's 800-line codepoint table — a unit test
      asserts it matches the C exactly for all 128 ASCII characters.
- [x] **`#[inline]` cargo cult** removed throughout.
- [x] **Duplicate `cursor_row`** gone with the renderer rewrite.
- [x] **O(n) buffer boundary helpers.** `backward`/`forward` are now O(1)
      (`chars().next_back()`), and the defensive re-alignment loops are gone
      because every mutator maintains the boundary invariant — which a property
      test now checks over arbitrary edit sequences.
- [x] **clippy `collapsible_match`** and every other lint fixed; the build is
      clean at `-D warnings`.

---

## Project hygiene ✅ all addressed

- [x] **Version control.** `git init` done, baseline committed before any change
      so the whole rewrite is bisectable.
- [x] **`Cargo.toml` metadata.** Description, `license = "BSD-2-Clause"`,
      repository, keywords, categories, `rust-version = "1.85"`, and
      `exclude = ["ref/", "rustline.md"]` so the reference C is not published.
- [x] **MSRV contradiction** resolved: `clippy.toml` and `Cargo.toml` both say
      1.85, and CI has a job that checks against exactly that toolchain.
- [x] **`.rustfmt.toml`** trimmed to stable-only options and moved to edition
      2024. `cargo fmt --check` is silent.
- [x] **`dirs`** now earns its place — `history_path` and `~` expansion in
      `FileCompleter`.
- [x] **`libc`** dropped as a direct dependency; `nix` covers everything.
- [x] **`deny.toml`** migrated to the current cargo-deny schema (`[graph]`, no
      removed keys) with a target matrix.
- [x] **`Makefile`** rewritten. There is no binary to install any more, so it is
      a dev-task runner matching CI: `check build test run fmt fmt-check clippy
      doc deny ci clean help`.
- [x] **`bin/rustline`** deleted — a 728 KB stale build artifact, committed in
      October, of a binary target that no longer exists. It is in the baseline
      commit if you want it back.
- [x] **CI** added at `.github/workflows/ci.yml`: test on Linux and macOS, lint
      (fmt + clippy + doc), an MSRV job, and cargo-deny.

⚠️ One environment note: `make` on your `PATH` resolves to
`~/.local/opt/cosmocc/bin/make`, a Cosmopolitan binary that hangs under wine in
this shell. `/usr/bin/make` works fine. That is unrelated to anything here, but
it will bite you when you run `make test`.

---

## Testing ✅ all gaps closed

Every technique the review asked for is now in place.

- **Pty integration harness** — `tests/common/mod.rs` spawns
  `examples/harness` on a real pseudoterminal via `openpty` + `setsid` +
  `TIOCSCTTY`, so `TIOCSWINSZ` genuinely delivers `SIGWINCH`. Every P0 finding
  is a named regression test in `tests/regressions.rs`.
- **Screen-model tests** — a small VT100 emulator (in `tests/common` and in the
  `display.rs` unit tests) renders the editor's byte stream to a cell grid and
  asserts on rows and cursor position. It implements *deferred* wrap, which the
  renderer depends on: writing into the last column leaves the cursor there with
  a pending wrap, and a following `\r` cancels it rather than skipping a row.
  Getting this wrong in the model was the source of five false failures while
  building it, which is itself the argument for having it.
- **Property tests** (`proptest`, dev-only) — arbitrary UTF-8 plus arbitrary
  sequences of all 22 buffer operations never panic and always leave the cursor
  on a character boundary; movement never edits; insert-then-rubout is identity;
  a kill returns exactly what it removed. For the parser: arbitrary bytes never
  panic and never wedge the state machine, and re-chunking the same bytes never
  changes the decoded keys.
- **Parser round-trip tests** — every escape sequence in bestline's dispatch,
  each also fed one byte at a time to prove the state machine resumes.
- **Table-driven keybinding tests** — `the_full_control_key_map_works` and
  `word_and_expression_editing` drive each binding end to end. The P0 case
  mismatch would have failed the first assertion.

---

## Found after implementation

- [x] **An application's `clear` command appeared to do nothing, then cleared
      the screen one command later.** Reported from a Fedora console and
      reproduced on the pty. **[verified]**

      Two layers. The examples printed the escape with `print!("\x1b[H\x1b[2J")`,
      which has no trailing newline, so Rust's `LineWriter` left it in the
      buffer. Underneath that, the editor writes straight to the descriptor and
      bypasses the buffer behind `std::io::stdout()` entirely, so the prompt
      overtook the pending text and the escape only escaped later, when some
      `println!` happened to flush it.

      This was never specific to `clear`: any caller using `print!` without a
      newline before a prompt would have seen their output surface at the wrong
      moment. bestline avoids it with `fflush(stdout)` in `bestlineInit` before
      entering raw mode (`bestline.c:3845`); the port had dropped that line.

      **Fixed:** `readline_raw` flushes `std::io::stdout()` before touching the
      terminal, and a public `rustline::clear_screen()` writes the sequence
      straight to the descriptor so applications need not think about buffering
      at all. Both examples use it.
      Guarded by `buffered_caller_output_is_flushed_before_the_prompt`.

---

## Deliberate divergences from bestline

Kept, and each for a reason:

1. **No global state.** bestline keeps `history[]`, `ring`, `rawmode`,
   `maskmode`, `gotwinch` and the mode flags in file-scope statics, which limits
   it to one prompt per process. Everything here belongs to a `Rustline` value.
2. **Control characters inside a bracketed paste are inserted or ignored, never
   obeyed.** bestline runs its full key dispatch during a paste, so a pasted tab
   triggers completion and a pasted `CTRL-U` erases the line. Preventing exactly
   that is what bracketed paste is for.
3. **`TAB` inserts the common prefix first, then lists, then cycles.** bestline
   cycles immediately. This is the behaviour readline and bash have trained
   everyone to expect, and cycling is still available on the next press.
4. **`CTRL-C` returns `Err(Interrupted)`** rather than re-raising `SIGINT`, so
   the caller decides. `CTRL-\` does re-raise, because there is no sensible
   alternative to the tty's `VQUIT` behaviour.
5. **Character-width wrapping.** bestline wraps on `x + rune.n > xn`, comparing a
   column against a *byte* length — which wraps one cell early for any
   multi-byte character. This uses the display width.
6. **`unicode-width`** rather than a hand-maintained codepoint table.
7. **Transpose is multi-byte safe.** bestline's `bestlineEditTranspose`
   decrements a byte index and then reads a rune from it, which lands mid
   character at the end of a line containing non-ASCII text.

## Where the code stands

```
src/unicode.rs     237   widths (ANSI-aware), separators, mirrors, case
src/parser.rs      577   resumable ANSI + UTF-8 key parser
src/buffer.rs      932   line buffer and every editing operation
src/history.rs     316   storage, search, persistence
src/completion.rs  398   providers, ranges, column formatting
src/hints.rs        97   hint providers
src/display.rs     701   the renderer
src/terminal.rs    532   raw mode, signals, size, interruptible I/O
src/state.rs       238   per-session state, kill ring
src/edit.rs        738   the editing loop and key dispatch
src/error.rs        57   error type
src/lib.rs         553   public API
```

Nothing from the review list is outstanding.
