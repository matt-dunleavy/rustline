#[derive(Debug, Clone, PartialEq)]
pub enum KeySeq {
    Char(char),
    Ctrl(char),
    Alt(char),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Tab,
    Enter,
    Backspace,
    F(u8),
    BracketedPasteStart,
    BracketedPasteEnd,
    Unknown(Vec<u8>),
}

pub struct Parser {
    state: ParseState,
    buffer: Vec<u8>,
    utf8_buffer: Vec<u8>,
    utf8_expected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ParseState {
    Ground,
    Escape,
    CsiParam,
    CsiIntermediate,
    Ss3,
    Utf8,
}

impl Parser {
    pub fn new() -> Self {
        Parser {
            state: ParseState::Ground,
            buffer: Vec::new(),
            utf8_buffer: Vec::with_capacity(4),
            utf8_expected: 0,
        }
    }

    pub fn parse(&mut self, input: &[u8]) -> Vec<KeySeq> {
        let mut keys = Vec::new();

        for &byte in input {
            if self.state == ParseState::Utf8 {
                self.process_utf8_byte(byte, &mut keys);
                continue;
            }

            self.buffer.push(byte);

            match self.state {
                ParseState::Ground => {
                    self.process_ground_byte(byte, &mut keys);
                }

                ParseState::Escape => {
                    self.process_escape_byte(byte, &mut keys);
                }

                ParseState::CsiParam => {
                    self.process_csi_param_byte(byte, &mut keys);
                }

                ParseState::CsiIntermediate => {
                    self.process_csi_intermediate_byte(byte, &mut keys);
                }

                ParseState::Ss3 => {
                    self.process_ss3_byte(byte, &mut keys);
                }

                ParseState::Utf8 => {
                    unreachable!("UTF-8 state should be handled at the beginning of the loop");
                }
            }
        }

        keys
    }

    fn process_escape_byte(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        match byte {
            b'[' => self.state = ParseState::CsiParam,
            b'O' => self.state = ParseState::Ss3,
            byte if byte >= 0x80 => {
                self.start_utf8_sequence(byte);
            }
            b'0'..=b'~' => {
                self.handle_alt_ascii(byte, keys);
                self.reset();
            }
            _ => self.reset(),
        }
    }

    fn handle_alt_ascii(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        if self.buffer.len() == 2 {
            keys.push(KeySeq::Alt(byte as char));
        }
    }

    fn process_csi_param_byte(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        match byte {
            b'0'..=b'9' | b';' => {}
            b' '..=b'/' => self.state = ParseState::CsiIntermediate,
            b'@'..=b'~' => {
                self.handle_csi_final_byte(byte, keys);
                self.reset();
            }
            _ => self.reset(),
        }
    }

    fn handle_csi_final_byte(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        if let Some(key) = self.parse_csi_sequence(byte) {
            keys.push(key);
        }
    }

    fn process_csi_intermediate_byte(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        match byte {
            b' '..=b'/' => {}
            b'@'..=b'~' => {
                self.handle_csi_final_byte(byte, keys);
                self.reset();
            }
            _ => self.reset(),
        }
    }

    fn process_ss3_byte(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        match byte {
            b'A' => keys.push(KeySeq::Up),
            b'B' => keys.push(KeySeq::Down),
            b'C' => keys.push(KeySeq::Right),
            b'D' => keys.push(KeySeq::Left),
            b'H' => keys.push(KeySeq::Home),
            b'F' => keys.push(KeySeq::End),
            _ => {}
        }
        self.reset();
    }

    fn process_utf8_byte(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        self.utf8_buffer.push(byte);

        if byte & 0xC0 != 0x80 {
            keys.push(KeySeq::Unknown(self.utf8_buffer.clone()));
            self.reset_utf8();
            self.state = ParseState::Ground;
            self.process_ground_byte(byte, keys);
            return;
        }

        if self.utf8_buffer.len() == self.utf8_expected {
            self.decode_utf8_sequence(keys);
        }
    }

    fn decode_utf8_sequence(&mut self, keys: &mut Vec<KeySeq>) {
        let utf8_buffer = &self.utf8_buffer;
        match std::str::from_utf8(utf8_buffer) {
            Ok(s) => {
                if let Some(ch) = s.chars().next() {
                    if self.buffer.len() == 1 && self.buffer[0] == 0x1b {
                        keys.push(KeySeq::Alt(ch));
                        self.buffer.clear();
                    } else {
                        keys.push(KeySeq::Char(ch));
                    }
                }
            }
            Err(_) => {
                keys.push(KeySeq::Unknown(self.utf8_buffer.clone()));
            }
        }
        self.reset_utf8();
        self.state = ParseState::Ground;
    }

