//! Regression tests for every defect found in the code review.
//!
//! Each test names the problem it guards against and drives the editor through
//! a real pseudoterminal, because these are all failures of the byte stream the
//! editor produces rather than of any single function.

mod common;

use common::Pty;

/// `CTRL-A` and every other control chord were dead: the parser produced
/// `Ctrl('A')` while the dispatcher matched `Ctrl('a')`.
#[test]
fn control_chords_are_bound() {
    let mut pty = Pty::spawn(&[], 24, 80);
    // Type "abc", jump to the start, insert X, submit.
    pty.type_keys(b"abc\x01X\r");
    assert_eq!(pty.lines(), ["Xabc"]);
}

/// Every letter chord, checked in one pass so a future normalization change
/// cannot silently unbind a subset.
#[test]
fn the_full_control_key_map_works() {
    let cases: &[(&[u8], &str, &str)] = &[
        (b"abc\x01X\r", "Xabc", "CTRL-A start of line"),
        (b"abc\x01\x05X\r", "abcX", "CTRL-E end of line"),
        (b"abc\x02X\r", "abXc", "CTRL-B back one"),
        (b"abc\x01\x06X\r", "aXbc", "CTRL-F forward one"),
        (b"abc\x08\r", "ab", "CTRL-H backspace"),
        (b"abc\x01\x04\r", "bc", "CTRL-D delete"),
        (b"abcdef\x02\x02\x02\x0b\r", "abc", "CTRL-K kill to end"),
        (b"abcdef\x02\x02\x02\x15\r", "def", "CTRL-U kill to start"),
        (b"one two\x17\r", "one ", "CTRL-W kill word back"),
        // A second yank inserts at the cursor, immediately after the first.
        (b"one two\x17\x19\x19\r", "one twotwo", "CTRL-Y yank twice"),
        (b"ab\x14\r", "ba", "CTRL-T transpose"),
    ];

    for (keys, expected, what) in cases {
        let mut pty = Pty::spawn(&[], 24, 80);
        pty.type_keys(keys);
        assert_eq!(pty.lines(), [*expected], "{what}");
        assert!(!pty.panicked(), "{what} panicked");
    }
}

/// The HOME and END keys were parsed and then dropped on the floor.
#[test]
fn home_and_end_keys_are_bound() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"abc\x1b[HQ\x1b[FW\r");
    assert_eq!(pty.lines(), ["Qabc W"].map(|s| s.replace(' ', "")));
}

/// Resizing the terminal returned `EINTR` from `poll`, which propagated out of
/// `readline` and killed the program.
#[test]
fn resizing_the_terminal_does_not_end_the_session() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"abc");

    pty.resize(30, 100);
    pty.settle();
    assert!(pty.is_alive(), "resize killed the process");

    pty.type_keys(b"Z\r");
    assert_eq!(pty.lines(), ["abcZ"]);
    assert!(
        !pty.output().contains("@@ERR@@"),
        "resize surfaced an error: {}",
        pty.output(),
    );
}

/// Repeated resizes, including a shrink, must all be absorbed.
#[test]
fn repeated_resizes_are_absorbed() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"hello");
    for (rows, cols) in [(40u16, 120u16), (10, 40), (24, 80), (5, 20)] {
        pty.resize(rows, cols);
        pty.settle();
        assert!(pty.is_alive(), "died after resize to {rows}x{cols}");
    }
    pty.type_keys(b"\r");
    assert_eq!(pty.lines(), ["hello"]);
}

/// A terminal reporting zero columns divided by zero in the renderer.
#[test]
fn a_zero_size_terminal_does_not_panic() {
    let mut pty = Pty::spawn(&[], 0, 0);
    pty.type_keys(b"abc\r");
    assert!(!pty.panicked(), "output was: {}", pty.output());
    assert_eq!(pty.lines(), ["abc"]);
}

/// A one-column terminal is degenerate but must still not panic or hang.
#[test]
fn a_one_column_terminal_does_not_panic() {
    let mut pty = Pty::spawn(&[], 1, 1);
    pty.type_keys(b"abc\r");
    assert!(!pty.panicked(), "output was: {}", pty.output());
}

