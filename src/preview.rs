//! A look inside a file, without leaving the tool.
//!
//! Only text, and only the head of it: this is a glance to confirm you have the
//! right file, not a reader. Whether something *is* text is decided by its
//! bytes rather than its name — plenty of text carries no extension (`Makefile`,
//! `.zshrc`) and plenty of binaries carry a familiar one.
//!
//! Structured formats get laid out first, by the tools macOS already ships:
//! `plutil` for JSON and property lists, `xmllint` for XML. We do not write
//! parsers here; we ask the ones that are already installed, and when they
//! refuse, their complaint is the most useful thing on the screen — a JSON file
//! that will not format is a JSON file that is broken, and plutil says where.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// How much we are willing to read. Enough to fill any screen several times
/// over, small enough that opening a video by accident costs nothing.
const LIMIT: usize = 128 * 1024;

/// Beyond this a line is almost certainly minified or generated, and cutting it
/// keeps the drawing honest.
const LONGEST_LINE: usize = 2000;

/// Formatting reads the *whole* file, unlike the glance itself, so it only
/// happens while that is still cheap.
const FORMAT_LIMIT: u64 = 4 * 1024 * 1024;

pub enum Preview {
    Text {
        /// What to show: the formatted version when there is one.
        lines: Vec<String>,
        /// The file as it actually reads, kept so the formatting can be
        /// switched off again.
        raw: Option<Vec<String>>,
        /// The formatter used, the reason it declined, or the fact that the
        /// file runs on past what we read.
        note: Option<String>,
    },
    NotText(String),
}

/// The tool that lays out this kind of file, if macOS has one.
fn formatter(path: &Path) -> Option<(&'static str, Command)> {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "json" | "geojson" | "webmanifest" | "jsonc" => {
            let mut c = Command::new("/usr/bin/plutil");
            c.args(["-convert", "json", "-r", "-o", "-", "--"]).arg(path);
            Some(("plutil", c))
        }
        "plist" | "entitlements" | "strings" => {
            let mut c = Command::new("/usr/bin/plutil");
            c.args(["-convert", "xml1", "-o", "-", "--"]).arg(path);
            Some(("plutil", c))
        }
        "xml" | "svg" | "xsl" | "xsd" | "xib" | "storyboard" | "pbxproj" | "rss" | "atom" => {
            let mut c = Command::new("/usr/bin/xmllint");
            c.arg("--format").arg(path);
            Some(("xmllint", c))
        }
        _ => None,
    }
}

/// Runs a formatter, briefly. Gives back its output, or its complaint.
fn format_with(mut command: Command, limit: Duration) -> Result<String, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("duurde te lang".to_string());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).to_string());
    }
    // The complaint, with the file name it repeats back at us trimmed off.
    let message = String::from_utf8_lossy(&out.stderr);
    Err(message
        .lines()
        .next()
        .map(|line| line.rsplit(": ").next().unwrap_or(line).trim().to_string())
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| "kon niet worden opgemaakt".to_string()))
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

    let size_ok = meta.len() <= FORMAT_LIMIT;

    // A binary property list is binary, and still perfectly readable once
    // plutil has turned it back into XML.
    if buffer.starts_with(b"bplist00") {
        if !size_ok {
            return Preview::NotText("binaire plist, te groot om om te zetten".to_string());
        }
        let mut c = Command::new("/usr/bin/plutil");
        c.args(["-convert", "xml1", "-o", "-", "--"]).arg(path);
        return match format_with(c, Duration::from_secs(5)) {
            Ok(text) => Preview::Text {
                lines: split(&text),
                raw: None,
                note: Some("binaire plist, omgezet door plutil".to_string()),
            },
            Err(e) => Preview::NotText(format!("binaire plist — {e}")),
        };
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
    let raw = split(&text);
    let clipped = (meta.len() as usize) > buffer.len();

    if let Some((tool, command)) = formatter(path)
        && size_ok
    {
        match format_with(command, Duration::from_secs(5)) {
            Ok(formatted) => {
                return Preview::Text {
                    lines: split(&formatted),
                    raw: Some(raw),
                    note: Some(format!("opgemaakt door {tool} · t toont het origineel")),
                };
            }
            // The refusal is the news: this file does not parse.
            Err(complaint) => {
                return Preview::Text {
                    lines: raw,
                    raw: None,
                    note: Some(format!("⚠ {complaint}")),
                };
            }
        }
    }

    Preview::Text {
        lines: raw,
        raw: None,
        note: clipped.then(|| "… alleen het begin van het bestand".to_string()),
    }
}

fn split(text: &str) -> Vec<String> {
    text.lines()
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
        .collect()
}
