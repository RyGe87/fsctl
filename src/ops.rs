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

use crate::toolbox::{self, CopyStyle, TrashStyle};
use std::process::{Command, Stdio};

use std::time::Duration;

/// Runs a command and gives up after `limit`, reporting whether it succeeded.
///
/// The child stays ours so that giving up can actually kill it: an osascript
/// left running would come back minutes later and delete what we have since
/// moved by hand.
fn succeeds_within(mut command: Command, limit: Duration) -> bool {
    let Ok(mut child) = command.stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    let deadline = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => return false,
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

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
        self.summary_of(match mode {
            Mode::Copy => "copied",
            Mode::Cut => "moved",
        })
    }

    pub fn summary_of(&self, verb: &str) -> String {
        let mut text = format!("{} {verb}", self.done);
        if self.skipped > 0 {
            text.push_str(&format!(", {} skipped", self.skipped));
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
                .push(format!("{name}: cannot paste into itself"));
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
    let tools = toolbox::get();
    let cp = tools.cp.as_ref().ok_or("no cp on this machine")?;
    match tools.copy_style {
        // The clone first; it refuses across volumes and on other filesystems,
        // and then plain -R does the ordinary thing.
        CopyStyle::Bsd => match run(cp, &["-Rc"], src, target) {
            Ok(()) => Ok(()),
            Err(_) => run(cp, &["-R"], src, target),
        },
        // -a keeps what -Rc keeps; the reflink clones where the filesystem can
        // and quietly copies where it cannot.
        CopyStyle::Gnu => match run(cp, &["-a", "--reflink=auto"], src, target) {
            Ok(()) => Ok(()),
            Err(_) => run(cp, &["-a"], src, target),
        },
    }
}

fn move_to(src: &Path, target: &Path) -> Result<(), String> {
    let mv = toolbox::get().mv.as_ref().ok_or("no mv on this machine")?;
    run(mv, &["-f"], src, target)
}

fn run(program: &Path, flags: &[&str], src: &Path, target: &Path) -> Result<(), String> {
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
        .unwrap_or("failed")
        .to_string())
}

/// Puts things in the trash — Finder's trash, with "Zet terug" intact.
///
/// Finder is asked through osascript, because the trash is more than a folder:
/// the put-back path is recorded by whoever does the moving, and only Finder
/// records it. Paths travel as arguments rather than inside the script text, so
/// a quote in a file name cannot become part of the program.
///
/// If Finder will not play along — automation not permitted, Finder not running
/// — the files still go to `~/.Trash` by hand. They are then recoverable but
/// without put-back, which beats leaving them where they are and saying nothing.
pub fn trash(items: &[PathBuf]) -> Outcome {
    let mut outcome = Outcome {
        done: 0,
        skipped: 0,
        errors: Vec::new(),
    };
    if items.is_empty() {
        return outcome;
    }
    // An escape hatch for anyone who would rather not have Finder involved at
    // all: FSCTL_TRASH=plain skips it and moves the files itself.
    let tools = toolbox::get();
    if std::env::var("FSCTL_TRASH").as_deref() == Ok("plain")
        || tools.trash == TrashStyle::Freedesktop
    {
        return by_hand(items, outcome);
    }
    let Some(osascript) = tools.osascript.as_ref() else {
        return by_hand(items, outcome);
    };

    let mut script = Command::new(osascript);
    script
        .args(["-e", "on run argv"])
        .args(["-e", "set out to {}"])
        .args(["-e", "repeat with p in argv"])
        .args(["-e", "set end of out to (POSIX file (p as text)) as alias"])
        .args(["-e", "end repeat"])
        .args(["-e", "tell application \"Finder\" to delete out"])
        .args(["-e", "end run"]);
    for item in items {
        script.arg(item);
    }

    // Finder is normally instant, but a beachballed Finder must not take the
    // file manager with it.
    if succeeds_within(script, Duration::from_secs(15)) {
        outcome.done = items.len();
        return outcome;
    }

    by_hand(items, outcome)
}