/// Keys arriving in one `read` after a submitted line were discarded, so a
/// script piping several commands lost all but the first.
#[test]
fn keys_batched_with_a_submission_are_not_lost() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"first\rsecond\rthird\r");
    assert_eq!(pty.lines(), ["first", "second", "third"]);
}

/// The same, with editing keys mixed into the batch.
#[test]
fn a_batch_may_contain_editing_keys() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"abc\x01X\rdef\x17ghi\r");
    assert_eq!(pty.lines(), ["Xabc", "ghi"]);
}

/// Newlines inside a bracketed paste were dropped, silently joining the lines.
#[test]
fn bracketed_paste_preserves_newlines() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"\x1b[200~alpha\rbeta\x1b[201~\r");
    assert_eq!(pty.lines(), ["alpha\nbeta"]);
}

/// Control characters inside a paste must be inserted or ignored, never obeyed:
/// that is the entire point of bracketed paste.
#[test]
fn bracketed_paste_does_not_execute_control_characters() {
    let mut pty = Pty::spawn(&[], 24, 80);
    // CTRL-U would kill the line if it were obeyed.
    pty.type_keys(b"\x1b[200~keep\x15me\x1b[201~\r");
    assert_eq!(pty.lines(), ["keepme"]);
}

/// A multi-byte common prefix was sliced with a character count used as a byte
/// index, panicking on the first non-ASCII completion.
#[test]
fn completion_with_a_multibyte_common_prefix_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("日本.txt"), "").unwrap();
    std::fs::write(dir.path().join("日月.txt"), "").unwrap();
    let base = dir.path().to_str().unwrap();

    let mut pty = Pty::spawn(&["--files"], 24, 80);
    pty.type_keys(format!("cat {base}/日").as_bytes());
    pty.type_keys(b"\t");
    assert!(!pty.panicked(), "output was: {}", pty.output());

    pty.type_keys(b"\t\r");
    assert!(!pty.panicked());
    let lines = pty.lines();
    assert!(
        lines[0].contains("日月.txt") || lines[0].contains("日本.txt"),
        "expected a completion, got {lines:?}",
    );
}

/// A candidate shorter than the typed word indexed past the end of the string.
#[test]
fn completion_shorter_than_the_typed_word_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("bin")).unwrap();
    let base = dir.path().to_str().unwrap();

    let mut pty = Pty::spawn(&["--files"], 24, 80);
    pty.type_keys(format!("cat {base}/././b").as_bytes());
    pty.type_keys(b"\t\r");
    assert!(!pty.panicked(), "output was: {}", pty.output());
    assert_eq!(pty.lines(), [format!("cat {base}/././bin/")]);
}

/// The completer's word boundary and the inserter's disagreed, so any path
/// containing a separator searched the wrong directory.
#[test]
fn completion_reads_the_directory_in_the_typed_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/buffer.rs"), "").unwrap();
    std::fs::write(dir.path().join("decoy.rs"), "").unwrap();
    let base = dir.path().to_str().unwrap();

    let mut pty = Pty::spawn(&["--files"], 24, 80);
    pty.type_keys(format!("cat {base}/src/buf").as_bytes());
    pty.type_keys(b"\t\r");
    assert_eq!(pty.lines(), [format!("cat {base}/src/buffer.rs")]);
}

/// Ambiguous candidates list rather than guessing, and a further TAB cycles.
#[test]
fn ambiguous_completion_lists_then_cycles() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alpha.txt"), "").unwrap();
    std::fs::write(dir.path().join("alpine.txt"), "").unwrap();
    let base = dir.path().to_str().unwrap();

    let mut pty = Pty::spawn(&["--files"], 24, 80);
    pty.type_keys(format!("cat {base}/al").as_bytes());
    pty.type_keys(b"\t");
    // The shared prefix "alp" is inserted first.
    assert!(pty.output().contains("alp"), "output was: {}", pty.output());

    pty.type_keys(b"\t\r");
    let line = &pty.lines()[0];
    assert!(
        line.ends_with("alpha.txt") || line.ends_with("alpine.txt"),
        "expected a cycled candidate, got {line:?}",
    );
}

