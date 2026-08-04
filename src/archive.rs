//! Looking inside an archive without unpacking it.
//!
//! `unzip -l` says what is in there and `unzip -p` writes one member straight
//! to its output — so a glance costs no temporary file, nothing to clean up,
//! and nothing that can be edited by accident in a place that will vanish.
//! `tar` does the same two tricks for its own family.
//!
//! Taking something out is a separate, deliberate act: it lands in the folder
//! you are standing in, as a file you own.

use std::path::Path;
use std::process::Command;

use crate::toolbox;

#[derive(Debug, Clone)]
pub struct Member {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Zip,
    Tar,
}

fn kind(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    // A doubled extension has to be tested before the single one.
    if name.ends_with(".tar")
        || name.ends_with(".tgz")
        || name.ends_with(".tar.gz")
        || name.ends_with(".tar.bz2")
        || name.ends_with(".tar.xz")
        || name.ends_with(".tbz")
    {
        return Some(Kind::Tar);
    }
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        // .dls is a DiscoveryLab pack, which is a zip with a purpose.
        "zip" | "dls" | "jar" | "epub" | "ipa" | "war" | "xpi" | "aar" => Some(Kind::Zip),
        _ => None,
    }
}

pub fn is_archive(path: &Path) -> bool {
    kind(path).is_some()
}

fn run(mut command: Command) -> Result<Vec<u8>, String> {
    let out = command.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    Err(String::from_utf8_lossy(&out.stderr)
        .lines()
        .next()
        .unwrap_or("could not read the archive")
        .trim()
        .to_string())
}

/// What is in there, without taking any of it out.
pub fn list(path: &Path) -> Result<Vec<Member>, String> {
    match kind(path).ok_or("not an archive")? {
        Kind::Zip => {
            let mut c = Command::new(toolbox::get().unzip.clone().ok_or("unzip is not installed")?);
            c.arg("-l").arg(path);
            Ok(parse_unzip(&String::from_utf8_lossy(&run(c)?)))
        }
        Kind::Tar => {
            let mut c = Command::new(toolbox::get().tar.clone().ok_or("tar is not installed")?);
            c.arg("-tvf").arg(path);
            Ok(parse_tar(&String::from_utf8_lossy(&run(c)?)))
        }
    }
}

/// Everything after the first `n` whitespace-separated fields.
///
/// Not `splitn`: that breaks on every space, and these listings are padded
/// with runs of them.
fn after_fields(line: &str, n: usize) -> Option<&str> {
    let mut rest = line.trim_start();
    for _ in 0..n {
        let end = rest.find(char::is_whitespace)?;
        rest = rest[end..].trim_start();
    }
    Some(rest)
}

/// `  17  08-04-2026 21:05   map/data.json` — three columns, then the name,
/// which may itself hold spaces and so is taken as everything that is left.
fn parse_unzip(text: &str) -> Vec<Member> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim_start().starts_with("---------") {
            // The dashes fence the listing at both ends.
            if inside {
                break;
            }
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        let Some(size) = line
            .split_whitespace()
            .next()
            .and_then(|f| f.parse::<u64>().ok())
        else {
            continue;
        };
        // length, date, time — then the name, spaces and all.
        let Some(name) = after_fields(line, 3).map(|n| n.trim_end().to_string()) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        out.push(Member {
            is_dir: name.ends_with('/'),
            name,
            size,
        });
    }
    out
}

/// `-rw-r--r--  0 geert staff  17 4 Aug 21:05 map/data.json`
fn parse_tar(text: &str) -> Vec<Member> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 9 {
                return None;
            }
            let size = fields[4].parse::<u64>().ok()?;
            // Permissions, links, owner, group, size, and a four-field date.
            let name = after_fields(line, 8)?.trim_end().to_string();
            Some(Member {
                is_dir: line.starts_with('d') || name.ends_with('/'),
                name,
                size,
            })
        })
        .collect()
}

