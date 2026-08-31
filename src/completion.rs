//! Tab completion.
//!
//! A provider reports *where* in the line a completion starts as well as what
//! the candidates are. Making the replacement range explicit is what keeps the
//! editor from having to re-derive a word boundary the provider already knew,
//! which is the class of mismatch that made the previous API slice strings at
//! invalid offsets.

use std::path::PathBuf;

/// Candidate completions for a position in a line.
#[derive(Debug, Clone, Default)]
pub struct Completions {
    start: usize,
    candidates: Vec<String>,
}

impl Completions {
    /// Creates an empty set whose candidates replace `line[start..pos]`.
    ///
    /// `start` must be a character boundary at or before the cursor.
    #[must_use]
    pub fn new(start: usize) -> Self {
        Completions {
            start,
            candidates: Vec::new(),
        }
    }

    /// Byte offset in the line where a candidate begins replacing text.
    #[must_use]
    pub fn start(&self) -> usize {
        self.start
    }

    /// Adds a candidate. The candidate is the full replacement text, not a suffix.
    pub fn add(&mut self, candidate: impl Into<String>) {
        self.candidates.push(candidate.into());
    }

    /// The candidates, in the order they will be offered.
    #[must_use]
    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }

    /// Number of candidates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// Whether there are no candidates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Sorts candidates and removes duplicates.
    pub fn sort_dedup(&mut self) {
        self.candidates.sort();
        self.candidates.dedup();
    }

    /// Longest prefix shared by every candidate, as a byte length.
    ///
    /// The result is always a character boundary in every candidate, so it is
    /// safe to use for slicing.
    #[must_use]
    pub fn common_prefix_len(&self) -> usize {
        let Some((first, rest)) = self.candidates.split_first() else {
            return 0;
        };
        let mut len = first.len();
        for other in rest {
            len = len.min(shared_prefix_len(first, other));
            if len == 0 {
                break;
            }
        }
        len
    }
}

/// Byte length of the longest common prefix of `a` and `b`, always landing on a
/// character boundary.
fn shared_prefix_len(a: &str, b: &str) -> usize {
    let mut len = 0;
    for (x, y) in a.chars().zip(b.chars()) {
        if x != y {
            break;
        }
        len += x.len_utf8();
    }
    len
}

/// Supplies completions for a line and cursor position.
pub trait CompletionProvider {
    /// Returns candidates for the word ending at `pos`.
    ///
    /// `pos` is a byte offset into `line` and is always a character boundary.
    fn complete(&self, line: &str, pos: usize) -> Completions;
}

/// A provider that never completes anything.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCompleter;

impl CompletionProvider for NoCompleter {
    fn complete(&self, _line: &str, pos: usize) -> Completions {
        Completions::new(pos)
    }
}

/// Completes filesystem paths, including `~` expansion.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileCompleter;

impl FileCompleter {
    /// Creates a file completer.
    #[must_use]
    pub fn new() -> Self {
        FileCompleter
    }
}

/// Start of the shell-like word ending at `pos`.
///
/// The word extends back to the last whitespace character. Backslashes are not
/// treated as escapes, so `cat foo\ bar` is two words; a provider that needs
/// shell quoting rules has to split the line itself. Path separators do *not*
/// end a word: a completion for `src/buf` has to see the whole `src/buf` in
/// order to know which directory to read.
#[must_use]
pub fn word_start(line: &str, pos: usize) -> usize {
    line[..pos]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8())
}

/// Splits a path-like word into its directory part (with trailing separator)
/// and the file-name prefix being completed.
fn split_path_word(word: &str) -> (&str, &str) {
    match word.rfind('/') {
        Some(i) => word.split_at(i + 1),
        None => ("", word),
    }
}

/// Expands a leading `~` or `~/` to the user's home directory.
fn expand_tilde(dir: &str) -> Option<PathBuf> {
    let rest = dir.strip_prefix('~')?;
    // Only `~` and `~/...` are handled; `~user` needs passwd lookups we do not
    // want to take a dependency on.
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    let home = dirs::home_dir()?;
    Some(home.join(rest.trim_start_matches('/')))
}

impl CompletionProvider for FileCompleter {
    fn complete(&self, line: &str, pos: usize) -> Completions {
        let start = word_start(line, pos);
        let mut completions = Completions::new(start);

        let word = &line[start..pos];
        let (dir_part, prefix) = split_path_word(word);

        let read_from: PathBuf = if dir_part.is_empty() {
            PathBuf::from(".")
        } else if let Some(expanded) = expand_tilde(dir_part) {
            expanded
        } else {
            PathBuf::from(dir_part)
        };

        let Ok(entries) = std::fs::read_dir(&read_from) else {
            return completions;
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with(prefix) {
                continue;
            }
            // The candidate is the whole word, so the editor can replace
            // `line[start..pos]` with it verbatim.
            let mut candidate = format!("{dir_part}{name}");
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                candidate.push('/');
            }
            completions.add(candidate);
        }

        completions.sort_dedup();
        completions
    }
}

/// Completes a fixed list of words at the start of a line, delegating anything
/// later in the line to another provider.
///
/// This is the shape most REPLs want: commands first, then arguments.
pub struct CommandCompleter<P: CompletionProvider> {
    commands: Vec<String>,
    rest: P,
}

impl<P: CompletionProvider> CommandCompleter<P> {
    /// Creates a completer offering `commands` in the first word position.
    pub fn new(commands: impl IntoIterator<Item = impl Into<String>>, rest: P) -> Self {
        let mut commands: Vec<String> = commands.into_iter().map(Into::into).collect();
        commands.sort();
        CommandCompleter { commands, rest }
    }
}

