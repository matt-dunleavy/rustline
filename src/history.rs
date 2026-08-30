use crate::error::Result;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct History {
    entries: Vec<String>,
    max_size: usize,
    current_index: usize,
    temp_entry: Option<String>,
}

impl History {
    pub fn new(max_size: usize) -> Self {
        History {
            entries: Vec::new(),
            max_size,
            current_index: 0,
            temp_entry: None,
        }
    }

    pub fn add(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }

        if self.entries.last().is_some_and(|last| last == line) {
            return;
        }

        self.entries.push(line.to_string());

        if self.entries.len() > self.max_size {
            self.entries.remove(0);
        }

        self.current_index = self.entries.len();
        self.temp_entry = None;
    }

    pub fn previous(&mut self, current: &str) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }

        if self.current_index == self.entries.len() {
            self.temp_entry = Some(current.to_string());
        }

        if self.current_index > 0 {
            self.current_index -= 1;
            Some(&self.entries[self.current_index])
        } else {
            None
        }
    }

    pub fn next_entry(&mut self) -> Option<&str> {
        if self.current_index < self.entries.len() {
            self.current_index += 1;
        }

        if self.current_index == self.entries.len() {
            self.temp_entry.as_deref()
        } else if self.current_index < self.entries.len() {
            Some(&self.entries[self.current_index])
        } else {
            None
        }
    }

    pub fn search_backward(&self, pattern: &str, start_index: usize) -> Option<(usize, &str)> {
        if pattern.is_empty() {
            return None;
        }

        for i in (0..=start_index.min(self.entries.len().saturating_sub(1))).rev() {
            if self.entries[i].contains(pattern) {
                return Some((i, &self.entries[i]));
            }
        }
        None
    }

    pub fn load<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        let reader = BufReader::new(file);
        self.entries.clear();

        for line in reader.lines() {
            let line = line?;
            if !line.is_empty() {
                self.entries.push(line);
                if self.entries.len() >= self.max_size {
                    self.entries.remove(0);
                }
            }
        }

        self.current_index = self.entries.len();
        Ok(())
    }

    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;

        for entry in &self.entries {
            writeln!(file, "{}", entry)?;
        }

        Ok(())
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.current_index = 0;
        self.temp_entry = None;
    }
}