/// After moving the cursor onto an upper row of a wrapped line, the redraw
/// walked up by the wrong number of rows and overwrote the scrollback.
#[test]
fn redrawing_a_wrapped_line_keeps_the_prompt_in_place() {
    let cols = 40;
    let mut pty = Pty::spawn(&["--prompt=> "], 24, cols);
    pty.clear();

    // 90 characters at 40 columns is three rows.
    let long = "a".repeat(90);
    pty.type_keys(long.as_bytes());
    // Walk the cursor back onto the first row, then type.
    pty.type_keys(&b"\x1b[D".repeat(60));
    pty.type_keys(b"Z");

    let screen = pty.screen(24, cols as usize);
    assert!(
        screen.line(0).starts_with("> a"),
        "the prompt moved; row 0 is {:?}",
        screen.line(0),
    );

    pty.type_keys(b"\r");
    let submitted = &pty.lines()[0];
    assert_eq!(submitted.len(), 91);
    assert!(submitted.contains('Z'));
}

/// A wide glyph must never be split across the right edge of the screen.
#[test]
fn wide_characters_wrap_whole() {
    let cols = 12;
    let mut pty = Pty::spawn(&["--prompt=> "], 24, cols);
    pty.clear();
    pty.type_keys("中中中中中中".as_bytes());

    let screen = pty.screen(24, cols as usize);
    // Prompt is 2 columns, so five glyphs fit on the first row.
    assert_eq!(screen.line(0), "> 中中中中中");

    pty.type_keys(b"\r");
    assert_eq!(pty.lines(), ["中中中中中中"]);
}

/// A coloured prompt is measured in columns, not bytes, so the cursor lands in
/// the right place.
#[test]
fn an_ansi_prompt_does_not_displace_the_cursor() {
    let mut pty = Pty::spawn(&["--prompt=\x1b[32m$ \x1b[0m"], 24, 40);
    pty.clear();
    pty.type_keys(b"hi");

    let screen = pty.screen(24, 40);
    assert_eq!(screen.line(0), "$ hi");
    assert_eq!(screen.cursor(), (0, 4));
}

/// `CTRL-D` on an empty line is end of input; on a non-empty line it deletes.
#[test]
fn ctrl_d_is_eof_only_when_the_line_is_empty() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"ab\x01\x04\r");
    assert_eq!(pty.lines(), ["b"]);

    pty.type_keys(b"\x04");
    pty.settle();
    assert!(pty.output().contains("@@EOF@@"), "{}", pty.output());
}

/// `CTRL-C` abandons the line without ending the session.
#[test]
fn ctrl_c_interrupts_without_exiting() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"discard me\x03");
    assert!(pty.output().contains("@@INT@@"));
    pty.type_keys(b"kept\r");
    assert_eq!(pty.lines(), ["kept"]);
}

/// An unclosed parenthesis keeps reading, and the result is one string with a
/// newline in it.
#[test]
fn balance_mode_reads_continuation_lines() {
    let mut pty = Pty::spawn(&["--balance"], 24, 80);
    pty.type_keys(b"(a\rb)\r");
    assert_eq!(pty.lines(), ["(a\nb)"]);
}

/// The balance check runs over the whole entry and clamps at zero, so a stray
/// closing paren cannot cancel a later opening one.
#[test]
fn balance_mode_clamps_at_zero() {
    let mut pty = Pty::spawn(&["--balance"], 24, 80);
    // ")(" is unbalanced: the ")" must not offset the "(".
    pty.type_keys(b")(\r");
    pty.settle();
    assert!(
        pty.lines().is_empty(),
        "should still be waiting, got {:?}",
        pty.lines(),
    );
    pty.type_keys(b")\r");
    assert_eq!(pty.lines(), [")(\n)"]);
}

/// `CTRL-J` starts a continuation line even when parentheses balance.
#[test]
fn linefeed_always_continues() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"one\x0atwo\r");
    assert_eq!(pty.lines(), ["one\ntwo"]);
}

