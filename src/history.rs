//! Command history: storage, navigation state, persistence and search.

use crate::error::{Result, RustlineError};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Mode the history file is created and kept at: readable by its owner only.
const PRIVATE: u32 = 0o600;

/// Wraps an I/O failure with the history file it happened on.
fn at(path: &Path, source: std::io::Error) -> RustlineError {
    RustlineError::History {
        path: path.to_path_buf(),
        source,
    }
}

/// Ring of previously entered lines, oldest first.
///
/// The navigation cursor is *not* stored here; it belongs to a single editing
/// session, so two prompts can walk the same history independently. That also
/// mirrors bestline, where moving through history writes the working buffer
/// back into the entry you are leaving, so an edit to a recalled line survives
/// moving away and coming back.
#[derive(Debug, Clone)]
pub struct History {
    entries: VecDeque<String>,
    max_size: usize,
}

impl History {
    /// Creates an empty history holding at most `max_size` entries.
    ///
    /// A `max_size` of zero is treated as one: a history that cannot hold
    /// anything would make every navigation key a silent no-op.
    #[must_use]
    pub fn new(max_size: usize) -> Self {
        History {
            entries: VecDeque::new(),
            max_size: max_size.max(1),
        }
    }

    /// Maximum number of entries retained.
    #[must_use]
    pub fn max_size(&self) -> usize {
        self.max_size
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the history is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates entries from oldest to newest.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &str> + ExactSizeIterator {
        self.entries.iter().map(String::as_str)
    }

    /// Returns entry `index`, counting from the oldest.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&str> {
        self.entries.get(index).map(String::as_str)
    }

    /// Overwrites entry `index`, counting from the oldest.
    pub fn set(&mut self, index: usize, line: &str) {
        if let Some(slot) = self.entries.get_mut(index) {
            slot.clear();
            slot.push_str(line);
        }
    }

    /// Appends `line` unless it duplicates the most recent entry or is empty.
    ///
    /// Returns whether an entry was added.
    pub fn add(&mut self, line: &str) -> bool {
        if line.is_empty() || self.entries.back().is_some_and(|last| last == line) {
            return false;
        }
        self.push(line.to_string());
        true
    }

    /// Appends `line` unconditionally, evicting the oldest entry if full.
    ///
    /// Unlike [`History::add`] this keeps empty lines and consecutive
    /// duplicates, which is what reloading a file or resizing the ring needs.
    pub fn push(&mut self, line: String) {
        self.entries.push_back(line);
        while self.entries.len() > self.max_size {
            self.entries.pop_front();
        }
    }

    /// Removes and returns the newest entry.
    ///
    /// The line being typed is *not* stored here, so this always removes an
    /// entry the user actually entered.
    pub fn pop(&mut self) -> Option<String> {
        self.entries.pop_back()
    }

    /// Removes every entry.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Searches backwards for `needle`, starting at entry `index` and scanning
    /// only the first `limit` bytes of that entry.
    ///
    /// Returns the entry index and the byte offset of the match. Later entries
    /// are searched first, matching the direction of `CTRL-R`.
    #[must_use]
    pub fn search_backward(
        &self,
        needle: &str,
        index: usize,
        limit: usize,
    ) -> Option<(usize, usize)> {
        if needle.is_empty() {
            return None;
        }
        let mut limit = limit;
        for i in (0..=index.min(self.entries.len().checked_sub(1)?)).rev() {
            let entry = &self.entries[i];
            let end = limit.min(entry.len());
            // Only the first candidate is restricted to `limit`; once we move
            // to an older entry the whole of it is eligible.
            limit = usize::MAX;
            let Some(haystack) = entry.get(..end) else {
                continue;
            };
            if let Some(offset) = haystack.rfind(needle) {
                return Some((i, offset));
            }
        }
        None
    }

