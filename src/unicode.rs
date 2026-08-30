//! Unicode helpers: terminal display width, word separators, and paren mirroring.
//!
//! The width functions mirror bestline's `GetMonospaceWidth`, which walks the
//! string as a small state machine so that ANSI escape sequences embedded in a
//! prompt or hint contribute zero columns.

use unicode_width::UnicodeWidthChar;

/// Returns `true` for C0/C1 control characters, which occupy no columns.
#[must_use]
pub fn is_control(c: char) -> bool {
    let n = c as u32;
    n < 0x20 || (0x7f..0xa0).contains(&n)
}

/// Number of terminal columns occupied by `c`.
///
/// Control characters and non-spacing marks report zero.
#[must_use]
pub fn char_width(c: char) -> usize {
    if is_control(c) {
        0
    } else {
        c.width().unwrap_or(0)
    }
}

/// Parser states for [`display_width_parts`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum WidthState {
    /// Normal text.
    Text,
    /// Saw `ESC`.
    Escape,
    /// Inside a CSI parameter run.
    CsiParam,
    /// Inside a CSI intermediate run.
    CsiIntermediate,
}

/// Monospace width of `s` in terminal columns, skipping ANSI escape sequences.
///
/// A prompt such as `"\x1b[32muser\x1b[0m$ "` measures 6 columns, not 15.
#[must_use]
pub fn display_width(s: &str) -> usize {
    display_width_parts(s).0
}

/// Like [`display_width`], but also reports whether `s` contains any character
/// wider than one column.
///
/// The renderer uses the flag to leave slack when deciding how much of an
/// over-long line it can show, since a double-width glyph cannot be split
/// across the right edge of the screen.
#[must_use]
pub fn display_width_parts(s: &str) -> (usize, bool) {
    let mut width = 0;
    let mut has_wide = false;
    let mut state = WidthState::Text;

    for c in s.chars() {
        match state {
            WidthState::Text => {
                if c == '\x1b' {
                    state = WidthState::Escape;
                } else {
                    let w = char_width(c);
                    has_wide |= w > 1;
                    width += w;
                }
            }
            WidthState::Escape => {
                state = if c == '[' {
                    WidthState::CsiParam
                } else {
                    // Two-character escape such as ESC 7; it ends here.
                    WidthState::Text
                };
            }
            WidthState::CsiParam => match c {
                '\u{30}'..='\u{3f}' => {}
                '\u{20}'..='\u{2f}' => state = WidthState::CsiIntermediate,
                '\u{40}'..='\u{7e}' => state = WidthState::Text,
                // Malformed sequence: treat the byte as text and resynchronize.
                _ => {
                    state = WidthState::Text;
                    width += char_width(c);
                }
            },
            WidthState::CsiIntermediate => match c {
                '\u{20}'..='\u{2f}' => {}
                '\u{40}'..='\u{7e}' => state = WidthState::Text,
                _ => {
                    state = WidthState::Text;
                    width += char_width(c);
                }
            },
        }
    }

    (width, has_wide)
}

/// Returns `true` if `c` separates words.
///
/// Matches bestline's `bestlineIsSeparator` exactly for ASCII (anything that is
/// not `[0-9A-Za-z]`) and approximates its codepoint table above ASCII with
/// [`char::is_alphanumeric`].
#[must_use]
pub fn is_separator(c: char) -> bool {
    !c.is_alphanumeric()
}

/// Returns `true` if `c` is a separator that is not a bracket.
///
/// Expression-level movement skips over these but stops at brackets, which is
/// what lets `ALT-LEFT` / `ALT-RIGHT` traverse s-expressions.
#[must_use]
pub fn is_xeparator(c: char) -> bool {
    is_separator(c) && mirror_left(c).is_none() && mirror_right(c).is_none()
}

/// Maps a closing bracket to its opening counterpart.
#[must_use]
pub fn mirror_left(c: char) -> Option<char> {
    match c {
        ')' => Some('('),
        ']' => Some('['),
        '}' => Some('{'),
        '>' => Some('<'),
        '\u{300b}' => Some('\u{300a}'),
        '\u{3009}' => Some('\u{3008}'),
        '\u{300d}' => Some('\u{300c}'),
        '\u{300f}' => Some('\u{300e}'),
        '\u{3011}' => Some('\u{3010}'),
        '\u{3015}' => Some('\u{3014}'),
        '\u{ff09}' => Some('\u{ff08}'),
        '\u{ff3d}' => Some('\u{ff3b}'),
        '\u{ff5d}' => Some('\u{ff5b}'),
        _ => None,
    }
}

/// Maps an opening bracket to its closing counterpart.
#[must_use]
pub fn mirror_right(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '<' => Some('>'),
        '\u{300a}' => Some('\u{300b}'),
        '\u{3008}' => Some('\u{3009}'),
        '\u{300c}' => Some('\u{300d}'),
        '\u{300e}' => Some('\u{300f}'),
        '\u{3010}' => Some('\u{3011}'),
        '\u{3014}' => Some('\u{3015}'),
        '\u{ff08}' => Some('\u{ff09}'),
        '\u{ff3b}' => Some('\u{ff3d}'),
        '\u{ff5b}' => Some('\u{ff5d}'),
        _ => None,
    }
}

/// Lowercases `c`, collapsing multi-character expansions to the first char.
#[must_use]
pub fn to_lowercase(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Uppercases `c`, collapsing multi-character expansions to the first char.
#[must_use]
pub fn to_uppercase(c: char) -> char {
    c.to_uppercase().next().unwrap_or(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_width() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn ansi_sequences_are_zero_width() {
        assert_eq!(display_width("\x1b[32muser\x1b[0m$ "), 6);
        assert_eq!(display_width("\x1b[1;31m"), 0);
        // Private-mode and intermediate bytes.
        assert_eq!(display_width("\x1b[?2004hx"), 1);
        assert_eq!(display_width("\x1b[ qX"), 1);
    }

    #[test]
    fn wide_and_combining() {
        let (w, wide) = display_width_parts("中文");
        assert_eq!(w, 4);
        assert!(wide);

        let (w, wide) = display_width_parts("abc");
        assert_eq!(w, 3);
        assert!(!wide);

        assert_eq!(char_width('\u{1f600}'), 2);
        assert_eq!(char_width('\t'), 0);
        assert_eq!(char_width('\x7f'), 0);
    }

    #[test]
    fn separators_match_bestline_for_ascii() {
        for c in '\0'..='\x7f' {
            let expected = !c.is_ascii_alphanumeric();
            assert_eq!(is_separator(c), expected, "mismatch for {c:?}");
        }
        assert!(!is_separator('é'));
        assert!(!is_separator('中'));
    }

    #[test]
    fn xeparator_excludes_brackets() {
        assert!(is_xeparator(' '));
        assert!(is_xeparator(','));
        assert!(!is_xeparator('('));
        assert!(!is_xeparator(')'));
        assert!(!is_xeparator('a'));
    }

    #[test]
    fn mirrors_round_trip() {
        for c in "([{<".chars() {
            let right = mirror_right(c).unwrap();
            assert_eq!(mirror_left(right), Some(c));
        }
    }
}