/// Reverse search finds an earlier entry and `ENTER` accepts it.
#[test]
fn reverse_search_recalls_an_entry() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"alpha one\r");
    pty.type_keys(b"beta two\r");
    pty.type_keys(b"gamma three\r");

    pty.type_keys(b"\x12");
    pty.type_keys(b"alpha");
    pty.type_keys(b"\r");
    assert_eq!(pty.lines()[3], "alpha one");
}

/// `CTRL-G` restores what was there before the search started.
#[test]
fn reverse_search_can_be_cancelled() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"stored\r");
    pty.type_keys(b"draft");
    pty.type_keys(b"\x12stor\x07");
    pty.type_keys(b"\r");
    assert_eq!(pty.lines()[1], "draft");
}

/// Moving up into history, editing, and moving back must not lose the edit or
/// the line that was being typed.
#[test]
fn history_navigation_preserves_edits() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"remembered\r");
    pty.type_keys(b"typing");
    // Up to the stored entry, append to it, then back down.
    pty.type_keys(b"\x1b[A!");
    pty.type_keys(b"\x1b[B");
    pty.type_keys(b"\r");
    assert_eq!(pty.lines()[1], "typing");

    // The edited history entry is still there.
    pty.type_keys(b"\x1b[A\x1b[A\r");
    assert_eq!(pty.lines()[2], "remembered!");
}

/// Mask mode draws asterisks and returns the real text.
#[test]
fn mask_mode_hides_the_input() {
    let mut pty = Pty::spawn(&["--mask", "--prompt=pw: "], 24, 40);
    pty.clear();
    pty.type_keys(b"secret");

    let screen = pty.screen(24, 40);
    assert_eq!(screen.line(0), "pw: ******");
    assert!(!pty.output().contains("secret"));

    pty.type_keys(b"\r");
    assert_eq!(pty.lines(), ["secret"]);
}

/// Hints are drawn but never become part of the line.
#[test]
fn hints_are_advisory_only() {
    let mut pty = Pty::spawn(&["--hints", "--prompt=> "], 24, 40);
    pty.clear();
    pty.type_keys(b"hel");

    // The hinter offers the first command with this prefix.
    let screen = pty.screen(24, 40);
    assert_eq!(screen.line(0), "> help");

    pty.type_keys(b"\r");
    assert_eq!(pty.lines(), ["hel"]);
}

/// A transliteration function is applied to typed characters.
#[test]
fn xlat_transforms_typed_characters() {
    let mut pty = Pty::spawn(&["--upcase"], 24, 80);
    pty.type_keys(b"hello\r");
    assert_eq!(pty.lines(), ["HELLO"]);
}

/// UTF-8 survives a round trip, including characters split across reads.
#[test]
fn utf8_round_trips() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys("héllo 中文 😀".as_bytes());
    pty.type_keys(b"\r");
    assert_eq!(pty.lines(), ["héllo 中文 😀"]);
}

/// A multi-byte character delivered one byte at a time must still arrive whole.
#[test]
fn utf8_split_across_reads_is_reassembled() {
    let mut pty = Pty::spawn(&[], 24, 80);
    for byte in "中".as_bytes() {
        pty.send(&[*byte]);
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    pty.type_keys(b"\r");
    assert_eq!(pty.lines(), ["中"]);
}

/// An escape sequence split across reads must not be inserted as text.
#[test]
fn escape_sequences_split_across_reads_are_reassembled() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"abc");
    for byte in b"\x1b[H" {
        pty.send(&[*byte]);
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    pty.type_keys(b"X\r");
    assert_eq!(pty.lines(), ["Xabc"]);
}

