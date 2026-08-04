//! A look inside a file, without leaving the tool.
//!
//! Only text, and only the head of it: this is a glance to confirm you have the
//! right file, not a reader. Whether something *is* text is decided by its
//! bytes rather than its name — plenty of text carries no extension (`Makefile`,
//! `.zshrc`) and plenty of binaries carry a familiar one.

use std::io::Read;
use std::path::Path;

/// How much we are willing to read. Enough to fill any screen several times
/// over, small enough that opening a video by accident costs nothing.
const LIMIT: usize = 128 * 1024;

/// Beyond this a line is almost certainly minified or generated, and cutting it
/// keeps the drawing honest.
const LONGEST_LINE: usize = 2000;

pub enum Preview {
    Text {
        lines: Vec<String>,
        /// True when the file goes on past what we read.
        clipped: bool,
    },
    NotText(String),
}

pub fn read(path: &Path) -> Preview {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) => return Preview::NotText(format!("niet te lezen — {e}")),
    };
    if meta.is_dir() {
        return Preview::NotText("dit is een map".to_string());
    }

    let mut buffer = Vec::new();
    match std::fs::File::open(path).and_then(|f| f.take(LIMIT as u64).read_to_end(&mut buffer)) {
        Ok(_) => {}
        Err(e) => return Preview::NotText(format!("niet te lezen — {e}")),
    }
    if buffer.is_empty() {
        return Preview::NotText("leeg bestand".to_string());
    }
    // A zero byte is the oldest and most reliable sign that this is not text.
    if buffer.contains(&0) {
        return Preview::NotText(format!(
            "geen tekstbestand ({})",
            crate::fsmodel::human_size(meta.len(), false)
        ));
    }

    // Cutting at LIMIT can land halfway through a character; drop the tail
    // rather than refuse the file.
    let text = match std::str::from_utf8(&buffer) {
        Ok(text) => text.to_string(),
        Err(e) if e.valid_up_to() > 0 => {
            String::from_utf8_lossy(&buffer[..e.valid_up_to()]).to_string()
        }
        Err(_) => {
            return Preview::NotText(format!(
                "geen leesbare tekst ({})",
                crate::fsmodel::human_size(meta.len(), false)
            ));
        }
    };

    let lines = text
        .lines()
        .map(|line| {
            // Tabs would each eat one cell in our grid and take the alignment
            // with them.
            let expanded = line.replace('\t', "    ");
            if expanded.chars().count() > LONGEST_LINE {
                expanded.chars().take(LONGEST_LINE).collect()
            } else {
                expanded
            }
        })
        .collect();

    Preview::Text {
        lines,
        clipped: (meta.len() as usize) > buffer.len(),
    }
}
