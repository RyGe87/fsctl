//! What git already knows, asked in bulk.
//!
//! One `git status` per repository, never per file — that is the whole trick.
//! On this machine a repository answers in about a tenth of a second, so a
//! sweep over two dozen of them costs a couple of seconds and no daemon.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::time::Duration;

/// How long one repository may take before we stop waiting for it.
///
/// Measured on this machine: an ordinary repository answers in about a tenth of
/// a second, but an archived one with a large untracked tree took three and a
/// half minutes. A file manager may not freeze for that, so a repository that
/// dawdles is listed as unread rather than waited on.
const PATIENCE: Duration = Duration::from_millis(1500);

/// Runs a command and gives up after `limit`.
///
/// The output is drained on its own thread: git can write more than a pipe
/// holds, and a child blocked on a full pipe would never finish no matter how
/// long we waited.
fn output_within(mut command: Command, limit: Duration) -> Option<Vec<u8>> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        let _ = tx.send(buffer);
    });
    match rx.recv_timeout(limit) {
        Ok(buffer) => {
            let _ = child.wait();
            Some(buffer)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct Change {
    /// Git's two-letter porcelain code: " M", "??", "A ", …
    pub code: String,
    pub path: PathBuf,
}

impl Change {
    /// The word behind the code, for people who do not read porcelain.
    pub fn label(&self) -> &'static str {
        match self.code.trim() {
            "M" | "MM" | "AM" => "modified",
            "??" => "untracked",
            "A" => "added",
            "D" => "deleted",
            "R" => "renamed",
            "C" => "copied",
            "U" | "UU" | "AA" | "DD" => "conflict",
            _ => "modified",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub path: PathBuf,
    pub name: String,
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub changes: Vec<Change>,
    /// Set when git took longer than we were willing to wait. The repository
    /// still gets a row — knowing it is there matters more than its status.
    pub unread: bool,
    /// Mtimes of .git/index and .git/HEAD: unchanged means git has nothing new
    /// to say and we can skip the call entirely.
    stamp: (i64, i64),
}

impl Repo {
    pub fn summary(&self) -> String {
        if self.unread {
            return "too slow — not read".to_string();
        }
        let mut parts = vec![self.branch.clone()];
        if !self.changes.is_empty() {
            parts.push(format!("±{}", self.changes.len()));
        }
        if self.ahead > 0 {
            parts.push(format!("↑{}", self.ahead));
        }
        if self.behind > 0 {
            parts.push(format!("↓{}", self.behind));
        }
        parts.join("  ")
    }
}

fn stamp_of(repo: &Path) -> (i64, i64) {
    let at = |name: &str| {
        std::fs::metadata(repo.join(".git").join(name))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    };
    (at("index"), at("HEAD"))
}

/// Every repository under the given roots.
///
/// `find` does the walking: one process beats a recursive read_dir over the
/// hundreds of thousands of files that live under a development directory.
pub fn discover(roots: &[PathBuf], max_depth: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        let mut command = Command::new("/usr/bin/find");
        command
            .arg(root)
            .args(["-maxdepth", &max_depth.to_string()])
            .args(["-type", "d", "-name", ".git", "-prune", "-print"]);
        // A network mount that stopped answering must not take the tool with
        // it; ten seconds is far beyond the 0.03 s a local tree costs.
        let Some(bytes) = output_within(command, Duration::from_secs(10)) else {
            continue;
        };
        for line in String::from_utf8_lossy(&bytes).lines() {
            if let Some(parent) = Path::new(line).parent() {
                found.push(parent.to_path_buf());
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Asks one repository how it stands.
pub fn status(path: &Path) -> Option<Repo> {
    let unread = |path: &Path| Repo {
        name: leaf(path),
        path: path.to_path_buf(),
        branch: "—".to_string(),
        ahead: 0,
        behind: 0,
        changes: Vec::new(),
        unread: true,
        stamp: stamp_of(path),
    };

    let mut command = Command::new("/usr/bin/git");
    command
        .arg("-C")
        .arg(path)
        // --no-optional-locks: a file manager looking around should not write
        // to someone else's repository.
        .args(["--no-optional-locks", "status", "--porcelain", "--branch"]);
    let Some(bytes) = output_within(command, PATIENCE) else {
        return Some(unread(path));
    };
    let text = String::from_utf8_lossy(&bytes);
    if text.is_empty() {
        return Some(unread(path));
    }
    let mut branch = "—".to_string();
    let (mut ahead, mut behind) = (0, 0);
    let mut changes = Vec::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            branch = branch_of(rest);
            if let Some(track) = rest.split('[').nth(1) {
                ahead = number_after(track, "ahead ");
                behind = number_after(track, "behind ");
            }
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        let code = line[..2].to_string();
        // A rename reads "old -> new"; the new name is the one on disk.
        let name = line[3..].rsplit(" -> ").next().unwrap_or(&line[3..]);
        let name = name.trim_matches('"');
        changes.push(Change {
            code,
            path: path.join(name),
        });
    }

    Some(Repo {
        name: leaf(path),
        branch,
        ahead,
        behind,
        changes,
        unread: false,
        stamp: stamp_of(path),
        path: path.to_path_buf(),
    })
}

fn leaf(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// The branch out of git's header line, in all four shapes it comes in:
/// `main`, `main...origin/main [ahead 1]`, `No commits yet on main`, and the
/// detached `HEAD (no branch)`.
fn branch_of(header: &str) -> String {
    if header.starts_with("HEAD (no branch)") {
        return "detached HEAD".to_string();
    }
    let body = header
        .strip_prefix("No commits yet on ")
        .map(|rest| rest.trim())
        .unwrap_or(header);
    let head = body.split("...").next().unwrap_or(body);
    head.split(" [").next().unwrap_or(head).trim().to_string()
}

fn number_after(text: &str, key: &str) -> usize {
    text.split(key)
        .nth(1)
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
        })
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0)
}

/// Re-asks only the repositories that moved since last time.
pub fn refresh(repos: &mut Vec<Repo>) {
    for repo in repos.iter_mut() {
        if stamp_of(&repo.path) == repo.stamp {
            continue;
        }
        if let Some(fresh) = status(&repo.path) {
            *repo = fresh;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_of_branch_header() {
        assert_eq!(branch_of("main"), "main");
        assert_eq!(branch_of("main...origin/main"), "main");
        assert_eq!(
            branch_of("feature/x...origin/feature/x [ahead 1, behind 2]"),
            "feature/x"
        );
        // A repository that has never been committed to.
        assert_eq!(branch_of("No commits yet on main"), "main");
        assert_eq!(branch_of("HEAD (no branch)"), "detached HEAD");
    }

    #[test]
    fn ahead_and_behind_are_read_off_the_bracket() {
        let header = "main...origin/main [ahead 3, behind 12]";
        let track = header.split('[').nth(1).unwrap();
        assert_eq!(number_after(track, "ahead "), 3);
        assert_eq!(number_after(track, "behind "), 12);
        assert_eq!(number_after("nothing here", "ahead "), 0);
    }
}

/// The repository a path belongs to, if any — walking up, no git call needed.
pub fn repo_of(path: &Path, repos: &[Repo]) -> Option<usize> {
    repos
        .iter()
        .enumerate()
        .filter(|(_, r)| path.starts_with(&r.path))
        // The deepest match wins, for repositories inside repositories.
        .max_by_key(|(_, r)| r.path.components().count())
        .map(|(i, _)| i)
}
