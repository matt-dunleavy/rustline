#[derive(Debug, Default)]
pub struct Completions {
    candidates: Vec<String>,
}

impl Completions {
    pub fn new() -> Self {
        Completions {
            candidates: Vec::new(),
        }
    }

    pub fn add(&mut self, candidate: String) {
        self.candidates.push(candidate);
    }

    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }

    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}
pub trait CompletionProvider {
    fn complete(&self, line: &str, pos: usize) -> Completions;
}

pub struct NoCompleter;

impl CompletionProvider for NoCompleter {
    fn complete(&self, _line: &str, _pos: usize) -> Completions {
        Completions::new()
    }
}

pub struct FileCompleter;

impl FileCompleter {
    pub fn new() -> Self {
        FileCompleter
    }
}

impl Default for FileCompleter {
    fn default() -> Self {
        FileCompleter::new()
    }
}

impl CompletionProvider for FileCompleter {
    fn complete(&self, line: &str, pos: usize) -> Completions {
        use std::fs;

        let mut completions = Completions::new();

        let start = line[..pos]
            .rfind(|c: char| c.is_whitespace() || c == '/' || c == '\\')
            .map(|i| i + 1)
            .unwrap_or(0);

        let word = &line[start..pos];

        let (dir_path, file_prefix) = if let Some(sep_pos) = word.rfind('/') {
            (&word[..=sep_pos], &word[sep_pos + 1..])
        } else if word.is_empty() {
            (".", "")
        } else {
            (".", word)
        };

        let Ok(entries) = fs::read_dir(dir_path) else {
            return completions;
        };

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };

            if !name.starts_with(file_prefix) {
                continue;
            }

            let mut completion = if dir_path == "." && !word.contains('/') {
                name.to_string()
            } else if dir_path.ends_with('/') {
                format!("{}{}", dir_path, name)
            } else {
                format!("{}/{}", dir_path.trim_end_matches('/'), name)
            };

            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                completion.push('/');
            }

            completions.add(completion);
        }
        completions.candidates.sort();

        completions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_completions() {
        let mut completions = Completions::new();
        assert!(completions.is_empty());

        completions.add("test1".to_string());
        completions.add("test2".to_string());

        assert_eq!(completions.len(), 2);
        assert!(!completions.is_empty());
        assert_eq!(completions.candidates(), &["test1", "test2"]);
    }

    #[test]
    fn test_no_completer() {
        let completer = NoCompleter;
        let completions = completer.complete("test", 4);
        assert!(completions.is_empty());
    }
}
