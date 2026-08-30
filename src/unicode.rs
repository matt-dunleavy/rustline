use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, Copy)]
#[allow(unused)]
pub struct Rune {
    pub codepoint: u32,
    pub bytes: [u8; 4],
    pub len: usize,
}

#[allow(unused)]
impl Rune {
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let s = std::str::from_utf8(bytes).ok()?;
        let ch = s.chars().next()?;
        let codepoint = ch as u32;
        let mut buf = [0u8; 4];
        let len = ch.encode_utf8(&mut buf).len();
        Some(Rune {
            codepoint,
            bytes: buf,
            len,
        })
    }

    pub fn width(&self) -> usize {
        let ch = std::char::from_u32(self.codepoint).unwrap_or('\0');
        ch.width().unwrap_or(0)
    }
}

pub fn is_separator(c: char) -> bool {
    !c.is_alphanumeric()
}

#[allow(unused)]
pub fn to_lowercase(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

#[allow(unused)]
pub fn to_uppercase(c: char) -> char {
    c.to_uppercase().next().unwrap_or(c)
}

#[allow(unused)]
pub fn mirror_left(c: char) -> Option<char> {
    match c {
        ')' => Some('('),
        ']' => Some('['),
        '}' => Some('{'),
        '>' => Some('<'),
        _ => None,
    }
}

#[allow(unused)]
pub fn mirror_right(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '<' => Some('>'),
        _ => None,
    }
}