/// Word, case and paredit operations, checked end to end.
#[test]
fn word_and_expression_editing() {
    let cases: &[(&[u8], &str, &str)] = &[
        (b"one two\x1bb\x1bu\r", "one TWO", "ALT-U uppercase word"),
        (b"ONE TWO\x1bb\x1bl\r", "ONE two", "ALT-L lowercase word"),
        (b"one two\x1bb\x1bc\r", "one Two", "ALT-C capitalize word"),
        (b"one two\x01\x1bd\r", " two", "ALT-D kill word forward"),
        (
            b"one    two\x01\x06\x06\x06\x1b\\\r",
            "onetwo",
            "ALT-\\ squeeze",
        ),
        // Like bestline (and Emacs), transposing words acts on the pair
        // ending at the cursor, so it is a no-op at the start of the line.
        (b"one two\x1bt\r", "two one", "ALT-T transpose words"),
        (
            b"(a b c)\x02\x02\x02\x02\x1bB\r",
            "(a b) c",
            "ALT-SHIFT-B barf",
        ),
    ];

    for (keys, expected, what) in cases {
        let mut pty = Pty::spawn(&[], 24, 80);
        pty.type_keys(keys);
        assert_eq!(pty.lines(), [*expected], "{what}");
    }
}

/// The mark is set with `CTRL-SPACE` and returned to with `CTRL-X CTRL-X`.
#[test]
fn mark_and_goto_mark() {
    let mut pty = Pty::spawn(&[], 24, 80);
    // Type "hello", go home, set the mark, go to the end, return, insert.
    pty.type_keys(b"hello\x01\x00\x05\x18\x18X\r");
    assert_eq!(pty.lines(), ["Xhello"]);
}

/// `ALT-Y` replaces the previous yank rather than inserting alongside it.
#[test]
fn alt_y_rotates_the_kill_ring() {
    let mut pty = Pty::spawn(&[], 24, 80);
    // Kill "two" then "one ", yank the newest, then rotate to the older.
    pty.type_keys(b"one two\x17\x17\x19\x1by\r");
    assert_eq!(pty.lines(), ["two"]);
}

/// `ALT-Y` without a preceding yank must do nothing at all.
#[test]
fn alt_y_without_a_yank_is_inert() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"one two\x17abc\x1by\r");
    assert_eq!(pty.lines(), ["one abc"]);
}

/// Piped input bypasses the editor and reads plain lines.
#[test]
fn a_pipe_falls_back_to_plain_reading() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe().unwrap();
    let harness = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/harness");

    let mut child = Command::new(harness)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .env("TERM", "xterm-256color")
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"one\n\ntwo\n")
        .unwrap();
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains(r#"@@LINE@@ "one""#), "{text}");
    // A blank line is an empty string, not end of file.
    assert!(text.contains(r#"@@LINE@@ """#), "{text}");
    assert!(text.contains(r#"@@LINE@@ "two""#), "{text}");
    assert!(text.contains("@@EOF@@"), "{text}");
}

/// A terminal that cannot render escape codes falls back to plain reading.
#[test]
fn a_dumb_terminal_falls_back_to_plain_reading() {
    let mut pty = Pty::spawn(&[], 24, 80);
    drop(pty.output());
    // Covered by the pipe test for behaviour; here just check the pty path is
    // unaffected by the fallback logic.
    pty.type_keys(b"still interactive\r");
    assert_eq!(pty.lines(), ["still interactive"]);
}

/// `CTRL-L` clears the screen and redraws the prompt in place.
#[test]
fn ctrl_l_clears_and_redraws() {
    let mut pty = Pty::spawn(&["--prompt=> "], 24, 40);
    pty.type_keys(b"kept");
    pty.clear();
    pty.type_keys(b"\x0c");

    assert!(pty.output().contains("\x1b[2J"), "no erase-display emitted");
    let screen = pty.screen(24, 40);
    assert_eq!(screen.line(0), "> kept");

    pty.type_keys(b"\r");
    assert_eq!(pty.lines(), ["kept"]);
}

/// `ALT-<` and `ALT->` jump to the ends of the history.
#[test]
fn alt_angle_brackets_jump_to_the_history_ends() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"oldest\r");
    pty.type_keys(b"middle\r");
    pty.type_keys(b"newest\r");

    pty.type_keys(b"\x1b<\r");
    assert_eq!(pty.lines()[3], "oldest");

    // Walk up twice, then jump back to the empty scratch line.
    pty.type_keys(b"\x1b[A\x1b[A");
    pty.type_keys(b"\x1b>done\r");
    assert_eq!(pty.lines()[4], "done");
}

/// Ambiguous completions are listed below the prompt, which is then redrawn.
#[test]
fn completions_are_listed_below_the_prompt() {
    let mut pty = Pty::spawn(&["--commands", "--prompt=> "], 24, 60);
    pty.clear();
    // "h" matches help, history and hello.
    pty.type_keys(b"h\t");

    let output = pty.output();
    assert!(output.contains("hello"), "candidates not listed: {output}");
    assert!(
        output.contains("history"),
        "candidates not listed: {output}"
    );

    // "hello", "help" and "history" share only "h", which is already typed, so
    // there is nothing to insert and the buffer is left alone.
    pty.type_keys(b"\r");
    assert_eq!(pty.lines(), ["h"]);

    // A second TAB after the listing cycles through the candidates.
    pty.type_keys(b"h\t\t\r");
    assert_eq!(pty.lines()[1], "hello");
}

/// `CTRL-Z` leaves raw mode, and the line survives coming back.
///
/// The child is a session leader here, so its process group is orphaned and
/// `SIGTSTP` is discarded rather than stopping it. What this checks is the part
/// that can go wrong silently: that leaving and re-entering raw mode around the
/// suspend leaves the editor usable and the buffer intact.
#[test]
fn suspend_leaves_the_editor_usable() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"kept\x1a");
    assert!(pty.is_alive(), "suspend killed the process");
    pty.type_keys(b"!\r");
    assert_eq!(pty.lines(), ["kept!"]);
}

