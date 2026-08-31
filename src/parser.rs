//! Incremental ANSI / UTF-8 keyboard input parser.
//!
//! Terminals deliver keystrokes as byte sequences that may be split across
//! `read` boundaries, so the parser is a resumable state machine: feed it
//! whatever bytes arrive and it returns the complete keys recognized so far,
//! retaining any partial sequence for the next call.

/// A decoded keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySeq {
    /// A printable character.
    Char(char),
    /// A control chord. The payload is normalized to lowercase, so `0x01`
    /// decodes as `Ctrl('a')`. `Ctrl('@')` is `NUL`, produced by `CTRL-SPACE`.
    Ctrl(char),
    /// `ESC` followed by a printable character (meta / alt).
    Alt(char),
    /// `ESC` followed by a control chord, normalized like [`KeySeq::Ctrl`].
    CtrlAlt(char),
    /// Cursor up.
    Up,
    /// Cursor down.
    Down,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
    /// `ALT` + cursor left, from either `ESC ESC [ D` or `CSI 1;3 D`.
    AltLeft,
    /// `ALT` + cursor right, from either `ESC ESC [ C` or `CSI 1;3 C`.
    AltRight,
    /// Home.
    Home,
    /// End.
    End,
    /// Insert.
    Insert,
    /// Forward delete.
    Delete,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Tab (`0x09`).
    Tab,
    /// Carriage return (`0x0d`), which submits the line.
    Enter,
    /// Line feed (`0x0a`), which always starts a continuation line.
    LineFeed,
    /// Backspace (`0x7f`).
    Backspace,
    /// `ESC` followed by backspace, conventionally "kill word backwards".
    AltBackspace,
    /// Function key, 1-based.
    F(u8),
    /// Start of a bracketed paste (`CSI 200 ~`).
    BracketedPasteStart,
    /// End of a bracketed paste (`CSI 201 ~`).
    BracketedPasteEnd,
    /// Bytes that did not form a recognized sequence.
    Unknown(Vec<u8>),
}

/// Longest run of `CSI` parameter and intermediate bytes kept.
///
/// No real terminal comes close: the longest sequence this crate decodes is
/// five bytes of parameters. The cap matters because `readline_raw` is offered
/// for serial lines and pseudoterminals, where the peer is not necessarily a
/// keyboard and an endless parameter run would otherwise grow the buffer until
/// the process died.
const MAX_SEQ: usize = 64;

/// Converts a C0 byte to its normalized control character.
///
/// `0x01`..=`0x1a` map to `a`..=`z`; the remaining C0 bytes map to the ASCII
/// character `0x40` above them, matching the usual `^X` notation.
fn ctrl_char(byte: u8) -> char {
    if (0x01..=0x1a).contains(&byte) {
        (b'a' + byte - 1) as char
    } else {
        (byte + 0x40) as char
    }
}