    fn process_ground_byte(&mut self, byte: u8, keys: &mut Vec<KeySeq>) {
        if byte == 0x1b {
            self.state = ParseState::Escape;
        } else if byte < 0x20 {
            match byte {
                0x09 => keys.push(KeySeq::Tab),
                0x0A | 0x0D => keys.push(KeySeq::Enter),
                _ => keys.push(KeySeq::Ctrl((byte + b'@') as char)),
            }
            self.buffer.clear();
        } else if byte == 0x7f {
            keys.push(KeySeq::Backspace);
            self.buffer.clear();
        } else if byte < 0x80 {
            keys.push(KeySeq::Char(byte as char));
            self.buffer.clear();
        } else {
            self.buffer.pop();
            self.start_utf8_sequence(byte);
        }
    }

    fn start_utf8_sequence(&mut self, first_byte: u8) {
        self.utf8_buffer.clear();
        self.utf8_buffer.push(first_byte);

        self.utf8_expected = if first_byte & 0xE0 == 0xC0 {
            2
        } else if first_byte & 0xF0 == 0xE0 {
            3
        } else if first_byte & 0xF8 == 0xF0 {
            4
        } else {
            self.state = ParseState::Ground;
            return;
        };

        self.state = ParseState::Utf8;
    }

    fn parse_csi_sequence(&self, final_byte: u8) -> Option<KeySeq> {
        match final_byte {
            b'A' => Some(KeySeq::Up),
            b'B' => Some(KeySeq::Down),
            b'C' => Some(KeySeq::Right),
            b'D' => Some(KeySeq::Left),
            b'H' => Some(KeySeq::Home),
            b'F' => Some(KeySeq::End),
            b'~' => self.parse_csi_tilde_sequence(),
            _ => None,
        }
    }

    fn parse_csi_tilde_sequence(&self) -> Option<KeySeq> {
        if self.buffer.len() < 4 {
            return None;
        }

        let param_bytes = &self.buffer[2..self.buffer.len() - 1];
        let param_str = std::str::from_utf8(param_bytes).ok()?;
        let param: u32 = param_str.parse().ok()?;

        match param {
            1 => Some(KeySeq::Home),
            2 => Some(KeySeq::Insert),
            3 => Some(KeySeq::Delete),
            4 => Some(KeySeq::End),
            5 => Some(KeySeq::PageUp),
            6 => Some(KeySeq::PageDown),
            11..=15 => Some(KeySeq::F((param - 10) as u8)),
            17..=21 => Some(KeySeq::F((param - 11) as u8)),
            23..=24 => Some(KeySeq::F((param - 12) as u8)),
            200 => Some(KeySeq::BracketedPasteStart),
            201 => Some(KeySeq::BracketedPasteEnd),
            _ => None,
        }
    }

    fn reset(&mut self) {
        self.state = ParseState::Ground;
        self.buffer.clear();
        self.reset_utf8();
    }

    fn reset_utf8(&mut self) {
        self.utf8_buffer.clear();
        self.utf8_expected = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_utf8_parsing() {
        let mut parser = Parser::new();

        let input = vec![0xC3, 0xA9];
        let keys = parser.parse(&input);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], KeySeq::Char('é'));

        let input = vec![0xE4, 0xB8, 0xAD];
        let keys = parser.parse(&input);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], KeySeq::Char('中'));

        let input = vec![0xF0, 0x9D, 0x84, 0x9E];
        let keys = parser.parse(&input);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], KeySeq::Char('𝄞'));
    }

    #[test]
    fn test_alt_utf8() {
        let mut parser = Parser::new();

        let input = vec![0x1B, 0xC3, 0xA9];
        let keys = parser.parse(&input);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], KeySeq::Alt('é'));
    }

    #[test]
    fn test_invalid_utf8() {
        let mut parser = Parser::new();

        let input = vec![0xFF, 0xFE];
        let keys = parser.parse(&input);
        assert!(keys.iter().any(|k| matches!(k, KeySeq::Unknown(_))));
    }

    #[test]
    fn test_mixed_input() {
        let mut parser = Parser::new();

        let input = b"a\xC3\xA9\x1B[A".to_vec();
        let keys = parser.parse(&input);
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0], KeySeq::Char('a'));
        assert_eq!(keys[1], KeySeq::Char('é'));
        assert_eq!(keys[2], KeySeq::Up);
    }
}