    /// Replaces the history with the contents of `path`.
    ///
    /// A missing file is not an error and leaves the history untouched. A line
    /// that is not valid UTF-8 is skipped rather than failing the load: one
    /// corrupt byte in a history file must not cost the caller their prompt.
    pub fn load<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path = path.as_ref();
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(at(path, e)),
        };

        // Collect first, then keep the newest `max_size` lines. Trimming as we
        // go would be O(n^2) on a long file.
        let mut reader = BufReader::new(file);
        let mut lines = Vec::new();
        let mut raw = Vec::new();
        loop {
            raw.clear();
            // Read bytes rather than `lines()`, which gives up at the first
            // byte that is not UTF-8 instead of skipping that entry.
            if reader
                .read_until(b'\n', &mut raw)
                .map_err(|e| at(path, e))?
                == 0
            {
                break;
            }
            if raw.last() == Some(&b'\n') {
                raw.pop();
            }
            if let Ok(line) = std::str::from_utf8(&raw) {
                if !line.is_empty() {
                    lines.push(line.to_string());
                }
            }
        }
        if lines.len() > self.max_size {
            lines.drain(..lines.len() - self.max_size);
        }

        self.entries.clear();
        self.entries.extend(lines);
        Ok(())
    }

    /// Writes the history to `path` with mode `0600`.
    ///
    /// History routinely contains credentials, so the file is user-readable
    /// only. The mode is enforced on every save, not just on creation, so a
    /// history file that was already world-readable is tightened rather than
    /// left as it was.
    ///
    /// The write goes to a sibling temporary file that is renamed over `path`,
    /// so a crash or a full disk leaves the previous history intact instead of
    /// a truncated one. `path` is resolved first, so saving through a symlink
    /// replaces its target rather than the link.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        let path = &std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let temp = temp_path(path);

        match self.write_to(&temp) {
            Ok(()) => std::fs::rename(&temp, path).map_err(|e| at(path, e)),
            Err(e) => {
                // Leaving the partial file behind would be picked up as a stale
                // temporary by the next save, or read as history by a glob.
                let _ = std::fs::remove_file(&temp);
                Err(e)
            }
        }
    }

    /// Writes every entry to `temp`, which is created private and made private
    /// again if it already existed with a looser mode.
    fn write_to(&self, temp: &Path) -> Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(PRIVATE)
            .open(temp)
            .map_err(|e| at(temp, e))?;

        // `mode` above applies only when the file is created, so a leftover
        // temporary from a crashed save could still be world-readable.
        file.set_permissions(std::fs::Permissions::from_mode(PRIVATE))
            .map_err(|e| at(temp, e))?;

        let mut out = BufWriter::new(file);
        for entry in &self.entries {
            out.write_all(entry.as_bytes()).map_err(|e| at(temp, e))?;
            out.write_all(b"\n").map_err(|e| at(temp, e))?;
        }
        out.flush().map_err(|e| at(temp, e))?;
        // The rename is only atomic for the caller if the bytes are on disk
        // before it happens; otherwise a crash can leave an empty new file.
        out.into_inner()
            .map_err(|e| at(temp, e.into_error()))?
            .sync_all()
            .map_err(|e| at(temp, e))
    }
}