impl<P: CompletionProvider> CompletionProvider for CommandCompleter<P> {
    fn complete(&self, line: &str, pos: usize) -> Completions {
        let start = word_start(line, pos);
        if !line[..start].trim().is_empty() {
            return self.rest.complete(line, pos);
        }
        let word = &line[start..pos];
        let mut completions = Completions::new(start);
        for command in &self.commands {
            if command.starts_with(word) {
                completions.add(command.clone());
            }
        }
        completions
    }
}

/// Formats `candidates` into columns that fit within `width` terminal columns.
///
/// Returns the lines to print, without trailing newlines.
#[must_use]
pub fn format_columns(candidates: &[String], width: usize) -> Vec<String> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let widest = candidates
        .iter()
        .map(|c| crate::unicode::display_width(c))
        .max()
        .unwrap_or(1);
    let column = widest + 2;
    let columns = (width / column).max(1);
    let rows = candidates.len().div_ceil(columns);

    let mut lines = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut line = String::new();
        for col in 0..columns {
            // Column-major, so reading down a column follows sort order.
            let Some(candidate) = candidates.get(col * rows + row) else {
                continue;
            };
            line.push_str(candidate);
            if col + 1 < columns && col * rows + row + rows < candidates.len() {
                let pad = column - crate::unicode::display_width(candidate);
                line.extend(std::iter::repeat_n(' ', pad));
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_start_stops_at_whitespace_only() {
        assert_eq!(word_start("cat src/buf", 11), 4);
        assert_eq!(word_start("cat", 3), 0);
        assert_eq!(word_start("", 0), 0);
        assert_eq!(word_start("a  b", 4), 3);
        // Multi-byte characters do not confuse the boundary scan.
        assert_eq!(word_start("échó xy", 9), 7);
    }

    #[test]
    fn split_path_word_keeps_the_directory() {
        assert_eq!(split_path_word("src/buf"), ("src/", "buf"));
        assert_eq!(split_path_word("buf"), ("", "buf"));
        assert_eq!(split_path_word("/etc/pa"), ("/etc/", "pa"));
        assert_eq!(split_path_word("src/"), ("src/", ""));
    }

    #[test]
    fn common_prefix_is_a_char_boundary() {
        // The regression test for the panic: a multi-byte common prefix must
        // never yield a byte length that splits a character.
        let mut c = Completions::new(0);
        c.add("日本.txt");
        c.add("日月.txt");
        let len = c.common_prefix_len();
        assert_eq!(len, "日".len());
        assert!(c.candidates()[0].is_char_boundary(len));

        let mut c = Completions::new(0);
        c.add("abc");
        c.add("abd");
        assert_eq!(c.common_prefix_len(), 2);

        let mut c = Completions::new(0);
        c.add("only");
        assert_eq!(c.common_prefix_len(), 4);

        let c = Completions::new(0);
        assert_eq!(c.common_prefix_len(), 0);

        let mut c = Completions::new(0);
        c.add("abc");
        c.add("xyz");
        assert_eq!(c.common_prefix_len(), 0);
    }

    #[test]
    fn file_completer_reads_the_right_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/buffer.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/butter.rs"), "").unwrap();
        std::fs::write(dir.path().join("decoy.rs"), "").unwrap();

        let base = dir.path().to_str().unwrap();
        let line = format!("cat {base}/src/buf");
        let c = FileCompleter.complete(&line, line.len());

        assert_eq!(c.candidates().len(), 1);
        assert!(c.candidates()[0].ends_with("src/buffer.rs"));
        // The candidate replaces the whole word, starting after "cat ".
        assert_eq!(c.start(), 4);
        assert_eq!(&line[c.start()..], format!("{base}/src/buf"));
    }

    #[test]
    fn file_completer_marks_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        let base = dir.path().to_str().unwrap();
        let line = format!("cat {base}/bi");
        let c = FileCompleter.complete(&line, line.len());
        assert_eq!(c.candidates().len(), 1);
        assert!(c.candidates()[0].ends_with("bin/"));
    }

    #[test]
    fn file_completer_on_unreadable_directory_is_empty() {
        let c = FileCompleter.complete("cat /nonexistent-xyz/f", 22);
        assert!(c.is_empty());
    }

    #[test]
    fn command_completer_only_fires_in_first_position() {
        let completer = CommandCompleter::new(["help", "history", "exit"], NoCompleter);

        let c = completer.complete("hi", 2);
        assert_eq!(c.candidates(), ["history"]);
        assert_eq!(c.start(), 0);

        let c = completer.complete("h", 1);
        assert_eq!(c.candidates(), ["help", "history"]);

        // Second word delegates.
        let c = completer.complete("echo h", 6);
        assert!(c.is_empty());
    }

    #[test]
    fn columns_fit_the_width() {
        let items: Vec<String> = (0..6).map(|i| format!("item{i}")).collect();
        let lines = format_columns(&items, 40);
        assert!(!lines.is_empty());
        for line in &lines {
            assert!(crate::unicode::display_width(line) <= 40);
        }
        // Every candidate appears exactly once.
        let joined = lines.join(" ");
        for item in &items {
            assert_eq!(joined.matches(item.as_str()).count(), 1);
        }
    }

    #[test]
    fn columns_handle_a_narrow_terminal() {
        let items = vec!["a-very-long-candidate-name".to_string(), "b".to_string()];
        let lines = format_columns(&items, 10);
        assert_eq!(lines.len(), 2);
    }
}
