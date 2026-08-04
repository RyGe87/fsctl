//! What is in a directory, and how to put it in order.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    /// A symlink stays a symlink here: we show what is on disk, and copying
    /// carries the link across rather than writing out its target.
    pub is_link: bool,
    pub size: u64,
    pub mtime: i64,
    /// Git's two-letter porcelain code, when a repository has an opinion.
    pub git: Option<String>,
}

impl Entry {
    /// What the type column shows: the extension, or what it is instead.
    pub fn kind(&self) -> String {
        if self.is_link {
            return "link".to_string();
        }
        if self.is_dir {
            return "map".to_string();
        }
        match self.path.extension().and_then(|e| e.to_str()) {
            Some(ext) if !ext.is_empty() => ext.to_lowercase(),
            _ => "—".to_string(),
        }
    }
}

pub fn entry_for(path: &Path) -> Option<Entry> {
    let name = path.file_name()?.to_string_lossy().to_string();
    // symlink_metadata, not metadata: a link to a directory is a link, and a
    // link whose target is gone still deserves a row.
    let meta = fs::symlink_metadata(path).ok()?;
    let is_link = meta.file_type().is_symlink();
    let is_dir = if is_link {
        fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
    } else {
        meta.is_dir()
    };
    Some(Entry {
        path: path.to_path_buf(),
        name,
        is_dir,
        is_link,
        size: meta.len(),
        mtime: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        git: None,
    })
}

/// Everything in a directory. Unreadable directories give an empty list rather
/// than an error: the pane says "0 items" and you move on.
pub fn read_dir(path: &Path, show_hidden: bool) -> Vec<Entry> {
    let Ok(iter) = fs::read_dir(path) else {
        return Vec::new();
    };
    iter.flatten()
        .filter(|e| show_hidden || !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| entry_for(&e.path()))
        .collect()
}

/// What the tree needs to know about a folder before drawing its row.
///
/// Two different questions live here, and they must not be answered the same
/// way. The triangle is about what *unfolding* would show, so it follows the
/// hidden-files setting — a triangle that opens onto nothing is a lie. The
/// cross is about what is *there*, hidden files included, because a copy, a
/// move and one day a delete act on the folder itself and not on our filtered
/// view of it. A folder that only holds a `.env` may never be called empty.
#[derive(Debug, Clone, Copy, Default)]
pub struct Probe {
    /// Holds at least one folder the pane would show.
    pub has_subdir: bool,
    /// Holds nothing whatsoever — hidden entries included.
    pub empty: bool,
    /// Holds something, but nothing the pane is currently showing.
    pub hidden_only: bool,
}

/// Answers all of it in one pass over the directory.
///
/// Stops at the first shown folder instead of listing everything: a folder with
/// ten thousand files should not cost ten thousand entries to learn that its
/// first child is a folder. A directory we may not read reports nothing at all
/// — we do not know, and opening it would tell you no more.
pub fn probe(path: &Path, show_hidden: bool) -> Probe {
    let Ok(iter) = fs::read_dir(path) else {
        return Probe::default();
    };
    let mut anything = false;
    let mut shown = false;
    for entry in iter.flatten() {
        anything = true;
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        if hidden && !show_hidden {
            continue;
        }
        shown = true;
        let is_dir = match entry.file_type() {
            // file_type() comes free with the directory read; only a symlink
            // costs an extra look to see what it points at.
            Ok(kind) if kind.is_symlink() => fs::metadata(entry.path())
                .map(|m| m.is_dir())
                .unwrap_or(false),
            Ok(kind) => kind.is_dir(),
            Err(_) => false,
        };
        if is_dir {
            return Probe {
                has_subdir: true,
                empty: false,
                hidden_only: false,
            };
        }
    }
    Probe {
        has_subdir: false,
        empty: !anything,
        hidden_only: anything && !shown,
    }
}

/// Just the directories, for the tree on the left.
pub fn subdirectories(path: &Path, show_hidden: bool) -> Vec<Entry> {
    let mut dirs: Vec<Entry> = read_dir(path, show_hidden)
        .into_iter()
        .filter(|e| e.is_dir)
        .collect();
    dirs.sort_by(|a, b| natural(&a.name, &b.name));
    dirs
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Name,
    Kind,
    Date,
}

