//! Inline hints shown after the cursor.
//!
//! A hint is advisory text drawn to the right of the line in a dim colour. It
//! is never part of the buffer and never returned from `readline`.

/// Text to display after the cursor, with the escape sequences that colour it.
#[derive(Debug, Clone)]
pub struct Hint {
    /// The hint text. Must not contain newlines.
    pub text: String,
    /// Escape sequence emitted before the text.
    pub before: String,
    /// Escape sequence emitted after the text.
    pub after: String,
}

impl Hint {
    /// Creates a hint using the default dim-grey styling.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Hint {
            text: text.into(),
            before: "\x1b[90m".to_string(),
            after: "\x1b[39m".to_string(),
        }
    }

    /// Overrides the escape sequences bracketing the hint.
    #[must_use]
    pub fn styled(mut self, before: impl Into<String>, after: impl Into<String>) -> Self {
        self.before = before.into();
        self.after = after.into();
        self
    }

    /// Renders the hint with its styling applied.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{}{}{}", self.before, self.text, self.after)
    }
}

/// Supplies a hint for the line being edited.
pub trait HintProvider {
    /// Returns a hint to draw after the cursor, or `None` for no hint.
    fn hint(&self, line: &str, pos: usize) -> Option<Hint>;
}

/// A provider that never hints.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHinter;

impl HintProvider for NoHinter {
    fn hint(&self, _line: &str, _pos: usize) -> Option<Hint> {
        None
    }
}

impl<F> HintProvider for F
where
    F: Fn(&str, usize) -> Option<Hint>,
{
    fn hint(&self, line: &str, pos: usize) -> Option<Hint> {
        self(line, pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_styling_wraps_the_text() {
        let hint = Hint::new(" (try --help)");
        assert_eq!(hint.render(), "\x1b[90m (try --help)\x1b[39m");
    }

    #[test]
    fn custom_styling_is_used() {
        let hint = Hint::new("x").styled("\x1b[31m", "\x1b[0m");
        assert_eq!(hint.render(), "\x1b[31mx\x1b[0m");
    }

    #[test]
    fn closures_implement_the_trait() {
        let provider = |line: &str, _pos: usize| {
            if line == "git" {
                Some(Hint::new(" status"))
            } else {
                None
            }
        };
        assert!(provider.hint("git", 3).is_some());
        assert!(provider.hint("cargo", 5).is_none());
        assert!(NoHinter.hint("anything", 0).is_none());
    }
}