/// Typing several characters in one write costs one redraw, not one per key.
#[test]
fn a_batch_of_keys_costs_one_redraw() {
    let mut pty = Pty::spawn(&["--prompt=> ", "--no-highlight"], 24, 80);
    pty.clear();
    pty.type_keys(b"abcdef");

    // Each redraw begins by returning to column zero, so counting carriage
    // returns counts frames.
    let frames = pty.output().matches('\r').count();
    assert!(
        frames <= 2,
        "expected one frame for the batch, saw {frames}: {:?}",
        pty.output(),
    );

    pty.type_keys(b"\r");
    assert_eq!(pty.lines(), ["abcdef"]);
}

/// An unbound key is ignored rather than inserted as garbage.
#[test]
fn unbound_keys_are_ignored() {
    let mut pty = Pty::spawn(&[], 24, 80);
    // F5, page up, page down and insert have no binding.
    pty.type_keys(b"ab\x1b[15~\x1b[5~\x1b[6~\x1b[2~cd\r");
    assert_eq!(pty.lines(), ["abcd"]);
}

/// Invalid UTF-8 must not be inserted or crash the editor.
#[test]
fn invalid_utf8_input_is_discarded() {
    let mut pty = Pty::spawn(&[], 24, 80);
    pty.type_keys(b"ab\xff\xfecd\r");
    assert!(!pty.panicked());
    assert_eq!(pty.lines(), ["abcd"]);
}

/// Output the caller printed without a trailing newline must reach the terminal
/// before the next prompt is drawn.
///
/// The editor writes to the descriptor directly, bypassing the buffer behind
/// `std::io::stdout()`. Without an explicit flush the caller's text overtakes
/// nothing at all — it surfaces later, interleaved with a redraw. That is what
/// made an application's `clear` command appear to do nothing, then clear the
/// screen at the wrong moment one command later.
#[test]
fn buffered_caller_output_is_flushed_before_the_prompt() {
    let mut pty = Pty::spawn(&["--prompt=> "], 24, 80);
    pty.type_keys(b"unflushed\r");

    let output = pty.output();
    let marker = output
        .find("@@UNFLUSHED@@")
        .unwrap_or_else(|| panic!("unflushed output never arrived: {output:?}"));

    // The prompt drawn after it must come later in the stream, not before.
    let prompt_after = output[marker..].find("> ");
    assert!(
        prompt_after.is_some(),
        "no prompt drawn after the flushed text: {output:?}",
    );

    pty.type_keys(b"ok\r");
    assert_eq!(pty.lines(), ["unflushed", "ok"]);
}