impl Sort {
    pub fn next(self) -> Sort {
        match self {
            Sort::Name => Sort::Kind,
            Sort::Kind => Sort::Date,
            Sort::Date => Sort::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Sort::Name => "naam",
            Sort::Kind => "type",
            Sort::Date => "datum",
        }
    }
}

/// Directories first, always. Sorting a listing so that folders scatter between
/// files reads as noise, whichever column you chose.
pub fn sort(entries: &mut [Entry], by: Sort, reverse: bool) {
    entries.sort_by(|a, b| {
        let order = b.is_dir.cmp(&a.is_dir).then_with(|| match by {
            Sort::Name => natural(&a.name, &b.name),
            Sort::Kind => a.kind().cmp(&b.kind()).then_with(|| natural(&a.name, &b.name)),
            // Newest first is what you mean by "sort by date".
            Sort::Date => b.mtime.cmp(&a.mtime),
        });
        if reverse { order.reverse() } else { order }
    });
}

/// Case-insensitive, and digits compare as numbers so `v2` lands before `v10`.
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let mut left = a.chars().peekable();
    let mut right = b.chars().peekable();
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(l), Some(r)) => {
                if l.is_ascii_digit() && r.is_ascii_digit() {
                    let ln = take_number(&mut left);
                    let rn = take_number(&mut right);
                    match ln.cmp(&rn) {
                        std::cmp::Ordering::Equal => continue,
                        other => return other,
                    }
                }
                let (lc, rc) = (l.to_lowercase().next().unwrap_or(l), r.to_lowercase().next().unwrap_or(r));
                match lc.cmp(&rc) {
                    std::cmp::Ordering::Equal => {
                        left.next();
                        right.next();
                    }
                    other => return other,
                }
            }
        }
    }
}

fn take_number(it: &mut std::iter::Peekable<std::str::Chars>) -> u128 {
    let mut n: u128 = 0;
    while let Some(c) = it.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        // A number longer than a u128 is not a version, it is a hash: stop
        // growing and let the rest compare as text.
        n = n.saturating_mul(10).saturating_add((c as u8 - b'0') as u128);
        it.next();
    }
    n
}

pub fn human_size(bytes: u64, is_dir: bool) -> String {
    if is_dir {
        return "—".to_string();
    }
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if size < 10.0 {
        format!("{size:.1} {}", UNITS[unit])
    } else {
        format!("{size:.0} {}", UNITS[unit])
    }
}

/// The offset between UTC and here, asked once of the system that knows.
///
/// Reading the zone rules ourselves would mean parsing /etc/localtime; `date`
/// already did that work, and one process at startup is cheaper than a parser.
fn local_offset() -> i64 {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        let Ok(out) = std::process::Command::new("/bin/date").arg("+%z").output() else {
            return 0;
        };
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // "+0200" — sign, two hours, two minutes.
        if text.len() < 5 {
            return 0;
        }
        let sign = if text.starts_with('-') { -1 } else { 1 };
        let hours: i64 = text[1..3].parse().unwrap_or(0);
        let minutes: i64 = text[3..5].parse().unwrap_or(0);
        sign * (hours * 3600 + minutes * 60)
    })
}

/// Seconds since the epoch as `YYYY-MM-DD HH:MM`, in local time.
pub fn format_time(epoch: i64) -> String {
    if epoch == 0 {
        return "—".to_string();
    }
    let local = epoch + local_offset();
    let days = local.div_euclid(86_400);
    let seconds = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        seconds / 3600,
        (seconds % 3600) / 60
    )
}

/// Howard Hinnant's civil-from-days: days since 1970-01-01 to a calendar date,
/// no lookup tables and no leap-year special cases.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_sort_as_numbers() {
        assert_eq!(natural("v2", "v10"), std::cmp::Ordering::Less);
        assert_eq!(natural("Apple", "apple"), std::cmp::Ordering::Equal);
        assert_eq!(natural("a", "b"), std::cmp::Ordering::Less);
    }

    #[test]
    fn epoch_zero_is_1970() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
    }

    #[test]
    fn sizes_read_like_sizes() {
        assert_eq!(human_size(512, false), "512 B");
        assert_eq!(human_size(2048, false), "2.0 K");
        assert_eq!(human_size(0, true), "—");
    }
}
