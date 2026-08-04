//! Copying and moving — which we do not do ourselves.
//!
//! `/bin/cp` keeps extended attributes, symlinks and permissions, clones on
//! APFS when it can, and merges directories instead of replacing them (a file
//! that only exists in the destination survives — Finder deletes it). `/bin/mv`
//! handles the volume boundary. Our job is to decide *what* to ask for.
//!
//! Arguments go through `Command` directly, never a shell, so a file named
//! `; rm -rf ~` is just an awkward name.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Copy,
    Cut,
}

#[derive(Debug, Clone)]
pub struct Clipboard {
    pub items: Vec<PathBuf>,
    pub mode: Mode,
}

/// What to do with the names that already exist at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Directories merge, files are replaced.
    Overwrite,
    /// The arrival is renamed to `naam-2`; nothing at the destination is touched.
    KeepBoth,
    Skip,
}

/// The names that would land on something.
///
/// Checked at item level, which is also the level the answer works at: a
/// directory that merges cannot lose a file, and one that is kept as `naam-2`
/// cannot collide deeper down.
pub fn conflicts(items: &[PathBuf], dest: &Path) -> Vec<PathBuf> {
    items
        .iter()
        .filter(|src| {
            src.file_name()
                .map(|n| dest.join(n).symlink_metadata().is_ok())
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// `naam-2`, `naam-3`, … — the first one free, extension kept intact.
fn free_name(dest: &Path, name: &str) -> PathBuf {
    let (stem, ext) = match name.rsplit_once('.') {
        // A leading dot is not an extension: ".gitignore" has no stem.
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
        _ => (name.to_string(), String::new()),
    };
    for n in 2..10_000 {
        let candidate = dest.join(format!("{stem}-{n}{ext}"));
        if candidate.symlink_metadata().is_err() {
            return candidate;
        }
    }
    dest.join(format!("{stem}-{}{ext}", std::process::id()))
}

pub struct Outcome {
    pub done: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

impl Outcome {
    pub fn summary(&self, mode: Mode) -> String {
        let verb = match mode {
            Mode::Copy => "gekopieerd",
            Mode::Cut => "verplaatst",
        };
        let mut text = format!("{} {verb}", self.done);
        if self.skipped > 0 {
            text.push_str(&format!(", {} overgeslagen", self.skipped));
        }
        if let Some(first) = self.errors.first() {
            text.push_str(&format!(" — {first}"));
        }
        text
    }
}

/// Carries out a paste. Every item is asked for separately, so one failure
/// stops that item and nothing else.
pub fn paste(clip: &Clipboard, dest: &Path, how: Resolution) -> Outcome {
    let mut outcome = Outcome {
        done: 0,
        skipped: 0,
        errors: Vec::new(),
    };
    let existing = conflicts(&clip.items, dest);

    for src in &clip.items {
        let Some(name) = src.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        // Pasting into itself, or into its own child, would run away.
        if dest.starts_with(src) {
            outcome
                .errors
                .push(format!("{name}: kan niet in zichzelf plakken"));
            continue;
        }
        let clashes = existing.contains(src);
        let target = match (clashes, how) {
            (true, Resolution::Skip) => {
                outcome.skipped += 1;
                continue;
            }
            (true, Resolution::KeepBoth) => free_name(dest, &name),
            _ => dest.join(&name),
        };

        let result = match clip.mode {
            Mode::Copy => copy(src, &target),
            Mode::Cut => move_to(src, &target),
        };
        match result {
            Ok(()) => outcome.done += 1,
            Err(e) => outcome.errors.push(format!("{name}: {e}")),
        }
    }
    outcome
}

/// `cp -Rc` first: on APFS that is a clone — instant, and no second copy of the
/// bytes on disk. It refuses across volumes and on other filesystems, and then
/// plain `-R` does the ordinary thing.
fn copy(src: &Path, target: &Path) -> Result<(), String> {
    match run("/bin/cp", &["-Rc"], src, target) {
        Ok(()) => Ok(()),
        Err(_) => run("/bin/cp", &["-R"], src, target),
    }
}

fn move_to(src: &Path, target: &Path) -> Result<(), String> {
    run("/bin/mv", &["-f"], src, target)
}

fn run(program: &str, flags: &[&str], src: &Path, target: &Path) -> Result<(), String> {
    let out = Command::new(program)
        .args(flags)
        .arg(src)
        .arg(target)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(&out.stderr);
    // cp says "cp: /path/x: Permission denied"; the path is already on screen.
    Err(message
        .lines()
        .next()
        .and_then(|l| l.rsplit(": ").next())
        .unwrap_or("mislukt")
        .to_string())
}

/// Hands a file to whatever macOS thinks should open it.
pub fn open(path: &Path) -> Result<(), String> {
    Command::new("/usr/bin/open")
        .arg(path)
        .status()
        .map_err(|e| e.to_string())
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_name_keeps_the_extension() {
        let dir = std::env::temp_dir();
        let picked = free_name(&dir, "report.tar.gz");
        let name = picked.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("report.tar-"), "got {name}");
        assert!(name.ends_with(".gz"), "got {name}");
    }

    #[test]
    fn dotfiles_have_no_extension() {
        let dir = std::env::temp_dir();
        let picked = free_name(&dir, ".gitignore");
        let name = picked.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with(".gitignore-"), "got {name}");
    }
}