/// Sibling path used for the temporary file `save` renames into place.
///
/// The pid keeps two processes saving the same history at once from writing
/// through each other's temporary file.
fn temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map_or_else(
        || "history".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let temp = format!(".{name}.{}.tmp", std::process::id());
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(temp),
        _ => PathBuf::from(temp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn add_skips_empty_and_consecutive_duplicates() {
        let mut h = History::new(10);
        assert!(h.add("one"));
        assert!(!h.add("one"));
        assert!(!h.add(""));
        assert!(h.add("two"));
        assert!(h.add("one"));
        assert_eq!(h.iter().collect::<Vec<_>>(), ["one", "two", "one"]);
    }

    #[test]
    fn capacity_is_exactly_max_size() {
        // The old implementation kept max_size - 1 entries on one path and
        // max_size on another; both must now hold exactly max_size.
        let mut h = History::new(3);
        for i in 0..10 {
            h.add(&format!("line{i}"));
        }
        assert_eq!(h.len(), 3);
        assert_eq!(h.iter().collect::<Vec<_>>(), ["line7", "line8", "line9"]);
    }

    #[test]
    fn load_keeps_the_newest_entries_and_matches_add_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hist");
        std::fs::write(&path, "a\nb\nc\nd\ne\n").unwrap();

        let mut h = History::new(3);
        h.load(&path).unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h.iter().collect::<Vec<_>>(), ["c", "d", "e"]);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::new(10);
        h.add("kept");
        h.load(dir.path().join("does-not-exist")).unwrap();
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn save_round_trips_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hist");

        let mut h = History::new(10);
        h.add("first");
        h.add("second");
        h.save(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "history must not be world readable");

        let mut loaded = History::new(10);
        loaded.load(&path).unwrap();
        assert_eq!(loaded.iter().collect::<Vec<_>>(), ["first", "second"]);
    }

    #[test]
    fn save_tightens_a_permissive_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hist");
        std::fs::write(&path, "old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let mut h = History::new(10);
        h.add("new");
        h.save(&path).unwrap();

        // `open` never changes the mode of a file that already exists, so a
        // history left world-readable by an earlier version, or by a careless
        // `touch`, used to stay world-readable forever.
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "an existing history was not tightened");

        let mut loaded = History::new(10);
        loaded.load(&path).unwrap();
        assert_eq!(loaded.iter().collect::<Vec<_>>(), ["new"]);
    }

    #[test]
    fn save_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hist");

        let mut h = History::new(10);
        h.add("only");
        h.save(&path).unwrap();

        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["hist"], "the temporary file was not renamed away");
    }

    #[test]
    fn save_through_a_symlink_replaces_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let link = dir.path().join("link");
        std::fs::write(&real, "old\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut h = History::new(10);
        h.add("new");
        h.save(&link).unwrap();

        assert!(link.is_symlink(), "the symlink was replaced by a file");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new\n");
    }

    #[test]
    fn save_reports_the_file_it_could_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-such-dir").join("hist");

        let err = History::new(10).save(&path).unwrap_err();
        assert!(
            matches!(err, RustlineError::History { .. }),
            "expected a history error, got {err:?}",
        );
        assert!(err.to_string().contains("hist"));
    }

    #[test]
    fn load_skips_lines_that_are_not_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hist");
        // One corrupt entry between two good ones must cost only itself.
        std::fs::write(&path, b"good one\n\xff\xfe bad\ngood two\n").unwrap();

        let mut h = History::new(10);
        h.load(&path).unwrap();
        assert_eq!(h.iter().collect::<Vec<_>>(), ["good one", "good two"]);
    }

    #[test]
    fn load_reads_a_final_line_without_a_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hist");
        std::fs::write(&path, "a\nb").unwrap();

        let mut h = History::new(10);
        h.load(&path).unwrap();
        assert_eq!(h.iter().collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn search_finds_the_most_recent_match() {
        let mut h = History::new(10);
        h.add("git status");
        h.add("cargo build");
        h.add("git commit");

        let (index, offset) = h.search_backward("git", 2, usize::MAX).unwrap();
        assert_eq!(index, 2);
        assert_eq!(offset, 0);

        // Continue searching from the previous entry.
        let (index, _) = h.search_backward("git", 1, usize::MAX).unwrap();
        assert_eq!(index, 0);

        assert_eq!(h.search_backward("nope", 2, usize::MAX), None);
        assert_eq!(h.search_backward("", 2, usize::MAX), None);
    }

    #[test]
    fn search_respects_the_byte_limit() {
        let mut h = History::new(10);
        h.add("abcabc");
        // Limited to the first 4 bytes, the later "abc" at offset 3 is invisible.
        assert_eq!(h.search_backward("abc", 0, 4), Some((0, 0)));
        assert_eq!(h.search_backward("abc", 0, usize::MAX), Some((0, 3)));
    }

    #[test]
    fn search_on_empty_history_is_none() {
        let h = History::new(10);
        assert_eq!(h.search_backward("x", 0, usize::MAX), None);
    }

    #[test]
    fn set_replaces_in_place() {
        let mut h = History::new(10);
        h.add("one");
        h.add("two");
        h.set(0, "ONE");
        assert_eq!(h.get(0), Some("ONE"));
        h.set(99, "ignored");
        assert_eq!(h.len(), 2);
    }
}