/// Number of bytes in a UTF-8 sequence beginning with `b`, or `None` if `b` is
/// not a valid lead byte.
fn utf8_len(b: u8) -> Option<usize> {
    match b {
        0x00..=0x7f => Some(1),
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        // 0xc0/0xc1 would encode an overlong sequence; 0x80..=0xbf is a
        // stray continuation byte; 0xf5.. is beyond the Unicode range.
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Outside any escape sequence.
    Ground,
    /// Saw `ESC`.
    Escape,
    /// Inside `CSI` parameter or intermediate bytes.
    Csi,
    /// Inside an `SS3` sequence (`ESC O`).
    Ss3,
    /// Accumulating a multi-byte UTF-8 character.
    Utf8,
}

/// Resumable parser turning raw terminal bytes into [`KeySeq`] values.
pub struct Parser {
    state: State,
    /// `CSI`/`SS3` bytes collected after the introducer.
    seq: Vec<u8>,
    /// Partial UTF-8 character.
    utf8: Vec<u8>,
    /// Total byte length of the UTF-8 character being accumulated.
    utf8_len: usize,
    /// Number of `ESC` bytes that introduced the current sequence. Two means
    /// the sequence carries an implicit `ALT` modifier (`ESC ESC [ C`).
    esc_count: u8,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    /// Creates a parser in the ground state.
    #[must_use]
    pub fn new() -> Self {
        Parser {
            state: State::Ground,
            seq: Vec::with_capacity(16),
            utf8: Vec::with_capacity(4),
            utf8_len: 0,
            esc_count: 0,
        }
    }

    /// Feeds `input` to the parser, returning every key completed by it.
    ///
    /// A trailing partial sequence is retained; pass the following bytes to a
    /// later call to complete it.
    pub fn parse(&mut self, input: &[u8]) -> Vec<KeySeq> {
        let mut keys = Vec::new();
        for &byte in input {
            self.step(byte, &mut keys);
        }
        keys
    }

    /// Returns `true` if a sequence is partially consumed.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.state == State::Ground
    }

    fn step(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        match self.state {
            State::Ground => self.ground(byte, keys),
            State::Escape => self.escape(byte, keys),
            State::Csi => self.csi(byte, keys),
            State::Ss3 => self.ss3(byte, keys),
            State::Utf8 => self.utf8(byte, keys),
        }
    }

    fn reset(&mut self) {
        self.state = State::Ground;
        self.seq.clear();
        self.utf8.clear();
        self.utf8_len = 0;
        self.esc_count = 0;
    }

    fn ground(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        match byte {
            0x1b => {
                self.state = State::Escape;
                self.esc_count = 1;
            }
            0x09 => keys.push(KeySeq::Tab),
            0x0a => keys.push(KeySeq::LineFeed),
            0x0d => keys.push(KeySeq::Enter),
            0x7f => keys.push(KeySeq::Backspace),
            0x00..=0x1f => keys.push(KeySeq::Ctrl(ctrl_char(byte))),
            0x20..=0x7e => keys.push(KeySeq::Char(byte as char)),
            _ => self.begin_utf8(byte, keys),
        }
    }

    fn escape(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        match byte {
            // A second ESC modifies the sequence that follows with ALT.
            0x1b if self.esc_count == 1 => self.esc_count = 2,
            b'[' => {
                self.state = State::Csi;
                self.seq.clear();
            }
            b'O' => {
                self.state = State::Ss3;
                self.seq.clear();
            }
            0x7f => {
                keys.push(KeySeq::AltBackspace);
                self.reset();
            }
            0x00..=0x1f => {
                keys.push(KeySeq::CtrlAlt(ctrl_char(byte)));
                self.reset();
            }
            0x20..=0x7e => {
                keys.push(KeySeq::Alt(byte as char));
                self.reset();
            }
            _ => {
                // ESC followed by a multi-byte character is ALT + that char.
                self.begin_utf8(byte, keys);
            }
        }
    }

    fn csi(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        match byte {
            // Parameter and intermediate bytes accumulate, up to `MAX_SEQ`.
            // Bytes past the cap are dropped rather than ending the sequence
            // early, so the parser still resynchronizes on the final byte
            // instead of treating the rest of the run as typed characters.
            0x20..=0x3f => {
                if self.seq.len() < MAX_SEQ {
                    self.seq.push(byte);
                }
            }
            // A final byte terminates the sequence.
            0x40..=0x7e => {
                // A truncated parameter list must not be decoded: the bytes
                // that were dropped could have changed what it means.
                let key = if self.seq.len() < MAX_SEQ {
                    self.decode_csi(byte)
                } else {
                    let mut bytes = vec![0x1b, b'['];
                    bytes.append(&mut self.seq);
                    bytes.push(byte);
                    Some(KeySeq::Unknown(bytes))
                };
                if let Some(key) = key {
                    keys.push(key);
                }
                self.reset();
            }
            _ => {
                let mut bytes = vec![0x1b, b'['];
                bytes.append(&mut self.seq);
                bytes.push(byte);
                keys.push(KeySeq::Unknown(bytes));
                self.reset();
            }
        }
    }

    fn ss3(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        let key = match byte {
            b'A' => Some(KeySeq::Up),
            b'B' => Some(KeySeq::Down),
            b'C' => Some(self.arrow_right()),
            b'D' => Some(self.arrow_left()),
            b'H' => Some(KeySeq::Home),
            b'F' => Some(KeySeq::End),
            b'P' => Some(KeySeq::F(1)),
            b'Q' => Some(KeySeq::F(2)),
            b'R' => Some(KeySeq::F(3)),
            b'S' => Some(KeySeq::F(4)),
            _ => None,
        };
        if let Some(key) = key {
            keys.push(key);
        }
        self.reset();
    }

    fn arrow_left(&self) -> KeySeq {
        if self.esc_count == 2 || self.modifier() == Some(3) {
            KeySeq::AltLeft
        } else {
            KeySeq::Left
        }
    }

    fn arrow_right(&self) -> KeySeq {
        if self.esc_count == 2 || self.modifier() == Some(3) {
            KeySeq::AltRight
        } else {
            KeySeq::Right
        }
    }

    /// Returns the `xterm` modifier parameter, i.e. the second `;`-separated
    /// field of the CSI parameter list. `3` means ALT.
    fn modifier(&self) -> Option<u32> {
        let params = std::str::from_utf8(&self.seq).ok()?;
        params.split(';').nth(1)?.parse().ok()
    }

    /// Returns the first `;`-separated CSI parameter.
    fn first_param(&self) -> Option<u32> {
        let params = std::str::from_utf8(&self.seq).ok()?;
        params.split(';').next()?.parse().ok()
    }

    fn decode_csi(&self, final_byte: u8) -> Option<KeySeq> {
        match final_byte {
            b'A' => Some(KeySeq::Up),
            b'B' => Some(KeySeq::Down),
            b'C' => Some(self.arrow_right()),
            b'D' => Some(self.arrow_left()),
            b'H' => Some(KeySeq::Home),
            b'F' => Some(KeySeq::End),
            b'~' => self.decode_tilde(),
            _ => None,
        }
    }

    fn decode_tilde(&self) -> Option<KeySeq> {
        match self.first_param()? {
            1 | 7 => Some(KeySeq::Home),
            2 => Some(KeySeq::Insert),
            3 => Some(KeySeq::Delete),
            4 | 8 => Some(KeySeq::End),
            5 => Some(KeySeq::PageUp),
            6 => Some(KeySeq::PageDown),
            n @ 11..=15 => Some(KeySeq::F((n - 10) as u8)),
            n @ 17..=21 => Some(KeySeq::F((n - 11) as u8)),
            n @ 23..=26 => Some(KeySeq::F((n - 12) as u8)),
            200 => Some(KeySeq::BracketedPasteStart),
            201 => Some(KeySeq::BracketedPasteEnd),
            _ => None,
        }
    }

    /// Starts accumulating a UTF-8 character, emitting [`KeySeq::Unknown`] if
    /// `byte` cannot begin a valid sequence.
    fn begin_utf8(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        let Some(len) = utf8_len(byte) else {
            // Invalid lead byte. Report it rather than dropping it silently, so
            // that a mis-decoded stream is visible instead of vanishing.
            keys.push(KeySeq::Unknown(vec![byte]));
            let alt = self.esc_count;
            self.reset();
            // Preserve a pending ALT so that `ESC <bad byte> a` still yields
            // ALT-a rather than a bare `a`.
            self.esc_count = alt;
            if alt > 0 {
                self.state = State::Escape;
            }
            return;
        };

        self.utf8.clear();
        self.utf8.push(byte);
        self.utf8_len = len;
        self.state = State::Utf8;
    }

    fn utf8(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        if byte & 0xc0 != 0x80 {
            // Truncated sequence. Report what we have, then reprocess this byte
            // from the ground state so no input is lost.
            keys.push(KeySeq::Unknown(std::mem::take(&mut self.utf8)));
            self.reset();
            self.ground(byte, keys);
            return;
        }

        self.utf8.push(byte);
        if self.utf8.len() < self.utf8_len {
            return;
        }

        let alt = self.esc_count > 0;
        match std::str::from_utf8(&self.utf8) {
            Ok(s) => {
                if let Some(c) = s.chars().next() {
                    keys.push(if alt { KeySeq::Alt(c) } else { KeySeq::Char(c) });
                }
            }
            Err(_) => keys.push(KeySeq::Unknown(self.utf8.clone())),
        }
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> Vec<KeySeq> {
        Parser::new().parse(bytes)
    }

    #[test]
    fn control_chords_are_lowercase() {
        // This is the regression test for the bug that disabled every control
        // binding: 0x01 must decode as Ctrl('a'), not Ctrl('A').
        assert_eq!(parse(&[0x01]), vec![KeySeq::Ctrl('a')]);
        assert_eq!(parse(&[0x05]), vec![KeySeq::Ctrl('e')]);
        assert_eq!(parse(&[0x1a]), vec![KeySeq::Ctrl('z')]);
        assert_eq!(parse(&[0x00]), vec![KeySeq::Ctrl('@')]);
        assert_eq!(parse(&[0x1c]), vec![KeySeq::Ctrl('\\')]);
    }

    #[test]
    fn every_control_byte_round_trips() {
        for byte in 0x01u8..=0x1a {
            let expected = (b'a' + byte - 1) as char;
            match byte {
                0x09 | 0x0a | 0x0d => continue,
                _ => assert_eq!(parse(&[byte]), vec![KeySeq::Ctrl(expected)]),
            }
        }
    }

    #[test]
    fn enter_and_linefeed_are_distinct() {
        assert_eq!(parse(&[0x0d]), vec![KeySeq::Enter]);
        assert_eq!(parse(&[0x0a]), vec![KeySeq::LineFeed]);
        assert_eq!(parse(&[0x09]), vec![KeySeq::Tab]);
        assert_eq!(parse(&[0x7f]), vec![KeySeq::Backspace]);
    }

    #[test]
    fn utf8_parsing() {
        assert_eq!(parse(&[0xc3, 0xa9]), vec![KeySeq::Char('é')]);
        assert_eq!(parse(&[0xe4, 0xb8, 0xad]), vec![KeySeq::Char('中')]);
        assert_eq!(parse(&[0xf0, 0x9d, 0x84, 0x9e]), vec![KeySeq::Char('𝄞')]);
    }

    #[test]
    fn alt_utf8() {
        assert_eq!(parse(&[0x1b, 0xc3, 0xa9]), vec![KeySeq::Alt('é')]);
        assert_eq!(parse(&[0x1b, 0xe4, 0xb8, 0xad]), vec![KeySeq::Alt('中')]);
    }

    #[test]
    fn invalid_utf8_is_reported() {
        let keys = parse(&[0xff, 0xfe]);
        assert_eq!(
            keys,
            vec![KeySeq::Unknown(vec![0xff]), KeySeq::Unknown(vec![0xfe])]
        );
    }

    #[test]
    fn truncated_utf8_does_not_swallow_the_next_key() {
        // Lead byte for a 3-byte sequence followed by a plain ASCII 'a'.
        let keys = parse(&[0xe4, b'a']);
        assert_eq!(keys, vec![KeySeq::Unknown(vec![0xe4]), KeySeq::Char('a')]);
    }

    #[test]
    fn mixed_input() {
        let keys = parse(b"a\xc3\xa9\x1b[A");
        assert_eq!(keys, vec![KeySeq::Char('a'), KeySeq::Char('é'), KeySeq::Up]);
    }

    #[test]
    fn arrows_and_navigation() {
        assert_eq!(parse(b"\x1b[A"), vec![KeySeq::Up]);
        assert_eq!(parse(b"\x1b[B"), vec![KeySeq::Down]);
        assert_eq!(parse(b"\x1b[C"), vec![KeySeq::Right]);
        assert_eq!(parse(b"\x1b[D"), vec![KeySeq::Left]);
        assert_eq!(parse(b"\x1b[H"), vec![KeySeq::Home]);
        assert_eq!(parse(b"\x1b[F"), vec![KeySeq::End]);
        assert_eq!(parse(b"\x1b[1~"), vec![KeySeq::Home]);
        assert_eq!(parse(b"\x1b[3~"), vec![KeySeq::Delete]);
        assert_eq!(parse(b"\x1b[4~"), vec![KeySeq::End]);
        assert_eq!(parse(b"\x1bOA"), vec![KeySeq::Up]);
        assert_eq!(parse(b"\x1bOH"), vec![KeySeq::Home]);
    }

    #[test]
    fn alt_arrows_both_encodings() {
        assert_eq!(parse(b"\x1b\x1b[C"), vec![KeySeq::AltRight]);
        assert_eq!(parse(b"\x1b\x1b[D"), vec![KeySeq::AltLeft]);
        assert_eq!(parse(b"\x1b\x1bOC"), vec![KeySeq::AltRight]);
        assert_eq!(parse(b"\x1b[1;3C"), vec![KeySeq::AltRight]);
        assert_eq!(parse(b"\x1b[1;3D"), vec![KeySeq::AltLeft]);
        // A plain arrow must not be misread as ALT.
        assert_eq!(parse(b"\x1b[1;2C"), vec![KeySeq::Right]);
    }

    #[test]
    fn meta_chords() {
        assert_eq!(parse(b"\x1bb"), vec![KeySeq::Alt('b')]);
        assert_eq!(parse(b"\x1b<"), vec![KeySeq::Alt('<')]);
        assert_eq!(parse(b"\x1b\\"), vec![KeySeq::Alt('\\')]);
        assert_eq!(parse(&[0x1b, 0x02]), vec![KeySeq::CtrlAlt('b')]);
        assert_eq!(parse(&[0x1b, 0x7f]), vec![KeySeq::AltBackspace]);
    }

    #[test]
    fn bracketed_paste_markers() {
        assert_eq!(parse(b"\x1b[200~"), vec![KeySeq::BracketedPasteStart]);
        assert_eq!(parse(b"\x1b[201~"), vec![KeySeq::BracketedPasteEnd]);
    }

    #[test]
    fn function_keys() {
        assert_eq!(parse(b"\x1b[11~"), vec![KeySeq::F(1)]);
        assert_eq!(parse(b"\x1b[24~"), vec![KeySeq::F(12)]);
        assert_eq!(parse(b"\x1bOP"), vec![KeySeq::F(1)]);
    }

    #[test]
    fn sequences_split_across_reads() {
        // Every escape sequence must survive being delivered one byte at a
        // time, because that is exactly what a slow serial line does.
        let inputs: &[&[u8]] = &[
            b"\x1b[A",
            b"\x1b[1;3C",
            b"\x1b[200~",
            b"\x1b\x1b[D",
            b"\x1bOP",
            &[0xf0, 0x9d, 0x84, 0x9e],
            &[0x1b, 0xc3, 0xa9],
        ];
        for input in inputs {
            let whole = parse(input);
            let mut parser = Parser::new();
            let mut drip = Vec::new();
            for &b in *input {
                drip.extend(parser.parse(&[b]));
            }
            assert_eq!(whole, drip, "byte-at-a-time mismatch for {input:?}");
            assert!(parser.is_idle());
        }
    }

    proptest::proptest! {
        /// Arbitrary bytes must never panic and never leave the parser stuck.
        #[test]
        fn arbitrary_bytes_never_panic(input in proptest::collection::vec(0u8..=255, 0..200)) {
            let mut parser = Parser::new();
            let _ = parser.parse(&input);
            // Feeding enough plain characters must always drain any partial
            // sequence, so the parser can never wedge.
            let _ = parser.parse(b"aaaaaaaa");
            proptest::prop_assert!(parser.is_idle());
        }

        /// Chunking the same bytes differently must produce the same keys.
        #[test]
        fn chunking_does_not_change_the_result(
            input in proptest::collection::vec(0u8..=255, 0..120),
            chunk in 1usize..8,
        ) {
            let whole = Parser::new().parse(&input);

            let mut parser = Parser::new();
            let mut chunked = Vec::new();
            for piece in input.chunks(chunk) {
                chunked.extend(parser.parse(piece));
            }
            proptest::prop_assert_eq!(whole, chunked);
        }

        /// Printable ASCII always decodes back to itself.
        #[test]
        fn printable_ascii_round_trips(text in "[ -~]{0,40}") {
            let keys = Parser::new().parse(text.as_bytes());
            let decoded: String = keys
                .iter()
                .map(|k| match k {
                    KeySeq::Char(c) => *c,
                    KeySeq::Tab => '\t',
                    _ => '\0',
                })
                .collect();
            proptest::prop_assert_eq!(decoded, text);
        }
    }

    /// A peer that never sends a final byte must not be able to grow the
    /// parser's buffer without bound. `readline_raw` is offered for serial
    /// lines, where the other end is not necessarily friendly.
    #[test]
    fn a_runaway_csi_parameter_run_is_bounded() {
        let mut parser = Parser::new();
        let mut input = vec![0x1b, b'['];
        input.extend(std::iter::repeat_n(b'1', 100_000));

        assert!(
            parser.parse(&input).is_empty(),
            "no key can be complete yet"
        );
        assert!(!parser.is_idle(), "the sequence is still open");
        assert!(parser.seq.len() <= MAX_SEQ, "the buffer grew without bound");

        // The sequence still ends where the stream says it does, and reports
        // itself as unrecognized rather than being decoded from a parameter
        // list most of which was thrown away.
        let keys = parser.parse(b"A");
        assert!(
            matches!(keys.as_slice(), [KeySeq::Unknown(_)]),
            "expected one unknown sequence, got {keys:?}",
        );
        assert!(parser.is_idle());

        // And the parser is back in step with the stream.
        assert_eq!(parser.parse(b"x"), vec![KeySeq::Char('x')]);
    }

    /// The cap must be nowhere near any sequence a terminal really sends.
    #[test]
    fn ordinary_sequences_are_well_inside_the_cap() {
        let mut parser = Parser::new();
        assert_eq!(parser.parse(b"\x1b[1;3C"), vec![KeySeq::AltRight]);
        assert_eq!(
            parser.parse(b"\x1b[200~"),
            vec![KeySeq::BracketedPasteStart]
        );
    }

    #[test]
    fn batched_keys_all_returned() {
        let keys = parse(b"ab\rcd\r");
        assert_eq!(
            keys,
            vec![
                KeySeq::Char('a'),
                KeySeq::Char('b'),
                KeySeq::Enter,
                KeySeq::Char('c'),
                KeySeq::Char('d'),
                KeySeq::Enter,
            ]
        );
    }
}