/// The fallback, and the whole of it when Finder is not wanted: move into
/// `~/.Trash` ourselves. Recoverable, but without put-back.
fn by_hand(items: &[PathBuf], mut outcome: Outcome) -> Outcome {
    let Ok(home) = std::env::var("HOME") else {
        outcome.errors.push("no home directory".to_string());
        return outcome;
    };
    let freedesktop = toolbox::get().trash == TrashStyle::Freedesktop;
    // The spec puts the trash under XDG_DATA_HOME and wants a note beside every
    // file saying where it came from; macOS just has ~/.Trash.
    let trash = if freedesktop {
        std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Path::new(&home).join(".local/share"))
            .join("Trash")
    } else {
        Path::new(&home).join(".Trash")
    };
    let files = if freedesktop {
        trash.join("files")
    } else {
        trash.clone()
    };
    if std::fs::create_dir_all(&files).is_err() {
        outcome
            .errors
            .push(format!("cannot reach {}", files.display()));
        return outcome;
    }

    for item in items {
        let Some(name) = item.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        let target = if files.join(&name).symlink_metadata().is_ok() {
            free_name(&files, &name)
        } else {
            files.join(&name)
        };
        if let Err(e) = move_to(item, &target) {
            outcome.errors.push(format!("{name}: {e}"));
            continue;
        }
        outcome.done += 1;
        if freedesktop {
            write_trashinfo(&trash, &target, item);
        }
    }
    if outcome.done > 0 && outcome.errors.is_empty() && !freedesktop {
        outcome
            .errors
            .push("via ~/.Trash, without Put Back".to_string());
    }
    outcome
}

/// The note the freedesktop spec wants beside a trashed file, so a desktop can
/// put it back where it came from.
fn write_trashinfo(trash: &Path, moved: &Path, came_from: &Path) {
    let Some(name) = moved.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return;
    };
    let info = trash.join("info");
    if std::fs::create_dir_all(&info).is_err() {
        return;
    }
    let text = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        percent_encode(&came_from.display().to_string()),
        crate::fsmodel::now_iso()
    );
    let _ = std::fs::write(info.join(format!("{name}.trashinfo")), text);
}

/// The spec stores the original path URL-encoded, slashes and all else intact.
fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Packs things into a zip, in the folder you are standing in.
///
/// `zip -r` runs from the deepest folder that holds all of them, with relative
/// names, so the archive carries the same shape the files had — and not the
/// `/Users/you/…` of the machine it was made on.
pub fn compress(items: &[PathBuf], dest_dir: &Path) -> Result<PathBuf, String> {
    if items.is_empty() {
        return Err("nothing selected".to_string());
    }
    let base = common_parent(items).ok_or("no common folder")?;
    let relative: Vec<PathBuf> = items
        .iter()
        .map(|p| p.strip_prefix(&base).unwrap_or(p).to_path_buf())
        .collect();

    // One item lends its own name; several take the name of the folder they
    // are going into, which is what you would have called it yourself.
    let stem = if items.len() == 1 {
        items[0]
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "archive".to_string())
    } else {
        dest_dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "archive".to_string())
    };
    let target = if dest_dir.join(format!("{stem}.zip")).symlink_metadata().is_ok() {
        free_name(dest_dir, &format!("{stem}.zip"))
    } else {
        dest_dir.join(format!("{stem}.zip"))
    };

    let zip = toolbox::get().zip.as_ref().ok_or("no zip on this machine")?;
    let out = Command::new(zip)
        .current_dir(&base)
        .args(["-r", "-q"])
        .arg(&target)
        .args(&relative)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or("packing failed")
            .trim()
            .to_string());
    }
    Ok(target)
}

/// The deepest folder that holds all of them.
fn common_parent(items: &[PathBuf]) -> Option<PathBuf> {
    let mut base = items.first()?.parent()?.to_path_buf();
    for item in items.iter().skip(1) {
        let parent = item.parent()?;
        while !parent.starts_with(&base) {
            base = base.parent()?.to_path_buf();
        }
    }
    Some(base)
}

/// Hands a file to whatever macOS thinks should open it.
pub fn open(path: &Path) -> Result<(), String> {
    let opener = toolbox::get()
        .open
        .as_ref()
        .ok_or("no way to open files on this machine")?;
    Command::new(opener)
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
