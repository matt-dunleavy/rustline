# Rustline — Code Review & Roadmap

**Reviewed:** 2026-08-30
**Rust port:** `src/` (2,541 lines across 11 modules)
**Reference:** `ref/bestline-master/bestline.c` (4,101 lines, single file) + `bestline.h`

## How this review was done

Every finding marked **[verified]** was reproduced by driving `target/debug/rustline`
inside a real pseudo-terminal (`pty.fork()` + `TIOCSWINSZ`) and capturing the raw byte
stream, not just by reading code. Findings marked **[by inspection]** are read from the
source and are not yet reproduced. The repro harness is in the Appendix.

---

## Verdict

The architecture is good — the module split (`parser` / `buffer` / `display` /
`history` / `completion` / `terminal` / `state`) is a genuine improvement over
bestline's single 4kloc file, and the ANSI state-machine parser is the right call
(bestline's own README lists "generalized parsing" as its headline fix over linenoise).
The UTF-8 handling in `buffer.rs` and the `unicode-width` integration are correct.

But the port is **not currently usable as a line editor**. A single-character bug in
`src/parser.rs:200` disables *every* control-key binding, which is most of what a
readline replacement is. On top of that, resizing the terminal kills the process,
two tab-completion paths panic, and pasted or fast-typed input is silently dropped.

Feature parity with bestline is roughly **30%** by keybinding count, and the library
API surface is missing hints, mask mode, history search, and the callback hooks that
`bestline.h` exposes.

Current test state: `cargo test` **does not build** (`examples/basic.rs` is broken);
`cargo test --lib` is **4 passed / 2 failed**. Both failing tests are correct — the
code is wrong, not the tests.

---

## P0 — Blockers (the editor does not work without these)

- [ ] **Ctrl-key case mismatch disables all control bindings.** `src/parser.rs:200`
      emits `Ctrl((byte + b'@') as char)`, so `0x01` becomes `Ctrl('A')` — **uppercase**.
      Every arm in `handle_key` matches lowercase: `Ctrl('a')`, `Ctrl('e')`, `Ctrl('c')`,
      `Ctrl('d')` … (`src/lib.rs:202`–`272`). Nothing matches; all fall through to
      `_ => {}`.
      **Impact:** Ctrl-A/B/D/E/F/H/K/L/N/P/U/W/Y are all dead. You cannot interrupt
      (Ctrl-C), cannot EOF (Ctrl-D), cannot kill or yank.
      **Fix:** normalize in the parser — `Ctrl((byte + b'`') as char)` for `0x01..=0x1a`,
      or `.to_ascii_lowercase()` on the produced char. Add a parser unit test asserting
      `parse(&[0x01]) == [Ctrl('a')]`. **[verified]**

      ```
      typed: "abc" then 0x01 (Ctrl-A) then "X"
      got:   rustline:1> abcX     ← Ctrl-A ignored, X appended at end
      want:  rustline:1> Xabc
      ```

- [ ] **Terminal resize terminates the program.** `poll(2)` is never restarted on
      `EINTR`, and `poll` is not restartable via `SA_RESTART`. The `SIGWINCH` handler
      fires, `wait_for_input` (`src/terminal.rs:142`) returns `Err(EINTR)`, and
      `src/lib.rs:139` propagates it straight out of `readline`.
      **Impact:** resizing the terminal window at the prompt kills the session.
      **Fix:** retry on `EINTR` in both `wait_for_input` and the `read` at
      `src/lib.rs:144`; on wakeup, check `check_sigwinch()` and re-query the window
      size before refreshing. **[verified]**

      ```
      after resize: ❌ Readline error: Nix(EINTR)
                    🦀 Thanks for using Rustline REPL!
      ```

- [ ] **Two panics in tab completion.**
      - `src/lib.rs:332` — `find_common_prefix_length` returns a **char** count
        (`src/lib.rs:383`, `common_len = i + 1` over `chars().enumerate()`) but is used
        as a **byte** index into `&candidates[0][..common_len]`.
        Repro: two files `日本.txt` / `日月.txt`, type `cat 日<Tab>` →
        `end byte index 1 is not a char boundary; it is inside '日'`.
      - `src/lib.rs:360` — `&completion[current.len()..]` assumes the completion is
        longer than the typed word and shares its prefix. Neither is guaranteed.
        Repro: `cat ././b<Tab>` → `start byte index 5 is out of bounds for string of
        length 4`.
      **Fix:** compute the common prefix over byte offsets at char boundaries, and
      make the completer return an explicit `(replace_range, replacement)` rather than
      having the caller re-derive the word boundary. **[verified]**

- [ ] **Divide by zero when the terminal reports 0 columns.** `src/display.rs:87`
      (`(prompt_width + line_width) / cols`) and `src/lib.rs:439`/`441`.
      `get_terminal_size` (`src/terminal.rs:73`) returns whatever `ioctl` gives it,
      including `ws_col == 0`, which happens on some pty/CI setups.
      bestline clamps: `if (!ws.ws_col) ws.ws_col = 80; if (!ws.ws_row) ws.ws_row = 24;`
      and also honours `$COLUMNS`/`$ROWS` (`bestline.c:2197`).
      **Fix:** mirror that clamping in `get_terminal_size`. **[verified]**

      ```
      thread 'main' panicked at src/display.rs:87:26: attempt to divide by zero
      ```

- [ ] **Input is silently dropped when several keys arrive in one `read`.**
      `process_keys` (`src/lib.rs:169`–`181`) returns as soon as one key yields a line
      and **discards the rest of the batch**.
      Repro: one write of `echo one\recho two\r` → only `one` runs; `echo two` vanishes.
      **Fix:** keep an unconsumed-key queue on `EditorState` and drain it at the top of
      the next `edit_loop` iteration before reading again. **[verified]**

- [ ] **Bracketed paste discards newlines.** `handle_paste_char` (`src/lib.rs:184`)
      only handles `KeySeq::Char`; `KeySeq::Enter` inside a paste is thrown away.
      Repro: paste `echo aa\recho bb` → buffer becomes `echo aaecho bb`.
      bestline's `pastemode` sets `is_finished = 0` on `\r` and appends a real newline
      to the accumulated buffer (`bestline.c:3486`).
      **Fix:** in paste mode, `\r`/`\n` should push the current line into
      `multiline_buffer` and continue, exactly like the multiline path. **[verified]**

---

## P1 — Correctness

- [ ] **`refresh_line` moves the cursor up by the wrong amount on wrapped lines.**
      `src/lib.rs:418` unconditionally emits `move_up(rows_used - 1)`, which assumes the
      cursor was parked on the **last** row of the previous render. bestline emits
      `\033[{l->rows - l->oldpos - 1}A` (`bestline.c:2661`) — it tracks which row the
      cursor actually ended on.
      Ironically `Display::calculate_metrics` computes exactly this value into
      `display.cursor_row` (`src/display.rs:90`) and then **never uses it**;
      `refresh_line` recomputes a *different* quantity into a local also called
      `cursor_row` (`src/lib.rs:439`, rows *above* vs. rows *below*). Two meanings, one
      name.
      **Impact:** any refresh that happens while the cursor sits above the last row of a
      wrapped line redraws the prompt one row too high and overwrites scrollback. Today
      this is partly masked because Ctrl-A/Home don't work; fixing P0 #1 will expose it.
      **Fix:** persist the rendered cursor row on `Display` and use it for the up-move;
      delete the duplicate local. **[verified via escape-sequence trace]**

      ```
      100 a's at 80 cols, 40× Left (cursor now on row 1 of 2), then type 'Z':
      emitted: \r \x1b[1A ...   ← moves up from row 1 to row 0, above the prompt
      ```

- [ ] **`FileCompleter` looks in the wrong directory for any path with a separator.**
      `src/completion.rs:61` sets the word start by scanning back for whitespace **or
      `/`**, so `cat src/buf` yields `word = "buf"`, and the `word.rfind('/')` at
      `src/completion.rs:68` then finds nothing and searches `.` instead of `src/`.
      Repro: `cat src/buf<Tab>` → nothing happens. `cat ./b<Tab>` → `cat ./b/`
      (should be `cat ./bin/`).
      **Fix:** stop at whitespace only, then split the word on the last `/` yourself.
      **[verified]**

- [ ] **Prompt width counts ANSI escape bytes as printable columns.**
      `src/lib.rs:434` uses `UnicodeWidthStr::width(&state.prompt)`. bestline's
      `GetMonospaceWidth` (`bestline.c:1973`) is a small state machine that skips
      `ESC`/CSI sequences precisely so prompts can be coloured — and `bestline.h`
      documents that "prompt may contain ansi escape sequences, color, utf8, etc."
      **Impact:** any coloured prompt puts the cursor in the wrong column.
      **Fix:** port `GetMonospaceWidth` into `unicode.rs` and use it for both prompt and
      hint widths. **[by inspection]**

- [ ] **`refresh_line` relies on terminal auto-wrap and misses the last-column case.**
      It writes prompt+buffer as one blob. bestline walks runes and emits an explicit
      `\033[K\r\n` at each wrap point, tracks `x` per cell so a wide char that doesn't
      fit in the final column wraps correctly, and emits `\n\r` when the cursor lands at
      `pos == len && x >= xn` (`bestline.c:2707`). Without that last case the cursor
      sticks in column 79 and the next character overwrites.
      **Fix:** port the rune-walking render loop. This is the single largest remaining
      gap in `display.rs`. **[by inspection]**

- [ ] **Alt + non-ASCII is parsed as a plain char.** `src/parser.rs:177` guards on
      `self.buffer.len() == 1 && self.buffer[0] == 0x1b`, but the escape-state branch at
      `src/parser.rs:97` has already pushed the UTF-8 lead byte into `self.buffer`, so
      the length is 2. `test_alt_utf8` fails for exactly this reason.
      **Fix:** record "we came from `Escape`" as an explicit flag rather than inferring
      it from buffer contents. **[verified — failing test]**

- [ ] **Invalid UTF-8 lead bytes are silently swallowed.** `start_utf8_sequence`
      (`src/parser.rs:215`) returns to `Ground` without emitting anything when the byte
      matches no lead pattern (`0xFF`, `0xFE`, or a stray continuation byte).
      `test_invalid_utf8` fails.
      **Fix:** emit `KeySeq::Unknown(vec![byte])` before returning. **[verified —
      failing test]**

- [ ] **`Config` fields that do nothing.**
      - `history_max_size` — `EditorState::new` hardcodes `History::new(1024)`
        (`src/state.rs:65`). Only `readline_with_history` honours the config value.
      - `enable_hints` — no hint support exists anywhere in the crate.
      **Fix:** thread the config into `EditorState::new`, and either implement hints or
      remove the flag until you do. **[by inspection]**

- [ ] **A write error closes stdin/stdout.** The `from_raw_fd` … `into_raw_fd` dance
      appears at `src/lib.rs:110`, `120`, `125` and `src/display.rs:53`. In three of
      those the `?` sits **between** the two calls, so an error drops the `File` and
      **closes fd 1**:
      ```rust
      let mut stdout_file = unsafe { std::fs::File::from_raw_fd(stdout) };
      stdout_file.write_all(b"\x1b[?2004h")?;   // ← early return closes stdout
      let _ = stdout_file.into_raw_fd();
      ```
      **Fix:** use `std::mem::ManuallyDrop`, or just `std::io::stdout()` /
      `nix::unistd::write(BorrowedFd, …)` (already used elsewhere in `terminal.rs`) and
      drop the raw-fd juggling entirely. **[by inspection]**

- [ ] **History is lost if the bracketed-paste enable write fails.**
      `self.history.take()` happens at `src/lib.rs:105`, before the fallible write at
      `src/lib.rs:111`. An early return there leaves `self.history == None` and drops
      every entry.
      **Fix:** take the history after all fallible setup, or restore it in a guard.
      **[by inspection]**

- [ ] **`History::load` holds `max_size - 1` entries.** `src/history.rs:101` pushes and
      *then* trims on `>=`, whereas `History::add` (`src/history.rs:35`) trims on `>`.
      Off-by-one, and inconsistent between the two paths. Also `Vec::remove(0)` per line
      makes loading O(n²) — bestline mmaps the file and calls this out as a "10x faster"
      improvement over linenoise. Use `VecDeque`, or read all lines then keep the tail.
      **[by inspection]**

- [ ] **History files are written with default permissions.** `History::save`
      (`src/history.rs:111`) does a plain `OpenOptions`. bestline sets a restrictive
      umask around `fopen` and then `chmod(filename, S_IRUSR | S_IWUSR)`
      (`bestline.c:3691`) — history routinely contains secrets.
      **Fix:** `OpenOptionsExt::mode(0o600)`. **[by inspection]**

- [ ] **`should_continue_multiline` diverges from `IsBalanced`.** `src/lib.rs:451`
      counts parens **on the current line only** and lets the depth go negative.
      bestline's `IsBalanced` (`bestline.c:3339`) runs over the whole accumulated buffer
      and clamps at zero (`else if (d > 0 && buf->b[i] == ')')`).
      Concretely: `((a` ⏎ `b)` → bestline continues (depth 1), Rustline submits.
      And `)(` → bestline says unbalanced, Rustline says balanced.
      **Fix:** run the check over `multiline_buffer.join("\n") + line` and clamp.
      **[by inspection]**

- [ ] **`Ctrl-J` (`\n`) is conflated with `Ctrl-M` (`\r`).** `src/parser.rs:199` maps
      both `0x0A` and `0x0D` to `KeySeq::Enter`. bestline treats `\n` as an
      *unconditional* line continuation and `\r` as submit (`bestline.c:3470` vs
      `3486`) — that's how you force a newline without balanced parens.
      **Fix:** split into `KeySeq::Enter` and `KeySeq::LineFeed`. **[by inspection]**

- [ ] **Dead error branch.** `readline_with_history` (`src/lib.rs:492`–`500`) carefully
      distinguishes `NotFound`, but `History::load` (`src/history.rs:90`) already
      swallows `NotFound` and returns `Ok(())`. The whole match is unreachable.
      **[by inspection]**

- [ ] **`Alt-Y` rotates the kill ring even when there is no prior yank.**
      `src/lib.rs:241` calls `kill_ring.rotate()` before checking `last_yank`. bestline
      guards on the previous keystroke first (`bestline.c:3041`).
      **Fix:** move the rotate inside the `if let Some(yank_state)`. **[by inspection]**

- [ ] **Signal handlers are installed but never restored.** `RawMode::enable`
      (`src/terminal.rs:49`) overwrites `SIGWINCH`/`SIGCONT` and `Drop`
      (`src/terminal.rs:58`) only restores the termios. bestline saves the old
      `sigaction` and restores both in `bestlineDisableRawMode` (`bestline.c:2099`).
      **[by inspection]**

- [ ] **`SIGCONT` is tracked and ignored.** `GOT_SIGCONT` is set but `check_sigcont()`
      is `#[allow(unused)]` and never called. bestline re-enters raw mode on
      foregrounding — one of its advertised fixes over linenoise. Without this,
      suspending and resuming leaves the terminal cooked. **[by inspection]**

- [ ] **`detect_size_ansi` can block forever and eat a keystroke.**
      `src/terminal.rs:95` issues a DSR query and does one unbounded blocking `read`.
      If the terminal never answers, `readline` hangs; if the user types first, that
      keystroke is consumed as the reply.
      **Fix:** poll with a short timeout, and check `$COLUMNS`/`$ROWS` before falling
      back to DSR, as bestline does. **[by inspection]**

---

## P2 — Feature parity with bestline

### Keybindings

Source of truth: the shortcut table in `ref/bestline-master/README.md`.

| Binding | bestline | Rustline | Note |
|---|---|---|---|
| Printable / UTF-8 insert | ✅ | ✅ | verified working |
| ← → Left/Right | ✅ | ✅ | verified working |
| ↑ ↓ history | ✅ | ✅ | verified working |
| Alt-B / Alt-F word move | ✅ | ✅ | verified working |
| Backspace (`0x7f`) | ✅ | ✅ | verified working |
| Delete (`\e[3~`) | ✅ | ✅ | verified working |
| Tab completion | ✅ | ⚠️ | works for simple words; panics / misfires on paths |
| Alt-D kill word forward | ✅ | ✅ | |
| Alt-Y rotate + yank | ✅ | ⚠️ | rotates without a prior yank |
| Ctrl-A/E/B/F/H/D/K/U/W/Y/L/N/P/C | ✅ | ❌ | **all dead — P0 #1** |
| **Home / End keys** | ✅ | ❌ | parser emits `KeySeq::Home`/`End`; `handle_key` has no arm — falls through `_ => {}` **[verified]** |
| Insert / PageUp / PageDown / F1-F12 | — | ❌ | parsed, no handler (bestline ignores these too) |
| **Ctrl-R reverse-i-search** | ✅ | ❌ | `History::search_backward` exists (`src/history.rs:74`) and is **never called** |
| Ctrl-G cancel search | ✅ | ❌ | |
| Alt-< / Alt-> begin/end of history | ✅ | ❌ | |
| Ctrl-T transpose chars | ✅ | ❌ | |
| Alt-T transpose words | ✅ | ❌ | |
| Alt-U / Alt-L / Alt-C case ops | ✅ | ❌ | `unicode::to_uppercase`/`to_lowercase` exist, unused |
| Alt-\ squeeze whitespace | ✅ | ❌ | |
| Alt-H / Ctrl-Alt-H kill word back | ✅ | ❌ | |
| Ctrl-Alt-B / Ctrl-Alt-F expr move | ✅ | ❌ | |
| Alt-← / Alt-→ expr move | ✅ | ❌ | |
| Ctrl-Space set mark, Ctrl-X Ctrl-X goto | ✅ | ❌ | |
| Ctrl-Z suspend | ✅ | ❌ | ISIG is off, so Ctrl-Z is simply swallowed |
| Ctrl-\ quit | ✅ | ❌ | |
| Ctrl-S / Ctrl-Q flow control | ✅ | ❌ | |
| Ctrl-Q escaped insert | ✅ | ❌ | |
| Barf / slurp / raise (Ctrl-C Ctrl-B/S/R) | ✅ | ❌ | paredit-style s-expr editing |

### Library API (`bestline.h`)

| Feature | Status |
|---|---|
| Completion callback | ✅ `CompletionProvider` trait — nicer than the C callback |
| History load / add / save | ✅ present |
| Bracketed paste | ⚠️ enabled, but drops newlines |
| Balance mode | ⚠️ partial, diverges from `IsBalanced` |
| **Hints callback** | ❌ not implemented (`Config::enable_hints` is inert) |
| **Mask mode** (password input) | ❌ |
| **Xlat callback** (input transliteration) | ❌ |
| **Paren mirror highlighting** | ❌ `unicode::mirror_left`/`mirror_right` exist, unused |
| Llama mode (`"""` heredocs) | ❌ |
| Emacs mode toggle | ❌ |
| `init` string (pre-filled buffer) | ❌ |
| `bestlineRaw` (explicit fds) | ❌ `readline` is hardcoded to stdin/stdout |
| `bestlineUserIO` (I/O hooks) | ❌ |
| Unsupported-term fallback | ❌ bestline checks `TERM` against `{dumb, cons25, emacs}` (`bestline.c:2046`) and falls back to `fgets` |
| Terminal resize handling | ❌ window size captured once at `src/lib.rs:101`, never refreshed |
| Editing history entries in place | ❌ bestline writes the edited buffer back into the history slot (`bestlineEditHistoryGoto`, `bestline.c:2322`); Rustline only keeps a single `temp_entry` |

---

## P3 — Code quality & structure

- [ ] **`examples/basic.rs` does not compile** — this is why plain `cargo test` fails.
      It references `ReadlineError` (the type is `RustlineError`), `rl.add_history_entry`,
      `History::iter`, and `History::is_empty` — none of which exist. Either add those
      methods to the public API (they're reasonable) or fix the example. Add a CI step
      that builds examples so this can't rot again.

- [ ] **Move the demo REPL out of the crate.** `src/main.rs` is 838 lines of showcase
      REPL — banner art, emoji, a calculator, a benchmark command — sitting in what
      should be a library crate. Move it to `examples/repl.rs`. It also keeps a
      *second*, parallel `history: Vec<String>` (`src/main.rs:117`) that the `history`
      builtin displays instead of the real one, and it never loads or saves
      `~/.rustline_history` at all — so the flagship `bestlineWithHistory` workflow is
      undemonstrated.

- [ ] **Zero documentation.** No `//!` module docs, no `///` on a single public item
      across 2,541 lines. `clippy.toml` sets `missing-docs-in-crate-items = true`, but
      that lint is allow-by-default so it never fires. Add `#![warn(missing_docs)]` to
      `lib.rs` and write them — bestline's public functions all carry doc comments worth
      porting.

- [ ] **14 `unsafe` blocks, 0 `// SAFETY:` comments.** Most are the `from_raw_fd`
      pattern that P1 says to delete anyway; the rest are `BorrowedFd::borrow_raw` and
      the `signal`/`ioctl` calls. Add `#![deny(clippy::undocumented_unsafe_blocks)]`
      once they're annotated.

- [ ] **`src/unicode.rs` is 66 lines of almost entirely dead code.** `Rune`,
      `to_lowercase`, `to_uppercase`, `mirror_left`, `mirror_right` are all
      `#[allow(unused)]`. They're placeholders for unimplemented features (case ops,
      paren matching) — fine, but the `#[allow(unused)]` sprinkling hides real rot.
      Either wire them up as part of P2 or delete them until needed.
      Note `is_separator` (`!c.is_alphanumeric()`) is a **reasonable** simplification of
      bestline's 800-line codepoint table — the ASCII behaviour matches exactly
      (`bestline.c:541`) and Unicode `is_alphanumeric` is close enough. Keep it.

- [ ] **`#[inline]` is cargo-culted throughout `src/main.rs`** — on `print_help`,
      `display_history`, `handle_ls_command`… Non-generic, non-trivial, called once per
      keystroke at most. No benefit; drop them.

- [ ] **Duplicate / confusing `cursor_row`.** See P1. `Display::cursor_row` is written
      and never read; `refresh_line` has a local of the same name with the opposite
      meaning.

- [ ] **`LineBuffer::prev_char_boundary` / `next_char_boundary` are O(n) and
      over-defensive** (`src/buffer.rs:140`–`168`). They scan to realign an already-aligned
      cursor, then do `char_indices().last()` — a full re-scan of the prefix on *every
      left-arrow*. `self.data[..self.cursor].chars().next_back().map_or(0, |c| self.cursor - c.len_utf8())`
      is O(1) and clearer. The realignment loops are dead if the cursor invariant holds —
      and it does, since every mutator maintains it.

- [ ] **One live clippy warning:** `collapsible_match` at `src/lib.rs:299`.
      `cargo clippy` is otherwise clean.

- [ ] **`main.rs` exits via `std::process::exit(0)`** inside `process_command`
      (`src/main.rs:701`), skipping the goodbye banner and any `Drop`. Return a
      "should quit" signal instead.

---

## Project hygiene

- [ ] **The repository is not under version control.** `git rev-parse` fails — there is
      no `.git` anywhere up to the mount point, despite a fully populated `.gitignore`.
      `git init` before anything else; this review lists changes you'll want to be able
      to bisect.

- [ ] **`Cargo.toml` has no publishable metadata** — no `description`, `license`,
      `repository`, `authors`, `keywords`, `categories`, or `rust-version`. Given
      `NOTICE` correctly carries the BSD-2 chain from bestline/linenoise, set
      `license = "BSD-2-Clause"`.

- [ ] **MSRV contradiction.** `clippy.toml` says `msrv = "1.70.0"`; `Cargo.toml` says
      `edition = "2024"`, which requires 1.85+. Pick one and add `rust-version` to
      `Cargo.toml` so cargo enforces it.

- [ ] **`.rustfmt.toml` is nightly-only.** 16 of its options are unstable, so
      `cargo fmt` on the pinned `stable` toolchain prints 16 warnings and silently
      ignores them (`imports_granularity`, `group_imports`, `format_strings`, …).
      Either pin `channel = "nightly"` in `rust-toolchain.toml` or trim the config to
      stable options.

- [ ] **`dirs` is an unused dependency.** Nothing in `src/` references it — the natural
      user is `readline_with_history`, which currently takes a raw path instead of
      deriving `~/.{prog}_history` the way `bestlineWithHistory` does
      (`bestline.c:3889`). Either use it for that or drop it.

- [ ] **`libc` is pulled in for a single `isatty` call** (`src/lib.rs:95`).
      `nix::unistd::isatty` already covers it; drop the direct dependency.

- [ ] **`deny.toml` uses the deprecated cargo-deny schema.** `[advisories] vulnerability
      / unmaintained / notice` and `[licenses] unlicensed / copyleft /
      allow-osi-fsf-free / default` were removed in cargo-deny 0.14+. It will warn or
      error on a current version.

- [ ] **`Makefile` hardcodes `INSTALL_DIR = /home/matt/bin`** — wrong user, and it
      should be `$(HOME)/bin` or `$(PREFIX)`. It also has a `.PHONY: fmt` label above a
      target actually named `format`.

- [ ] **No `tests/` directory and no CI.** 6 unit tests total, none covering
      `display.rs`, `buffer.rs`, `state.rs`, or `terminal.rs`. See below.

---

## Suggested order of work

**Milestone 1 — make it work.** All of P0, plus the `refresh_line` cursor-up fix.
That's ~6 focused changes and takes the port from "unusable" to "a working
Emacs-mode line editor". Land the parser fix first; it's one line and unblocks
manual testing of everything else.

**Milestone 2 — make it correct.** The rest of P1. The big one is porting bestline's
rune-walking render loop and `GetMonospaceWidth` into `display.rs` — that's what makes
wide characters, coloured prompts, and window edges behave.

**Milestone 3 — parity.** Ctrl-R search first (the plumbing already exists in
`history.rs`), then hints, then mask mode, then the case/transpose/squeeze family. The
paredit barf/slurp/raise commands are the last 10% and can wait indefinitely.

**Milestone 4 — ship it.** Docs, metadata, examples that build, `tests/`, CI.

## Testing gaps worth closing

The pty harness in the Appendix found six bugs in about twenty minutes; most of them
are invisible to unit tests because they only manifest as terminal byte streams. Worth
building into `tests/`:

- **A pty integration harness.** Spawn the binary under a pty, send bytes, assert on the
  output stream. Every P0 finding becomes a regression test.
- **A screen-model test.** Feed `refresh_line`'s output into a terminal emulator model
  (the `vt100` crate, or `pyte` via a script) and assert the rendered grid and cursor
  position. This is the only practical way to test wrapping, wide chars, and the
  cursor-up arithmetic.
- **Property tests on `LineBuffer`** (`proptest`/`quickcheck`): arbitrary UTF-8 plus an
  arbitrary sequence of edits should never panic, and the cursor should always land on
  a char boundary.
- **Parser round-trip tests** covering every escape sequence in the bestline
  `bestlineEdit` switch, plus split-across-reads inputs (a CSI sequence arriving one
  byte at a time — the state machine handles it, but nothing tests it).
- **Table-driven keybinding tests** asserting `KeySeq → buffer state`, which would have
  caught the P0 case mismatch immediately.

## Things the port already does better than bestline

Worth preserving as you fix the above:

- Module boundaries instead of one 4,101-line file.
- `CompletionProvider` as a trait rather than a global C function pointer — no
  process-wide mutable state.
- `RawMode` as an RAII guard, so termios is restored on unwind. bestline needs an
  `atexit` hook for this (`bestline.c:3663`).
- No global state at all: bestline keeps `history[]`, `ring`, `rawmode`, `maskmode`,
  `gotwinch` and friends as file-scope statics, which is why it can only drive one
  prompt per process.
- Typed errors via `thiserror` instead of `-1`/`errno`.
- `unicode-width` instead of a hand-maintained 800-line codepoint table.

---

## Appendix — pty repro harness

The harness used for this review. Save as `tests/pty_probe.py` or keep in a scratch dir.

```python
import os, pty, time, select, fcntl, termios, struct

BIN = "target/debug/rustline"

def spawn(rows=24, cols=80):
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.execv(BIN, ["rustline"])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    return pid, fd

def drain(fd, t=0.5):
    out = b""
    while True:
        r, _, _ = select.select([fd], [], [], t)
        if not r:
            break
        try:
            c = os.read(fd, 65536)
        except OSError:
            break
        if not c:
            break
        out += c
        t = 0.25
    return out

pid, fd = spawn()
drain(fd, 1.0)                      # swallow banner + first prompt
os.write(fd, b"abc\x01X")           # type abc, Ctrl-A, X
time.sleep(0.4)
print(drain(fd).decode("utf8", "replace").replace(chr(27), "ESC"))
os.kill(pid, 9); os.waitpid(pid, 0)
```

Probes that found the P0 issues:

| Probe | Expected | Actual |
|---|---|---|
| `b"abc\x01X"` | `Xabc` | `abcX` — Ctrl-A dead |
| `TIOCSWINSZ` mid-prompt | redraw at new width | `Readline error: Nix(EINTR)`, process exits |
| `TIOCSWINSZ` to 0×0 | clamp to 80×24 | panic, `display.rs:87`, divide by zero |
| `b"echo one\recho two\r"` | both run | only `one` runs |
| `b"\x1b[200~echo aa\recho bb\x1b[201~"` | two lines | `echo aaecho bb` |
| `cat 日<Tab>` (`日本.txt`,`日月.txt` in cwd) | common prefix | panic, `lib.rs:332`, char boundary |
| `cat ././b<Tab>` | `cat ././bin/` | panic, `lib.rs:360`, index out of bounds |
| `cat src/buf<Tab>` | `cat src/buffer.rs` | nothing |
| `\x1b[H` (Home) | cursor to col 0 | nothing |
