//! A writer for text files — the smallest one that is honest.
//!
//! It edits what fits in memory, it saves the whole file at once, and it never
//! pretends to be more: no undo history, no syntax awareness, no autosave. What
//! it does promise is that your file is either the old one or the new one and
//! never something in between, which is why saving writes a neighbour first and
//! then renames it into place.

use std::path::{Path, PathBuf};

/// Above this a file is not something you glance at and fix; open it in the
/// tool that was built for it.
const MOST: u64 = 4 * 1024 * 1024;

pub struct Editor {
    pub path: PathBuf,
    pub name: String,
    pub lines: Vec<String>,
    /// Where the caret is, counted in characters rather than bytes.
    pub row: usize,
    pub col: usize,
    pub offset: usize,
    pub dirty: bool,
    /// Set while the question "you have changes" is on screen.
    pub asking: bool,
    pub note: Option<String>,
    /// Drawn over the whole screen even when the pane could hold it — set
    /// when the writer was opened from the look window, so closing can go
    /// back there.
    pub windowed: bool,
    /// Whether the file ended with a newline, so saving does not quietly add
    /// or drop one.
    trailing_newline: bool,
}

impl Editor {
    pub fn open(path: &Path, name: &str) -> Result<Editor, String> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            return Err("a folder cannot be edited".to_string());
        }
        if meta.len() > MOST {
            return Err(format!(
                "{} is too large to edit here",
                crate::fsmodel::human_size(meta.len(), false)
            ));
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        if bytes.contains(&0) {
            return Err("not a text file".to_string());
        }
        let text = String::from_utf8(bytes).map_err(|_| "not readable text".to_string())?;
        let trailing_newline = text.ends_with('\n');
        let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Ok(Editor {
            path: path.to_path_buf(),
            name: name.to_string(),
            lines,
            row: 0,
            col: 0,
            offset: 0,
            dirty: false,
            asking: false,
            note: None,
            windowed: false,
            trailing_newline,
        })
    }

    fn line(&self) -> &String {
        &self.lines[self.row.min(self.lines.len() - 1)]
    }

    /// The caret in characters, which is what the screen counts, converted to
    /// the byte offset that a String wants.
    fn byte_at(&self, col: usize) -> usize {
        self.line()
            .char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or(self.line().len())
    }

    pub fn chars_in_line(&self) -> usize {
        self.line().chars().count()
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte_at(self.col);
        self.lines[self.row].insert(at, c);
        self.col += 1;
        self.dirty = true;
    }

    pub fn split_line(&mut self) {
        let at = self.byte_at(self.col);
        let rest = self.lines[self.row].split_off(at);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
        self.dirty = true;
    }

    /// Backspace: a character, or the line break that put you here.
    pub fn delete_before(&mut self) {
        if self.col > 0 {
            let at = self.byte_at(self.col - 1);
            self.lines[self.row].remove(at);
            self.col -= 1;
            self.dirty = true;
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.chars_in_line();
            self.lines[self.row].push_str(&line);
            self.dirty = true;
        }
    }

    pub fn delete_at(&mut self) {
        if self.col < self.chars_in_line() {
            let at = self.byte_at(self.col);
            self.lines[self.row].remove(at);
            self.dirty = true;
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
            self.dirty = true;
        }
    }

    pub fn move_to(&mut self, row: isize, col: isize) {
        let rows = self.lines.len() as isize;
        self.row = row.clamp(0, rows - 1) as usize;
        self.col = col.clamp(0, self.chars_in_line() as isize) as usize;
    }

    pub fn up(&mut self) {
        if self.row > 0 {
            let want = self.col;
            self.row -= 1;
            self.col = want.min(self.chars_in_line());
        }
    }

    pub fn down(&mut self) {
        if self.row + 1 < self.lines.len() {
            let want = self.col;
            self.row += 1;
            self.col = want.min(self.chars_in_line());
        }
    }

    pub fn left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.chars_in_line();
        }
    }

    pub fn right(&mut self) {
        if self.col < self.chars_in_line() {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    pub fn text(&self) -> String {
        let mut text = self.lines.join("\n");
        if self.trailing_newline {
            text.push('\n');
        }
        text
    }

    /// Writes a neighbour and renames it over the original, so a crash halfway
    /// leaves the old file whole rather than half a new one.
    pub fn save(&mut self) -> Result<(), String> {
        let parent = self.path.parent().ok_or("nowhere to write")?;
        let scratch = parent.join(format!(".{}.fsctl-{}", self.name, std::process::id()));
        std::fs::write(&scratch, self.text()).map_err(|e| e.to_string())?;

        // Keep whatever permissions the file had; a fresh file would not have
        // them, and an executable script must stay executable.
        if let Ok(meta) = std::fs::metadata(&self.path) {
            let _ = std::fs::set_permissions(&scratch, meta.permissions());
        }
        if let Err(e) = std::fs::rename(&scratch, &self.path) {
            let _ = std::fs::remove_file(&scratch);
            return Err(e.to_string());
        }
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(lines: &[&str]) -> Editor {
        Editor {
            path: PathBuf::from("/tmp/x"),
            name: "x".to_string(),
            lines: lines.iter().map(|l| l.to_string()).collect(),
            row: 0,
            col: 0,
            offset: 0,
            dirty: false,
            asking: false,
            note: None,
            windowed: false,
            trailing_newline: true,
        }
    }

    #[test]
    fn typing_lands_where_the_caret_is() {
        let mut e = buffer(&["ac"]);
        e.col = 1;
        e.insert('b');
        assert_eq!(e.lines[0], "abc");
        assert_eq!(e.col, 2);
        assert!(e.dirty);
    }

    #[test]
    fn enter_splits_and_backspace_joins_again() {
        let mut e = buffer(&["hello world"]);
        e.col = 5;
        e.split_line();
        assert_eq!(e.lines, vec!["hello", " world"]);
        assert_eq!((e.row, e.col), (1, 0));
        e.delete_before();
        assert_eq!(e.lines, vec!["hello world"]);
        assert_eq!((e.row, e.col), (0, 5));
    }

    #[test]
    fn the_caret_counts_characters_not_bytes() {
        let mut e = buffer(&["café"]);
        e.col = 4;
        e.insert('!');
        assert_eq!(e.lines[0], "café!");
    }

    #[test]
    fn a_missing_trailing_newline_is_not_invented() {
        let mut e = buffer(&["one", "two"]);
        e.trailing_newline = false;
        assert_eq!(e.text(), "one\ntwo");
        e.trailing_newline = true;
        assert_eq!(e.text(), "one\ntwo\n");
    }
}