/// Every folder in the archive, including the ones no entry names but a path
/// implies: a zip may hold `a/b/c.txt` without ever listing `a/` or `a/b/`.
pub fn folders(members: &[Member]) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    out.insert(String::new()); // the archive itself
    for member in members {
        let trimmed = member.name.trim_end_matches('/');
        let parts: Vec<&str> = trimmed.split('/').collect();
        // A file's own name is not a folder; a folder entry's is.
        let depth = if member.is_dir {
            parts.len()
        } else {
            parts.len().saturating_sub(1)
        };
        for i in 1..=depth {
            out.insert(parts[..i].join("/"));
        }
    }
    out
}

/// The folders that sit directly inside `dir`.
pub fn folders_in(all: &std::collections::BTreeSet<String>, dir: &str) -> Vec<String> {
    all.iter()
        .filter(|f| !f.is_empty() && parent_of(f) == dir)
        .cloned()
        .collect()
}

/// The files — not folders — that sit directly inside `dir`.
pub fn files_in<'a>(members: &'a [Member], dir: &str) -> Vec<&'a Member> {
    members
        .iter()
        .filter(|m| !m.is_dir && parent_of(m.name.trim_end_matches('/')) == dir)
        .collect()
}

pub fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(at) => path[..at].to_string(),
        None => String::new(),
    }
}

pub fn leaf_of(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(at) => trimmed[at + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

/// One member's bytes, straight from the archive.
pub fn read_member(path: &Path, member: &str) -> Result<Vec<u8>, String> {
    match kind(path).ok_or("not an archive")? {
        Kind::Zip => {
            let mut c = Command::new(toolbox::get().unzip.clone().ok_or("unzip is not installed")?);
            c.arg("-p").arg(path).arg(member);
            run(c)
        }
        Kind::Tar => {
            let mut c = Command::new(toolbox::get().tar.clone().ok_or("tar is not installed")?);
            c.arg("-xOf").arg(path).arg(member);
            run(c)
        }
    }
}

/// Takes one member out, into a folder of your choosing. The inner path is
/// kept, so an archive cannot scatter its contents across your directory.
pub fn extract(path: &Path, member: &str, into: &Path) -> Result<(), String> {
    let command = match kind(path).ok_or("not an archive")? {
        Kind::Zip => {
            let mut c = Command::new(toolbox::get().unzip.clone().ok_or("unzip is not installed")?);
            // -n: an existing file is never overwritten by an archive.
            c.arg("-n").arg(path).arg(member).arg("-d").arg(into);
            c
        }
        Kind::Tar => {
            let mut c = Command::new(toolbox::get().tar.clone().ok_or("tar is not installed")?);
            c.arg("-xf").arg(path).arg("-C").arg(into).arg(member);
            c
        }
    };
    run(command).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dls_pack_is_a_zip() {
        assert!(is_archive(Path::new("/x/honingraat.dls")));
        assert!(is_archive(Path::new("/x/pak.zip")));
        assert!(is_archive(Path::new("/x/bundel.tar.gz")));
        assert!(!is_archive(Path::new("/x/gewoon.txt")));
    }

    #[test]
    fn folders_include_the_ones_only_a_path_implies() {
        let members = vec![
            Member {
                name: "a/b/c.txt".to_string(),
                size: 1,
                is_dir: false,
            },
            Member {
                name: "top.txt".to_string(),
                size: 1,
                is_dir: false,
            },
        ];
        let folders = folders(&members);
        assert!(folders.contains(""));
        assert!(folders.contains("a"));
        assert!(folders.contains("a/b"));
        assert_eq!(folders.len(), 3);
        assert_eq!(files_in(&members, "").len(), 1);
        assert_eq!(files_in(&members, "a/b")[0].name, "a/b/c.txt");
        assert_eq!(folders_in(&folders, "a"), vec!["a/b".to_string()]);
    }

    #[test]
    fn unzip_listings_survive_spaces_in_names() {
        let text = "Archive:  x.zip\n  Length      Date    Time    Name\n\
                    ---------  ---------- -----   ----\n\
                    \x2017  08-04-2026 21:05   dir/my file.txt\n\
                    \x20 0  08-04-2026 21:05   map/\n\
                    ---------                     -------\n\
                    \x2017                     2 files\n";
        let members = parse_unzip(text);
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].name, "dir/my file.txt");
        assert_eq!(members[0].size, 17);
        assert!(members[1].is_dir);
    }
}
