//! fsctl — a two-pane file manager for the terminal.
//!
//! Left a source, right the items of whatever is selected in it. The folder
//! tree is only one of the sources: git knows which repositories exist and what
//! is unsaved in them, and that makes two more views over the same files
//! without a second way of drawing them.
//!
//! Nothing here reimplements what the system already does well. `cp` and `mv`
//! move the bytes, `git status` reports the repositories, `open` opens files,
//! and `stty` puts the terminal in raw mode.

mod archive;
mod editor;
mod fsmodel;
mod git;
mod html;
mod image;
mod markdown;
mod ops;
mod preview;
mod term;
mod toolbox;
mod widgets;
mod width;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use fsmodel::{Entry, Sort};
use git::Repo;
use ops::{Clipboard, Mode, Resolution};
use term::event::{self, Event, KeyCode, KeyEventKind};
use term::{Block, Color, Constraint, Layout, Modifier, Rect, Style};
use widgets::{List, Row};

const LEFT_WIDTH: u16 = 34;

/// How far a leap goes — what J K, D F and the ctrl-arrows jump at once.
///
/// Ten by default: far enough to be worth a chord, short enough to keep your
/// place on the screen. `FSCTL_LEAP` picks another ten; nonsense is ignored.
fn leap_size() -> isize {
    static SIZE: OnceLock<isize> = OnceLock::new();
    *SIZE.get_or_init(|| {
        std::env::var("FSCTL_LEAP")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| (1..=999).contains(n))
            .unwrap_or(10)
    })
}

/// What `--help` prints. The keys live behind `?` inside; out here are the
/// arguments and the handful of environment variables that steer the tool.
const USAGE: &str = "\
fsctl — a two-pane file manager for the terminal

Usage:
  fsctl [path]        open at path (default: the current directory)
  fsctl --doctor      what this machine can and cannot do, in one screen
  fsctl --version     print the version
  fsctl --help        print this text

Inside, ? shows the keys.

Environment:
  FSCTL_ROOTS         where to look for git repositories, colon-separated
                      like a PATH (default: ~/Development)
  FSCTL_CWD_FILE      a file to write the folder you leave in, so a shell
                      function can cd there afterwards (see the README)
  FSCTL_LEAP          how far a shift-leap jumps — J K, D F and the
                      ctrl-arrows, this many at once (default 10)
  FSCTL_TRASH=plain   delete without Finder, moving files to the trash by hand
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Folders,
    Repos,
    Modified,
}

impl Source {
    fn title(self) -> &'static str {
        match self {
            Source::Folders => " Folders ",
            Source::Repos => " Repos ",
            Source::Modified => " Unsaved ",
        }
    }
}

/// How much of the right column the listing keeps for itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    /// All of it. Looking and writing happen in a window over the top.
    Files,
    /// A strip at the bottom that follows the cursor, cheaply.
    Peek,
    /// Half and half, and then the strip does the full job: formatted, and
    /// written in place.
    Split,
}

impl Pane {
    fn next(self) -> Pane {
        match self {
            Pane::Files => Pane::Peek,
            Pane::Peek => Pane::Split,
            Pane::Split => Pane::Files,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Pane::Files => "files only",
            Pane::Peek => "a strip below",
            Pane::Split => "half and half",
        }
    }

    /// What the pane gets, out of the height the right column has.
    fn height(self, room: u16) -> u16 {
        match self {
            Pane::Files => 0,
            Pane::Peek if room >= 14 => (room / 3).clamp(5, 14),
            // A shade more than half: the file being read is what you came for.
            Pane::Split if room >= 12 => (room * 55 / 100).max(6),
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Left,
    Right,
}

/// A row in the left pane, whichever source built it.
struct Node {
    label: String,
    detail: String,
    path: PathBuf,
    depth: usize,
    expandable: bool,
    expanded: bool,
    /// Nothing inside at all — drawn with a cross instead of a triangle.
    empty: bool,
    /// Looks empty here, but holds hidden entries that a copy or move would
    /// carry along.
    hidden_only: bool,
}

/// Work that must happen after the next frame, so the screen can say what it is
/// busy with before it freezes for a second.
enum Pending {
    ScanRepos,
    /// Pull a file down from the cloud, then do what was asked with it.
    Fetch(FetchAsk),
    /// Zipping can take a while on a big folder; say so before it starts.
    Compress {
        items: Vec<PathBuf>,
        dest: PathBuf,
    },
    Refresh,
    /// Finder does the trashing, and a busy Finder should not look like a
    /// frozen tool: the screen says so first, then we wait.
    Trash(Vec<PathBuf>),
}

struct Conflict {
    dest: PathBuf,
    clashing: usize,
    total: usize,
}

/// What a delete is about to touch, gathered before anything is asked, so the
/// question can say what it costs.
struct DeleteAsk {
    items: Vec<PathBuf>,
    folders: usize,
    /// Folders that look empty here but are not: their content would go along
    /// unseen, which is the one thing a delete must say out loud.
    concealing: Vec<String>,
}

/// A look inside a file: the lines we read, and how far down we have walked.
struct Look {
    name: String,
    /// Where the file lives — a member read out of an archive lives nowhere,
    /// and so cannot be written.
    path: Option<PathBuf>,
    lines: Vec<markdown::Styled>,
    /// The file as it reads on disk, when a formatter or a renderer changed
    /// how it looks.
    raw: Option<Vec<markdown::Styled>>,
    showing_raw: bool,
    offset: usize,
    /// How far the window has slid sideways, in cells.
    column: usize,
    /// The widest line we hold, so sliding stops where the text does.
    widest: usize,
    /// Set when the file runs on past what we read, or when it is not text at
    /// all and this is the reason why.
    note: Option<String>,
    /// A picture gets no line numbers — there is nothing to number.
    picture: bool,
    /// The archive to step back into when this member is closed.
    back: Option<Inside>,
}

/// A file that is listed here but stored elsewhere, and what we were about to
/// do with it.
struct FetchAsk {
    path: PathBuf,
    name: String,
    size: u64,
    /// True when the plan was to hand it to another app rather than look at it.
    hand_over: bool,
}

/// Standing inside an archive — folders on the left, files on the right, the
/// same shape the tool has outside.
struct Inside {
    path: PathBuf,
    name: String,
    members: Vec<archive::Member>,
    folders: BTreeSet<String>,
    /// Which folders are unfolded, by their path inside the archive.
    opened: BTreeSet<String>,
    /// The folder rows as drawn: depth, path.
    rows: Vec<(usize, String)>,
    dir_cursor: usize,
    dir_offset: usize,
    file_cursor: usize,
    file_offset: usize,
    focus: Focus,
}

impl Inside {
    fn rebuild(&mut self) {
        let previous = self.here();
        self.rows.clear();
        self.push_folder("", 0);
        self.dir_cursor = self
            .rows
            .iter()
            .position(|(_, p)| *p == previous)
            .unwrap_or(0);
    }

    fn push_folder(&mut self, dir: &str, depth: usize) {
        self.rows.push((depth, dir.to_string()));
        if depth == 0 || self.opened.contains(dir) {
            for child in archive::folders_in(&self.folders, dir) {
                self.push_folder(&child, depth + 1);
            }
        }
    }

    fn here(&self) -> String {
        self.rows
            .get(self.dir_cursor)
            .map(|(_, p)| p.clone())
            .unwrap_or_default()
    }

    fn files(&self) -> Vec<&archive::Member> {
        archive::files_in(&self.members, &self.here())
    }

    fn selected_file(&self) -> Option<&archive::Member> {
        self.files().get(self.file_cursor).copied()
    }
}

/// Where should this go? The same tree, asked as a question.
struct Destination {
    root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    nodes: Vec<Node>,
    cursor: usize,
    offset: usize,
}

/// A name being typed over an old one.
struct Rename {
    path: PathBuf,
    text: String,
    cursor: usize,
}

/// What the pane under the listing is showing: for which file, at which size,
/// and the lines themselves — thrown away when any of those change.
struct Peeked {
    path: PathBuf,
    size: (u16, u16),
    lines: Vec<markdown::Styled>,
    note: Option<String>,
}

enum Modal {
    Edit(editor::Editor),
    Rename(Rename),
    Destination(Destination),
    Inside(Inside),
    Fetch(FetchAsk),
    Conflict(Conflict),
    Delete(DeleteAsk),
    Help,
    Look(Look),
}

struct App {
    source: Source,
    root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    nodes: Vec<Node>,
    left_cursor: usize,
    left_offset: usize,
    items: Vec<Entry>,
    right_cursor: usize,
    right_offset: usize,
    focus: Focus,
    sort: Sort,
    reverse: bool,
    show_hidden: bool,
    selection: BTreeSet<PathBuf>,
    clipboard: Option<Clipboard>,
    repos: Vec<Repo>,
    scanned: bool,
    status: String,
    modal: Option<Modal>,
    pending: Option<Pending>,
    left_height: usize,
    right_height: usize,
    /// The last frame's size, so opening a picture knows how big to make it.
    screen: (u16, u16),
    /// How the right column is divided.
    pane: Pane,
    /// What the pane is showing — recomputed when file or size changes.
    peeked: Option<Peeked>,
    quit: bool,
}

impl App {
    fn new(start: PathBuf) -> App {
        let root = start
            .ancestors()
            .find(|p| p.is_dir())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut app = App {
            source: Source::Folders,
            root: root.clone(),
            expanded: BTreeSet::from([root.clone()]),
            nodes: Vec::new(),
            left_cursor: 0,
            left_offset: 0,
            items: Vec::new(),
            right_cursor: 0,
            right_offset: 0,
            focus: Focus::Left,
            sort: Sort::Name,
            reverse: false,
            show_hidden: false,
            selection: BTreeSet::new(),
            clipboard: None,
            repos: Vec::new(),
            scanned: false,
            status: shorten(&root),
            modal: None,
            pending: None,
            left_height: 20,
            right_height: 20,
            screen: (80, 24),
            pane: Pane::Split,
            peeked: None,
            quit: false,
        };
        app.rebuild_left();
        app.rebuild_right();
        app
    }

    // ---------------------------------------------------------------- panes --

    fn rebuild_left(&mut self) {
        let previous = self.current_path();
        self.nodes = match self.source {
            Source::Folders => folder_nodes(&self.root, &self.expanded, self.show_hidden),
            Source::Repos => self
                .repos
                .iter()
                .map(|r| Node {
                    label: r.name.clone(),
                    detail: r.summary(),
                    path: r.path.clone(),
                    depth: 0,
                    expandable: false,
                    expanded: false,
                    empty: false,
                    hidden_only: false,
                })
                .collect(),
            Source::Modified => self
                .repos
                .iter()
                .filter(|r| !r.changes.is_empty())
                .map(|r| Node {
                    label: r.name.clone(),
                    detail: format!("{} changed", r.changes.len()),
                    path: r.path.clone(),
                    depth: 0,
                    expandable: false,
                    expanded: false,
                    empty: false,
                    hidden_only: false,
                })
                .collect(),
        };
        // Keep standing where we stood, if that row still exists.
        if let Some(previous) = previous
            && let Some(i) = self.nodes.iter().position(|n| n.path == previous)
        {
            self.left_cursor = i;
        }
        if self.left_cursor >= self.nodes.len() {
            self.left_cursor = self.nodes.len().saturating_sub(1);
        }
    }

    fn current_path(&self) -> Option<PathBuf> {
        self.nodes.get(self.left_cursor).map(|n| n.path.clone())
    }

    fn rebuild_right(&mut self) {
        let Some(path) = self.current_path() else {
            self.items.clear();
            return;
        };
        self.items = match self.source {
            Source::Modified => {
                let changes = self
                    .repos
                    .iter()
                    .find(|r| r.path == path)
                    .map(|r| r.changes.clone())
                    .unwrap_or_default();
                changes
                    .iter()
                    .map(|c| {
                        let mut entry = fsmodel::entry_for(&c.path).unwrap_or(Entry {
                            path: c.path.clone(),
                            name: c.path.display().to_string(),
                            is_dir: false,
                            is_link: false,
                            size: 0,
                            mtime: 0,
                            git: None,
                            dataless: false,
                        });
                        // Inside a repository the path from the root reads
                        // better than a bare file name.
                        if let Ok(rest) = c.path.strip_prefix(&path) {
                            entry.name = rest.display().to_string();
                        }
                        entry.git = Some(c.label().to_string());
                        entry
                    })
                    .collect()
            }
            // Folders belong on the left: the tree is the only place one lives,
            // so the right pane shows none.
            _ => {
                let mut items: Vec<Entry> = fsmodel::read_dir(&path, self.show_hidden)
                    .into_iter()
                    .filter(|e| !e.is_dir)
                    .collect();
                fsmodel::sort(&mut items, self.sort, self.reverse);
                self.attach_git(&path, &mut items);
                if fsmodel::is_cloud(&path) {
                    fsmodel::mark_dataless(&mut items);
                }
                items
            }
        };
        if self.right_cursor >= self.items.len() {
            self.right_cursor = self.items.len().saturating_sub(1);
        }
    }

    /// Hangs git's opinion on the rows when this directory sits in a repository
    /// we already know. No git call: the status is already in hand.
    fn attach_git(&self, dir: &Path, items: &mut [Entry]) {
        let Some(index) = git::repo_of(dir, &self.repos) else {
            return;
        };
        let repo = &self.repos[index];
        for item in items.iter_mut() {
            if let Some(change) = repo
                .changes
                .iter()
                .find(|c| c.path == item.path || item.path.starts_with(&c.path))
            {
                item.git = Some(change.label().to_string());
            }
        }
    }

    fn ensure_repos(&mut self) {
        if self.scanned {
            return;
        }
        let found = git::discover(&scan_roots(), 5);
        self.repos = found.iter().filter_map(|p| git::status(p)).collect();
        self.scanned = true;
        let slow = self.repos.iter().filter(|r| r.unread).count();
        self.status = match slow {
            0 => format!("{} repositories", self.repos.len()),
            n => format!("{} repositories · {n} too slow to read", self.repos.len()),
        };
    }

    // ----------------------------------------------------------------- keys --

    fn on_key(&mut self, key: term::event::KeyEvent) {
        // Ctrl-something is not the letter itself. Without this, ctrl-d asks
        // to delete and ctrl-c copies — and a terminal sends ctrl-d all by
        // itself when a pipe closes.
        if key.modifiers.contains(term::event::KeyModifiers::CONTROL)
            && !matches!(self.modal, Some(Modal::Edit(_)))
        {
            match key.code {
                // The one everyone's fingers know.
                KeyCode::Char('c') => self.leave(),
                // A leap. Only the arrows: ctrl-d is also what a closing
                // pipe sends, and a key that a machine can press by itself
                // has no business moving your cursor.
                KeyCode::Down => self.leap(leap_size()),
                KeyCode::Up => self.leap(-leap_size()),
                _ => {}
            }
            return;
        }
        if matches!(self.modal, Some(Modal::Edit(_))) {
            self.on_edit_key(key);
            return;
        }
        let code = key.code;
        if self.modal.is_some() {
            self.on_modal_key(code);
            return;
        }
        match code {
            KeyCode::Char('q') => self.leave(),
            KeyCode::Char('1') => self.switch(Source::Folders),
            KeyCode::Char('2') => self.switch(Source::Repos),
            KeyCode::Char('3') => self.switch(Source::Modified),
            KeyCode::Tab => {
                self.focus = if self.focus == Focus::Left {
                    Focus::Right
                } else {
                    Focus::Left
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            // Shift for a leap: ctrl-j is byte 0x0A, which is Enter itself and
            // cannot be told apart from it.
            KeyCode::Char('J') => self.leap(leap_size()),
            KeyCode::Char('K') => self.leap(-leap_size()),
            KeyCode::PageDown => self.move_cursor(self.page() as isize),
            KeyCode::PageUp => self.move_cursor(-(self.page() as isize)),
            KeyCode::Home | KeyCode::Char('g') => self.move_cursor(-1_000_000),
            KeyCode::End | KeyCode::Char('G') => self.move_cursor(1_000_000),
            KeyCode::Right | KeyCode::Char('l') => self.expand_or_focus(),
            KeyCode::Left | KeyCode::Char('h') => self.collapse_or_back(),
            KeyCode::Enter => self.enter(),
            KeyCode::Char(' ') => self.toggle_selection(),
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.status = format!("sorted by {}", self.sort.label());
                self.rebuild_right();
            }
            KeyCode::Char('S') => {
                self.reverse = !self.reverse;
                self.status = if self.reverse {
                    "reversed".into()
                } else {
                    "normal order".into()
                };
                self.rebuild_right();
            }
            KeyCode::Char('.') => {
                self.show_hidden = !self.show_hidden;
                self.status = if self.show_hidden {
                    "hidden files shown".into()
                } else {
                    "hidden files hidden".into()
                };
                self.rebuild_left();
                self.rebuild_right();
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('P') => {
                self.pane = self.pane.next();
                self.peeked = None;
                self.status = format!("layout: {}", self.pane.label());
            }
            KeyCode::Char('R') => self.ask_rename(),
            KeyCode::Char('e') => self.edit(),
            KeyCode::Char('p') => self.look(),
            KeyCode::Char('x') | KeyCode::Delete => self.delete(),
            KeyCode::Char('z') => self.compress(),
            KeyCode::Char('w') => self.root_here(),
            KeyCode::Char('W') => self.root_up(),
            KeyCode::Char('c') => self.yank(Mode::Copy),
            KeyCode::Char('m') => {
                self.yank(Mode::Cut);
                self.ask_destination();
            }
            KeyCode::Char('v') => self.paste(),
            KeyCode::Char('r') => {
                self.status = "refreshing…".into();
                self.pending = Some(Pending::Refresh);
            }
            KeyCode::Esc => {
                if self.selection.is_empty() {
                    self.clipboard = None;
                    self.status = "clipboard cleared".into();
                } else {
                    self.selection.clear();
                    self.status = "selection cleared".into();
                }
            }
            _ => {}
        }
    }

    fn on_modal_key(&mut self, code: KeyCode) {
        match self.modal {
            Some(Modal::Delete(_)) => self.on_delete_key(code),
            Some(Modal::Rename(_)) => self.on_rename_key(code),
            Some(Modal::Destination(_)) => self.on_destination_key(code),
            Some(Modal::Inside(_)) => self.on_inside_key(code),
            Some(Modal::Fetch(_)) => self.on_fetch_key(code),
            Some(Modal::Look(_)) => self.on_look_key(code),
            Some(Modal::Help) => {
                if matches!(
                    code,
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter
                ) {
                    self.modal = None;
                }
            }
            _ => self.on_conflict_key(code),
        }
    }

    fn on_conflict_key(&mut self, code: KeyCode) {
        let how = match code {
            KeyCode::Char('o') | KeyCode::Char('O') => Resolution::Overwrite,
            KeyCode::Char('b') | KeyCode::Char('B') => Resolution::KeepBoth,
            KeyCode::Char('s') | KeyCode::Char('S') => Resolution::Skip,
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = None;
                self.status = "cancelled".into();
                return;
            }
            _ => return,
        };
        let Some(Modal::Conflict(conflict)) = self.modal.take() else {
            return;
        };
        self.execute_paste(&conflict.dest, how);
    }

    fn on_look_key(&mut self, code: KeyCode) {
        let height = self.right_height.max(1);
        let Some(Modal::Look(look)) = &mut self.modal else {
            return;
        };
        let last = look.lines.len().saturating_sub(1);
        match code {
            KeyCode::Down | KeyCode::Char('j') => look.offset = (look.offset + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => look.offset = look.offset.saturating_sub(1),
            KeyCode::PageDown => look.offset = (look.offset + height).min(last),
            KeyCode::PageUp => look.offset = look.offset.saturating_sub(height),
            KeyCode::Char('J') => look.offset = (look.offset + leap_size() as usize).min(last),
            KeyCode::Char('K') => look.offset = look.offset.saturating_sub(leap_size() as usize),
            KeyCode::Home | KeyCode::Char('g') => look.offset = 0,
            KeyCode::End | KeyCode::Char('G') => look.offset = last,
            // Sideways in steps of eight: one column at a time turns reading a
            // wide line into a chore.
            KeyCode::Right | KeyCode::Char('f') | KeyCode::Char('l') => {
                look.column = (look.column + 8).min(look.widest.saturating_sub(8))
            }
            KeyCode::Left | KeyCode::Char('d') | KeyCode::Char('h') => {
                look.column = look.column.saturating_sub(8)
            }
            // The shift of the same keys leaps sideways: the usual steps of
            // eight, a leap of them at once.
            KeyCode::Char('F') | KeyCode::Char('L') => {
                look.column =
                    (look.column + 8 * leap_size() as usize).min(look.widest.saturating_sub(8))
            }
            KeyCode::Char('D') | KeyCode::Char('H') => {
                look.column = look.column.saturating_sub(8 * leap_size() as usize)
            }
            KeyCode::Char('0') => look.column = 0,
            // Back and forth between what the formatter made of it and what
            // is actually in the file.
            KeyCode::Char('t') => {
                if let Some(other) = look.raw.take() {
                    look.raw = Some(std::mem::replace(&mut look.lines, other));
                    look.showing_raw = !look.showing_raw;
                    look.offset = 0;
                    look.column = 0;
                    look.widest = widest_of(&look.lines);
                }
            }
            // From looking straight into writing — same file, no detour past
            // the listing.
            KeyCode::Char('e') => self.edit_looked(),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('p') | KeyCode::Enter => {
                let back = match &mut self.modal {
                    Some(Modal::Look(look)) => look.back.take(),
                    _ => None,
                };
                self.modal = back.map(Modal::Inside);
            }
            _ => {}
        }
    }

    /// From looking to writing without the detour past the listing: the same
    /// file, opened in the writer. A member shown out of an archive has no
    /// file to write to, and says so.
    fn edit_looked(&mut self) {
        let (path, name, picture) = match &self.modal {
            Some(Modal::Look(look)) => (look.path.clone(), look.name.clone(), look.picture),
            _ => return,
        };
        if picture {
            self.status = "a picture is not text".into();
            return;
        }
        let Some(path) = path else {
            self.status = "this lives in the archive — u unpacks it first".into();
            return;
        };
        match editor::Editor::open(&path, &name) {
            Ok(buffer) => self.modal = Some(Modal::Edit(buffer)),
            Err(reason) => self.status = reason,
        }
    }

    /// Opens the file under the cursor, in a box, without leaving the tool.
    fn look(&mut self) {
        let Some(item) = self.items.get(self.right_cursor) else {
            self.status = "nothing to look into".into();
            return;
        };
        let (path, name, dataless, size) = (
            item.path.clone(),
            item.name.clone(),
            item.dataless,
            item.size,
        );
        // Reading a file that is only listed here pulls it down first, and that
        // is a decision, not a keystroke.
        if dataless {
            self.modal = Some(Modal::Fetch(FetchAsk {
                path,
                name,
                size,
                hand_over: false,
            }));
            return;
        }
        if archive::is_archive(&path) {
            self.open_archive(path, name);
            return;
        }
        self.preview_of(path, name);
    }

    fn preview_of(&mut self, path: PathBuf, name: String) {
        let (w, h) = self.screen;
        let built = build_preview(
            &path,
            w.saturating_sub(8),
            h.saturating_sub(7),
            Shown::Window,
        );
        let widest = widest_of(&built.lines);
        self.modal = Some(Modal::Look(Look {
            name,
            path: Some(path),
            lines: built.lines,
            raw: built.raw,
            showing_raw: false,
            offset: 0,
            column: 0,
            widest,
            note: built.note,
            picture: built.picture,
            back: None,
        }));
    }

    fn open_archive(&mut self, path: PathBuf, name: String) {
        match archive::list(&path) {
            Ok(members) if !members.is_empty() => {
                let folders = archive::folders(&members);
                let mut inside = Inside {
                    path,
                    name,
                    members,
                    folders,
                    opened: BTreeSet::new(),
                    rows: Vec::new(),
                    dir_cursor: 0,
                    dir_offset: 0,
                    file_cursor: 0,
                    file_offset: 0,
                    focus: Focus::Right,
                };
                inside.rebuild();
                self.modal = Some(Modal::Inside(inside));
            }
            Ok(_) => self.status = "the archive is empty".into(),
            Err(e) => self.status = format!("archive: {e}"),
        }
    }

    fn on_inside_key(&mut self, code: KeyCode) {
        let height = self.screen.1.saturating_sub(8).max(1) as usize;
        let Some(Modal::Inside(inside)) = &mut self.modal else {
            return;
        };
        let (last, cursor) = match inside.focus {
            Focus::Left => (inside.rows.len().saturating_sub(1), inside.dir_cursor),
            Focus::Right => (inside.files().len().saturating_sub(1), inside.file_cursor),
        };
        let moved = match code {
            KeyCode::Down | KeyCode::Char('j') => (cursor + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => cursor.saturating_sub(1),
            KeyCode::Char('J') => (cursor + leap_size() as usize).min(last),
            KeyCode::Char('K') => cursor.saturating_sub(leap_size() as usize),
            KeyCode::PageDown => (cursor + height).min(last),
            KeyCode::PageUp => cursor.saturating_sub(height),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => last,
            KeyCode::Tab => {
                inside.focus = if inside.focus == Focus::Left {
                    Focus::Right
                } else {
                    Focus::Left
                };
                return;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if inside.focus == Focus::Left {
                    let here = inside.here();
                    // A folder with nothing under it hands the keyboard on.
                    if !here.is_empty() && !archive::folders_in(&inside.folders, &here).is_empty() {
                        inside.opened.insert(here);
                        inside.rebuild();
                    } else {
                        inside.focus = Focus::Right;
                    }
                }
                return;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if inside.focus == Focus::Right {
                    inside.focus = Focus::Left;
                } else {
                    let here = inside.here();
                    if inside.opened.remove(&here) {
                        inside.rebuild();
                    } else {
                        let parent = archive::parent_of(&here);
                        if let Some(at) = inside.rows.iter().position(|(_, p)| *p == parent) {
                            inside.dir_cursor = at;
                            inside.file_cursor = 0;
                        }
                    }
                }
                return;
            }
            KeyCode::Enter | KeyCode::Char('p') => {
                if inside.focus == Focus::Left {
                    inside.focus = Focus::Right;
                    return;
                }
                self.look_at_member();
                return;
            }
            KeyCode::Char('u') => {
                self.take_member_out();
                return;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = None;
                return;
            }
            _ => return,
        };
        match inside.focus {
            Focus::Left if moved != inside.dir_cursor => {
                inside.dir_cursor = moved;
                // A new folder means the file list starts over.
                inside.file_cursor = 0;
                inside.file_offset = 0;
            }
            Focus::Left => {}
            Focus::Right => inside.file_cursor = moved,
        }
    }

    /// Reads one member straight out of the archive — no unpacking, nothing
    /// left behind.
    fn look_at_member(&mut self) {
        let Some(Modal::Inside(inside)) = &self.modal else {
            return;
        };
        let Some(member) = inside.selected_file() else {
            return;
        };
        let (name, size) = (member.name.clone(), member.size);
        let bytes = archive::read_member(&inside.path, &name);
        let back = match self.modal.take() {
            Some(Modal::Inside(inside)) => Some(inside),
            _ => None,
        };
        let (lines, note) = match bytes {
            Ok(bytes) => match preview::from_bytes(&bytes, size) {
                preview::Preview::Text { lines, note, .. } => {
                    let rendered = if name.to_lowercase().ends_with(".md") {
                        markdown::render(&lines)
                    } else {
                        as_styled(lines)
                    };
                    (rendered, note)
                }
                preview::Preview::NotText(reason) => (Vec::new(), Some(reason)),
            },
            Err(e) => (Vec::new(), Some(e)),
        };
        let widest = widest_of(&lines);
        self.modal = Some(Modal::Look(Look {
            name,
            path: None,
            lines,
            raw: None,
            showing_raw: false,
            offset: 0,
            column: 0,
            widest,
            note,
            picture: false,
            back,
        }));
    }

    /// Out of the archive and into the folder you are standing in — a real
    /// file, in a place that will still be there tomorrow.
    fn take_member_out(&mut self) {
        let Some(Modal::Inside(inside)) = &self.modal else {
            return;
        };
        let Some(member) = inside.selected_file() else {
            return;
        };
        let Some(dest) = self.current_path() else {
            return;
        };
        let (archive_path, name) = (inside.path.clone(), member.name.clone());
        self.status = match archive::extract(&archive_path, &name, &dest) {
            Ok(()) => format!("{name} extracted into {}", shorten(&dest)),
            Err(e) => format!("extracting failed: {e}"),
        };
        self.modal = None;
        self.rebuild_right();
    }

    fn on_fetch_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter | KeyCode::Char('p') => {
                let Some(Modal::Fetch(ask)) = self.modal.take() else {
                    return;
                };
                self.status = format!("fetching {}…", ask.name);
                self.pending = Some(Pending::Fetch(ask));
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = None;
                self.status = "nothing fetched".into();
            }
            _ => {}
        }
    }

    fn on_delete_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('x') | KeyCode::Char('X') => {
                let Some(Modal::Delete(ask)) = self.modal.take() else {
                    return;
                };
                self.status = "deleting…".into();
                self.pending = Some(Pending::Trash(ask.items));
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = None;
                self.status = "nothing deleted".into();
            }
            _ => {}
        }
    }

    fn switch(&mut self, source: Source) {
        self.source = source;
        self.left_cursor = 0;
        self.left_offset = 0;
        self.right_cursor = 0;
        self.right_offset = 0;
        if source != Source::Folders && !self.scanned {
            self.status = "finding repositories…".into();
            self.pending = Some(Pending::ScanRepos);
            return;
        }
        self.rebuild_left();
        self.rebuild_right();
    }

    /// Ten rows at a time, in whatever is in front of you.
    fn leap(&mut self, delta: isize) {
        if let Some(Modal::Look(look)) = &mut self.modal {
            let last = look.lines.len().saturating_sub(1);
            look.offset = if delta < 0 {
                look.offset.saturating_sub(delta.unsigned_abs())
            } else {
                (look.offset + delta as usize).min(last)
            };
            return;
        }
        if self.modal.is_none() {
            self.move_cursor(delta);
        }
    }

    fn page(&self) -> usize {
        match self.focus {
            Focus::Left => self.left_height.max(1),
            Focus::Right => self.right_height.max(1),
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let (cursor, len) = match self.focus {
            Focus::Left => (self.left_cursor, self.nodes.len()),
            Focus::Right => (self.right_cursor, self.items.len()),
        };
        if len == 0 {
            return;
        }
        let next = (cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        match self.focus {
            Focus::Left => {
                if next != self.left_cursor {
                    self.left_cursor = next;
                    self.right_cursor = 0;
                    self.right_offset = 0;
                    self.rebuild_right();
                }
            }
            Focus::Right => self.right_cursor = next,
        }
    }

    fn expand_or_focus(&mut self) {
        if self.focus == Focus::Right {
            return;
        }
        match self.nodes.get(self.left_cursor) {
            Some(node) if node.expandable && !node.expanded => {
                let path = node.path.clone();
                self.expanded.insert(path);
                self.rebuild_left();
            }
            _ => self.focus = Focus::Right,
        }
    }

    fn collapse_or_back(&mut self) {
        if self.focus == Focus::Right {
            self.focus = Focus::Left;
            return;
        }
        let Some(node) = self.nodes.get(self.left_cursor) else {
            return;
        };
        if node.expanded {
            let path = node.path.clone();
            self.expanded.remove(&path);
            self.rebuild_left();
        } else if let Some(parent) = node.path.parent().map(|p| p.to_path_buf())
            && let Some(i) = self.nodes.iter().position(|n| n.path == parent)
        {
            self.left_cursor = i;
            self.rebuild_right();
        }
    }

    /// Enter walks in: a directory becomes the selected node on the left, a file
    /// goes to whatever macOS opens it with.
    fn enter(&mut self) {
        if self.focus == Focus::Left {
            self.focus = Focus::Right;
            return;
        }
        let Some(item) = self.items.get(self.right_cursor) else {
            return;
        };
        if !item.is_dir {
            let path = item.path.clone();
            let name = item.name.clone();
            if item.dataless {
                let size = item.size;
                self.modal = Some(Modal::Fetch(FetchAsk {
                    path,
                    name,
                    size,
                    hand_over: true,
                }));
                return;
            }
            self.status = match ops::open(&path) {
                Ok(()) => format!("opened: {name}"),
                Err(e) => format!("could not open: {e}"),
            };
            return;
        }
        let target = item.path.clone();
        // Walking into a directory is a folder-tree act; switch views and keep
        // the destination.
        self.source = Source::Folders;
        if !target.starts_with(&self.root) {
            self.root = target.clone();
        }
        for ancestor in target.ancestors() {
            if ancestor.starts_with(&self.root) {
                self.expanded.insert(ancestor.to_path_buf());
            }
        }
        self.rebuild_left();
        if let Some(i) = self.nodes.iter().position(|n| n.path == target) {
            self.left_cursor = i;
        }
        self.right_cursor = 0;
        self.right_offset = 0;
        self.rebuild_right();
    }

    /// Makes the folder under the cursor the top of the tree.
    ///
    /// Works from any source: pick a repository in the second view, press the
    /// key, and the tree opens rooted there.
    fn root_here(&mut self) {
        let Some(path) = self.current_path() else {
            return;
        };
        if !path.is_dir() {
            return;
        }
        self.source = Source::Folders;
        self.root = path.clone();
        self.expanded = BTreeSet::from([path.clone()]);
        self.left_cursor = 0;
        self.left_offset = 0;
        self.right_cursor = 0;
        self.right_offset = 0;
        self.focus = Focus::Left;
        self.status = format!("root: {}", shorten(&path));
        self.rebuild_left();
        self.rebuild_right();
    }

    /// Lifts the tree one level, keeping the old root open and under the
    /// cursor so you can see where you came from.
    fn root_up(&mut self) {
        let Some(parent) = self.root.parent().map(|p| p.to_path_buf()) else {
            self.status = "this is as far up as it goes".into();
            return;
        };
        let previous = self.root.clone();
        self.source = Source::Folders;
        self.root = parent.clone();
        self.expanded.insert(parent.clone());
        self.expanded.insert(previous.clone());
        self.rebuild_left();
        if let Some(i) = self.nodes.iter().position(|n| n.path == previous) {
            self.left_cursor = i;
        }
        self.focus = Focus::Left;
        self.status = format!("root: {}", shorten(&parent));
        self.rebuild_right();
    }

    fn toggle_selection(&mut self) {
        if self.focus != Focus::Right {
            return;
        }
        let Some(item) = self.items.get(self.right_cursor) else {
            return;
        };
        let path = item.path.clone();
        if !self.selection.remove(&path) {
            self.selection.insert(path);
        }
        // Ticking walks on, the way ticking always does.
        if self.right_cursor + 1 < self.items.len() {
            self.right_cursor += 1;
        }
    }

    /// What a copy or cut acts on: everything ticked, or else the row you are
    /// standing on — and in the tree that row is a folder, which is how whole
    /// folders still get copied now that they no longer sit on the right.
    fn targets(&self) -> Vec<PathBuf> {
        if self.focus == Focus::Left {
            return self.current_path().into_iter().collect();
        }
        if !self.selection.is_empty() {
            return self.selection.iter().cloned().collect();
        }
        self.items
            .get(self.right_cursor)
            .map(|i| vec![i.path.clone()])
            .unwrap_or_default()
    }

    fn yank(&mut self, mode: Mode) {
        let items = self.targets();
        if items.is_empty() {
            self.status = "nothing to take".into();
            return;
        }
        let verb = match mode {
            Mode::Copy => "copy",
            Mode::Cut => "move",
        };
        self.status = if self.focus == Focus::Left {
            let name = items
                .first()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            format!("folder {name} ready to {verb}")
        } else {
            format!("{} file(s) ready to {verb}", items.len())
        };
        self.clipboard = Some(Clipboard { items, mode });
    }

    /// Asks first, and gathers what it needs to ask well.
    fn delete(&mut self) {
        let items = self.targets();
        if items.is_empty() {
            self.status = "nothing to delete".into();
            return;
        }
        // Refuse to delete the ground you are standing on: the tree would be
        // rooted at something that no longer exists.
        if items.contains(&self.root) {
            self.status = "the root of the tree cannot go from here".into();
            return;
        }
        let mut folders = 0;
        let mut concealing = Vec::new();
        for item in &items {
            if !item.is_dir() {
                continue;
            }
            folders += 1;
            let probe = fsmodel::probe(item, self.show_hidden);
            if probe.hidden_only {
                concealing.push(
                    item.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                );
            }
        }
        self.modal = Some(Modal::Delete(DeleteAsk {
            items,
            folders,
            concealing,
        }));
    }

    /// Opens the file under the cursor for writing.
    fn edit(&mut self) {
        let Some(item) = self.items.get(self.right_cursor) else {
            self.status = "nothing to edit".into();
            return;
        };
        if item.dataless {
            self.status = "fetch it first — p asks".into();
            return;
        }
        match editor::Editor::open(&item.path, &item.name) {
            Ok(buffer) => self.modal = Some(Modal::Edit(buffer)),
            Err(reason) => self.status = reason,
        }
    }

    fn on_edit_key(&mut self, key: term::event::KeyEvent) {
        let page = self.screen.1.saturating_sub(8).max(1) as usize;
        let Some(Modal::Edit(buffer)) = &mut self.modal else {
            return;
        };
        // Ctrl-s saves wherever you are; the rest of ctrl is not ours.
        if key.modifiers.contains(term::event::KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('s') {
                buffer.note = match buffer.save() {
                    Ok(()) => Some("saved".to_string()),
                    Err(e) => Some(format!("could not save: {e}")),
                };
            }
            return;
        }
        // The question after Esc with unsaved changes.
        if buffer.asking {
            match key.code {
                KeyCode::Char('s') => {
                    let saved = buffer.save();
                    match saved {
                        Ok(()) => {
                            self.modal = None;
                            self.status = "saved".into();
                            self.peeked = None;
                            self.rebuild_right();
                        }
                        Err(e) => {
                            buffer.asking = false;
                            buffer.note = Some(format!("could not save: {e}"));
                        }
                    }
                }
                KeyCode::Char('d') => {
                    self.modal = None;
                    self.status = "changes thrown away".into();
                }
                KeyCode::Esc => buffer.asking = false,
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char(c) => buffer.insert(c),
            KeyCode::Enter => buffer.split_line(),
            KeyCode::Backspace => buffer.delete_before(),
            KeyCode::Delete => buffer.delete_at(),
            KeyCode::Up => buffer.up(),
            KeyCode::Down => buffer.down(),
            KeyCode::Left => buffer.left(),
            KeyCode::Right => buffer.right(),
            KeyCode::Home => buffer.col = 0,
            KeyCode::End => buffer.col = buffer.chars_in_line(),
            KeyCode::PageUp => {
                let row = buffer.row.saturating_sub(page) as isize;
                buffer.move_to(row, buffer.col as isize)
            }
            KeyCode::PageDown => {
                let row = (buffer.row + page) as isize;
                buffer.move_to(row, buffer.col as isize)
            }
            KeyCode::Esc => {
                if buffer.dirty {
                    buffer.asking = true;
                } else {
                    self.modal = None;
                }
            }
            _ => {}
        }
    }

    /// A new name for the row you are on, offered as the old one.
    fn ask_rename(&mut self) {
        let (path, name) = match self.focus {
            Focus::Right => match self.items.get(self.right_cursor) {
                Some(item) => (item.path.clone(), item.name.clone()),
                None => return,
            },
            Focus::Left => match self.current_path() {
                Some(path) if Some(&path) != Some(&self.root) => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    (path, name)
                }
                _ => {
                    self.status = "the root of the tree keeps its name".into();
                    return;
                }
            },
        };
        // The cursor sits at the end of the stem, so a typo in the name is one
        // keystroke away and the extension is not in the way.
        let at = name.rfind('.').filter(|i| *i > 0).unwrap_or(name.len());
        self.modal = Some(Modal::Rename(Rename {
            path,
            text: name,
            cursor: at,
        }));
    }

    fn on_rename_key(&mut self, code: KeyCode) {
        let Some(Modal::Rename(rename)) = &mut self.modal else {
            return;
        };
        match code {
            KeyCode::Char(c) => {
                rename.text.insert(rename.cursor, c);
                rename.cursor += c.len_utf8();
            }
            KeyCode::Backspace => {
                if let Some((at, c)) = rename.text[..rename.cursor].char_indices().next_back() {
                    rename.text.remove(at);
                    rename.cursor = at.min(rename.text.len());
                    let _ = c;
                }
            }
            KeyCode::Delete => {
                if rename.cursor < rename.text.len() {
                    rename.text.remove(rename.cursor);
                }
            }
            KeyCode::Left => {
                rename.cursor = rename.text[..rename.cursor]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            }
            KeyCode::Right => {
                rename.cursor = rename.text[rename.cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(i, _)| rename.cursor + i)
                    .unwrap_or(rename.text.len())
            }
            KeyCode::Home => rename.cursor = 0,
            KeyCode::End => rename.cursor = rename.text.len(),
            KeyCode::Enter => {
                let (path, text) = (rename.path.clone(), rename.text.trim().to_string());
                self.modal = None;
                self.do_rename(&path, &text);
            }
            KeyCode::Esc => {
                self.modal = None;
                self.status = "name kept".into();
            }
            _ => {}
        }
    }

    fn do_rename(&mut self, path: &Path, name: &str) {
        let old = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.is_empty() || name == old {
            self.status = "name kept".into();
            return;
        }
        // A slash would move the file somewhere else, which is what m is for.
        if name.contains('/') {
            self.status = "a name cannot hold a slash".into();
            return;
        }
        let Some(parent) = path.parent() else {
            return;
        };
        let target = parent.join(name);
        if target.symlink_metadata().is_ok() {
            self.status = format!("{name} already exists");
            return;
        }
        self.status = match std::fs::rename(path, &target) {
            Ok(()) => format!("{old} → {name}"),
            Err(e) => format!("could not rename: {e}"),
        };
        // The tree may have been holding the old name open.
        if self.expanded.remove(path) {
            self.expanded.insert(target.clone());
        }
        if self.root == path {
            self.root = target;
        }
        self.rebuild_left();
        self.rebuild_right();
    }

    /// Opens the tree as a question: where does this go?
    fn ask_destination(&mut self) {
        if self.clipboard.is_none() {
            return;
        }
        let expanded = self.expanded.clone();
        let nodes = folder_nodes(&self.root, &expanded, self.show_hidden);
        let here = self.current_path();
        let cursor = here
            .and_then(|p| nodes.iter().position(|n| n.path == p))
            .unwrap_or(0);
        self.modal = Some(Modal::Destination(Destination {
            root: self.root.clone(),
            expanded,
            nodes,
            cursor,
            offset: 0,
        }));
    }

    fn on_destination_key(&mut self, code: KeyCode) {
        let height = self.screen.1.saturating_sub(7).max(1) as usize;
        let show_hidden = self.show_hidden;
        let Some(Modal::Destination(pick)) = &mut self.modal else {
            return;
        };
        let last = pick.nodes.len().saturating_sub(1);
        match code {
            KeyCode::Down | KeyCode::Char('j') => pick.cursor = (pick.cursor + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => pick.cursor = pick.cursor.saturating_sub(1),
            KeyCode::Char('J') => pick.cursor = (pick.cursor + leap_size() as usize).min(last),
            KeyCode::Char('K') => pick.cursor = pick.cursor.saturating_sub(leap_size() as usize),
            KeyCode::PageDown => pick.cursor = (pick.cursor + height).min(last),
            KeyCode::PageUp => pick.cursor = pick.cursor.saturating_sub(height),
            KeyCode::Home | KeyCode::Char('g') => pick.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => pick.cursor = last,
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                if let Some(node) = pick.nodes.get(pick.cursor)
                    && node.expandable
                    && !node.expanded
                {
                    pick.expanded.insert(node.path.clone());
                    let at = pick.nodes[pick.cursor].path.clone();
                    pick.nodes = folder_nodes(&pick.root, &pick.expanded, show_hidden);
                    pick.cursor = pick.nodes.iter().position(|n| n.path == at).unwrap_or(0);
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(node) = pick.nodes.get(pick.cursor) {
                    let at = if node.expanded {
                        pick.expanded.remove(&node.path);
                        node.path.clone()
                    } else {
                        node.path.parent().unwrap_or(&node.path).to_path_buf()
                    };
                    pick.nodes = folder_nodes(&pick.root, &pick.expanded, show_hidden);
                    pick.cursor = pick.nodes.iter().position(|n| n.path == at).unwrap_or(0);
                }
            }
            KeyCode::Char('v') => {
                let Some(target) = pick.nodes.get(pick.cursor).map(|n| n.path.clone()) else {
                    return;
                };
                self.modal = None;
                self.paste_into(target);
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = None;
                // The clipboard stays filled: you can always walk there
                // yourself and press v.
                self.status = "pick a folder and press v".into();
            }
            _ => {}
        }
    }

    /// Packs what you picked into a zip, here.
    fn compress(&mut self) {
        let items = self.targets();
        if items.is_empty() {
            self.status = "nothing to pack".into();
            return;
        }
        let Some(mut dest) = self.current_path() else {
            return;
        };
        // Zipping the folder you are standing in puts the archive beside it,
        // not inside it — an archive that contains itself is a riddle.
        if items.contains(&dest)
            && let Some(parent) = dest.parent()
        {
            dest = parent.to_path_buf();
        }
        self.status = format!("packing {} item(s)…", items.len());
        self.pending = Some(Pending::Compress { items, dest });
    }

    fn paste(&mut self) {
        let Some(dest) = self.current_path() else {
            return;
        };
        self.paste_into(dest);
    }

    fn paste_into(&mut self, dest: PathBuf) {
        let Some(clip) = self.clipboard.clone() else {
            self.status = "the clipboard is empty".into();
            return;
        };
        let clashing = ops::conflicts(&clip.items, &dest).len();
        if clashing == 0 {
            self.execute_paste(&dest, Resolution::Overwrite);
            return;
        }
        self.modal = Some(Modal::Conflict(Conflict {
            dest,
            clashing,
            total: clip.items.len(),
        }));
    }

    fn execute_paste(&mut self, dest: &Path, how: Resolution) {
        let Some(clip) = self.clipboard.clone() else {
            return;
        };
        let outcome = ops::paste(&clip, dest, how);
        self.status = outcome.summary(clip.mode);
        if clip.mode == Mode::Cut && outcome.errors.is_empty() {
            self.clipboard = None;
        }
        self.selection.clear();
        self.rebuild_left();
        self.rebuild_right();
    }

    /// Leaving writes the current directory where a shell function can read it,
    /// so `q` can put you where you were looking.
    fn leave(&mut self) {
        if let (Ok(target), Some(path)) = (std::env::var("FSCTL_CWD_FILE"), self.current_path()) {
            let _ = std::fs::write(target, path.display().to_string());
        }
        self.quit = true;
    }
}

/// The folder tree, from a root and a set of opened folders.
///
/// Shared by the pane on the left and by the picker that asks where something
/// should go: two views of one shape, so choosing a destination feels like
/// walking the tree you already know.
fn folder_nodes(root: &Path, expanded: &BTreeSet<PathBuf>, show_hidden: bool) -> Vec<Node> {
    let probe = fsmodel::probe(root, show_hidden);
    let mut nodes = vec![Node {
        label: root_label(root),
        detail: String::new(),
        path: root.to_path_buf(),
        depth: 0,
        expandable: probe.has_subdir,
        expanded: expanded.contains(root),
        empty: probe.empty,
        hidden_only: probe.hidden_only,
    }];
    if expanded.contains(root) {
        push_children(root, 1, expanded, show_hidden, &mut nodes);
    }
    nodes
}

fn push_children(
    dir: &Path,
    depth: usize,
    expanded: &BTreeSet<PathBuf>,
    show_hidden: bool,
    out: &mut Vec<Node>,
) {
    // Deep trees are legal but unreadable; the right pane is where you go
    // deeper.
    if depth > 12 {
        return;
    }
    for child in fsmodel::subdirectories(dir, show_hidden) {
        let open = expanded.contains(&child.path);
        // A folder with nothing to unfold gets no triangle: the mark should
        // promise something.
        let probe = fsmodel::probe(&child.path, show_hidden);
        out.push(Node {
            label: child.name.clone(),
            detail: String::new(),
            path: child.path.clone(),
            depth,
            expandable: probe.has_subdir,
            expanded: open,
            empty: probe.empty,
            hidden_only: probe.hidden_only,
        });
        if open {
            push_children(&child.path, depth + 1, expanded, show_hidden, out);
        }
    }
}

/// A file made ready to look at: formatted where a tool knows how, rendered
/// where markdown asks for it, drawn as blocks where it is a picture.
///
/// The same work for the window and for the pane at the bottom, so the two can
/// never disagree about what a file looks like.
/// Who is asking for the file: the pane, where you also write, or the window,
/// where you only read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Pane,
    Window,
}

struct Built {
    lines: Vec<markdown::Styled>,
    raw: Option<Vec<markdown::Styled>>,
    note: Option<String>,
    picture: bool,
}

fn build_preview(path: &Path, cols: u16, rows: u16, shown: Shown) -> Built {
    // An archive is not text, but it is not opaque either: the pane shows what
    // is in it, the same listing `p` walks through. Reading the central
    // directory costs no unpacking.
    if archive::is_archive(path) {
        return match archive::list(path) {
            Ok(members) if !members.is_empty() => {
                let room = cols as usize;
                let visible = (rows as usize).saturating_sub(1).max(1);
                // Indented by depth: a listing of full paths reads as noise,
                // and the shape of an archive is the first thing you want.
                let mut sorted: Vec<&archive::Member> = members.iter().collect();
                sorted.sort_by(|a, b| a.name.cmp(&b.name));
                let mut lines: Vec<markdown::Styled> = sorted
                    .iter()
                    .take(visible)
                    .map(|m| {
                        let depth = m.name.trim_end_matches('/').matches('/').count();
                        let style = if m.is_dir {
                            Style::new().add_modifier(Modifier::BOLD)
                        } else {
                            Style::new()
                        };
                        let label = format!(
                            "{}{}{}",
                            "  ".repeat(depth),
                            archive::leaf_of(&m.name),
                            if m.is_dir { "/" } else { "" }
                        );
                        vec![
                            (width::fit(&label, room.saturating_sub(10)), style),
                            (
                                width::fit_right(&fsmodel::human_size(m.size, m.is_dir), 9),
                                Style::new().fg(Color::DarkGray),
                            ),
                        ]
                    })
                    .collect();
                if members.len() > visible {
                    lines.push(
                        as_styled(vec![format!("… and {} more", members.len() - visible)])
                            .remove(0),
                    );
                }
                Built {
                    lines,
                    raw: None,
                    note: Some(format!("{} items · p to walk through it", members.len())),
                    picture: false,
                }
            }
            Ok(_) => Built {
                lines: Vec::new(),
                raw: None,
                note: Some("the archive is empty".to_string()),
                picture: false,
            },
            Err(reason) => Built {
                lines: Vec::new(),
                raw: None,
                note: Some(reason),
                picture: false,
            },
        };
    }
    if image::is_image(path) {
        let (lines, note) = match image::thumbnail(path, cols as usize, rows as usize) {
            Ok((lines, note)) => (lines, note),
            Err(reason) => (Vec::new(), reason),
        };
        return Built {
            lines,
            raw: None,
            note: Some(note),
            picture: true,
        };
    }

    // A page reads as a page in the window and as its source in the pane: the
    // pane is where e writes, and you cannot write in a rendering.
    if html::is_html(path) && shown == Shown::Pane {
        let source = std::fs::read_to_string(path).unwrap_or_default();
        return Built {
            lines: as_styled(source.lines().map(|l| l.to_string()).collect()),
            raw: None,
            note: Some("source · p renders it".to_string()),
            picture: false,
        };
    }
    // A page is read by whoever already knows how; we only mark the headings.
    if html::is_html(path) {
        return match html::render(path) {
            Ok((lines, source, tool)) => Built {
                lines,
                raw: Some(as_styled(source)),
                note: Some(format!("read by {tool} · t shows the source")),
                picture: false,
            },
            Err(reason) => Built {
                lines: as_styled(
                    std::fs::read_to_string(path)
                        .unwrap_or_default()
                        .lines()
                        .map(|l| l.to_string())
                        .collect(),
                ),
                raw: None,
                note: Some(format!("⚠ {reason}")),
                picture: false,
            },
        };
    }

    let markdown_file = matches!(
        path.extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .as_deref(),
        Some("md" | "markdown" | "mdown" | "mkd")
    );
    let (plain, formatted_raw, mut note) = match preview::read(path) {
        preview::Preview::Text { lines, raw, note } => (lines, raw, note),
        preview::Preview::NotText(reason) => (Vec::new(), None, Some(reason)),
    };

    let (lines, raw) = if markdown_file && !plain.is_empty() {
        note = Some("rendered · t shows the source".to_string());
        (markdown::render(&plain), Some(as_styled(plain)))
    } else {
        (as_styled(plain), formatted_raw.map(as_styled))
    };
    Built {
        lines,
        raw,
        note,
        picture: false,
    }
}

/// Plain text as the drawing side wants it: one piece, no styling.
fn as_styled(lines: Vec<String>) -> Vec<markdown::Styled> {
    lines
        .into_iter()
        .map(|line| vec![(line, Style::new())])
        .collect()
}

fn widest_of(lines: &[markdown::Styled]) -> usize {
    lines
        .iter()
        .map(|line| line.iter().map(|(t, _)| width::str_width(t)).sum::<usize>())
        .max()
        .unwrap_or(0)
}

/// The sideways window, across pieces: skip `start` cells, keep `width`.
fn window_of(line: &markdown::Styled, start: usize, width: usize) -> Vec<term::Span<'static>> {
    let mut out = Vec::new();
    let mut passed = 0;
    let mut used = 0;
    for (text, style) in line {
        if used >= width {
            break;
        }
        let cells = width::str_width(text);
        if passed + cells <= start {
            passed += cells;
            continue;
        }
        let piece = width::window(text, start.saturating_sub(passed), width - used);
        passed += cells;
        used += width::str_width(&piece);
        if !piece.is_empty() {
            out.push(term::Span::styled(piece, *style));
        }
    }
    out
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_default()
}

/// Where to look for repositories.
///
/// `~/Development` by default — the whole home directory would drag in
/// archives and backups that take minutes to answer. `FSCTL_ROOTS` overrides
/// it, colon-separated like a PATH.
fn scan_roots() -> Vec<PathBuf> {
    if let Ok(configured) = std::env::var("FSCTL_ROOTS") {
        let roots: Vec<PathBuf> = configured
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .collect();
        if !roots.is_empty() {
            return roots;
        }
    }
    [dirs_home().join("Development")]
        .into_iter()
        .filter(|p| p.is_dir())
        .collect()
}

/// The top of the tree wears its own name, not its whole path — the path is
/// already spelled out above the file list, so repeating it here would only
/// eat the width the tree needs.
fn root_label(path: &Path) -> String {
    if path == dirs_home() {
        return "~".to_string();
    }
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        // The volume root has no name of its own.
        .unwrap_or_else(|| path.display().to_string())
}

/// `/Users/you/Development` shows as `~/Development`.
fn shorten(path: &Path) -> String {
    let home = dirs_home();
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

// ------------------------------------------------------------------- drawing --

fn node_rows(nodes: &[Node], room: usize) -> Vec<Row> {
    nodes
        .iter()
        .map(|node| {
            // ▾ ▸ something unfolds · × truly empty · · hidden content only ·
            // nothing: files, and those are on the right.
            let marker = if node.expandable {
                if node.expanded { "▾ " } else { "▸ " }
            } else if node.empty {
                "× "
            } else if node.hidden_only {
                "· "
            } else {
                "  "
            };
            let indent = "  ".repeat(node.depth);
            let space = room.saturating_sub(indent.len() + 6);
            // Emphasis by weight, not by colour: sshctl keeps Blue for a
            // highlight background only, and dark blue text is unreadable on
            // half the themes out there.
            let mut segments = vec![
                (
                    format!("{indent}{marker}"),
                    Style::new().fg(Color::DarkGray),
                ),
                (
                    width::truncate(&node.label, space),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
            ];
            if !node.detail.is_empty() {
                segments.push((
                    format!("  {}", node.detail),
                    Style::new().fg(Color::DarkGray),
                ));
            }
            Row::new(segments)
        })
        .collect()
}

fn left_rows(app: &App) -> Vec<Row> {
    node_rows(&app.nodes, LEFT_WIDTH as usize)
}

fn right_rows(app: &App, width_cells: usize) -> Vec<Row> {
    const KIND: usize = 11;
    const SIZE: usize = 8;
    const DATE: usize = 17;
    // Narrow panes drop columns from the right instead of squeezing the names
    // down to three letters and an ellipsis.
    let show_date = width_cells >= 66;
    let show_size = width_cells >= 52;
    let fixed =
        4 + KIND + if show_size { SIZE + 1 } else { 0 } + if show_date { DATE + 1 } else { 0 };
    let name_w = width_cells.saturating_sub(fixed).max(10);

    app.items
        .iter()
        .map(|item| {
            let style = if item.is_link {
                Style::new().fg(Color::Cyan)
            } else if item.is_dir {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            let name = if item.is_dir {
                format!("{}/", item.name)
            } else {
                item.name.clone()
            };
            let mut segments = vec![(format!("{} ", width::fit(&name, name_w)), style)];
            match item.git.as_deref() {
                Some(label) => {
                    let colour = match label {
                        "untracked" | "deleted" | "conflict" => Color::Red,
                        "added" => Color::Green,
                        _ => Color::Yellow,
                    };
                    segments.push((
                        width::fit(label, KIND),
                        Style::new().fg(colour).add_modifier(Modifier::BOLD),
                    ));
                }
                None if item.dataless => segments.push((
                    width::fit(&format!("☁ {}", item.kind()), KIND),
                    Style::new().fg(Color::Cyan),
                )),
                None => segments.push((
                    width::fit(&item.kind(), KIND),
                    Style::new().fg(Color::DarkGray),
                )),
            }
            if show_size {
                segments.push((
                    format!(
                        " {}",
                        width::fit_right(&fsmodel::human_size(item.size, item.is_dir), SIZE)
                    ),
                    Style::new().fg(Color::DarkGray),
                ));
            }
            if show_date {
                segments.push((
                    format!(" {}", fsmodel::format_time(item.mtime)),
                    Style::new().fg(Color::DarkGray),
                ));
            }
            Row::new(segments).tickable(app.selection.contains(&item.path))
        })
        .collect()
}

/// The pane under the listing.
///
/// In `Peek` it shows only what is free — the head of the text, markdown laid
/// out by us — because it runs on every arrow key. In `Split` it does the whole
/// job, formatters and thumbnails included: at half the screen it *is* the
/// reading, so it had better show the file the way the window would.
fn draw_peek(frame: &mut term::Frame, app: &mut App, area: Rect) {
    let item = app.items.get(app.right_cursor).cloned();
    let inner_w = area.width.saturating_sub(2);
    let inner_h = area.height.saturating_sub(2);
    let rows = inner_h as usize;

    let (title, lines, note) = match &item {
        None => (" preview ".to_string(), Vec::new(), None),
        Some(item) => {
            let size = (inner_w, inner_h);
            let stale = app
                .peeked
                .as_ref()
                .map(|peeked| peeked.path != item.path || peeked.size != size)
                .unwrap_or(true);
            if stale {
                let (styled, note) = match app.pane {
                    Pane::Split => {
                        let built = build_preview(&item.path, inner_w, inner_h, Shown::Pane);
                        (built.lines, built.note)
                    }
                    _ => {
                        let cheap = match preview::quick(&item.path, 8 * 1024) {
                            Some(lines) if item.name.to_lowercase().ends_with(".md") => {
                                markdown::render(&lines)
                            }
                            Some(lines) => as_styled(lines),
                            None => as_styled(vec![format!(
                                "{} · {} — p for a closer look",
                                item.kind(),
                                fsmodel::human_size(item.size, item.is_dir)
                            )]),
                        };
                        (cheap, None)
                    }
                };
                app.peeked = Some(Peeked {
                    path: item.path.clone(),
                    size,
                    lines: styled,
                    note,
                });
            }
            let (lines, note) = app
                .peeked
                .as_ref()
                .map(|peeked| (peeked.lines.clone(), peeked.note.clone()))
                .unwrap_or_default();
            (
                format!(" {} ", width::truncate(&item.name, inner_w as usize)),
                lines,
                note,
            )
        }
    };

    // The note earns its line only when the pane is tall enough to spare one.
    let body = if note.is_some() && rows > 3 {
        rows - 1
    } else {
        rows
    };
    let mut text: Vec<term::Line> = lines
        .iter()
        .take(body)
        .map(|line| term::Line::from(window_of(line, 0, inner_w as usize)))
        .collect();
    if let Some(note) = note.filter(|_| rows > 3) {
        while text.len() < body {
            text.push(term::Line::default());
        }
        text.push(term::Line::from(term::Span::styled(
            width::fit(&note, inner_w as usize),
            Style::new().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(
        term::Paragraph::new(term::Text::from(text)).block(Block::bordered().title(title)),
        area,
    );
}

fn status_line(app: &App, width_cells: usize) -> String {
    let mut left = format!(" {}", app.status);
    let mut right = format!("sort: {}", app.sort.label());
    if !app.selection.is_empty() {
        right = format!("{} selected  ·  {right}", app.selection.len());
    }
    if let Some(clip) = &app.clipboard {
        let verb = if clip.mode == Mode::Copy {
            "copy"
        } else {
            "move"
        };
        right = format!("clipboard: {} ({verb})  ·  {right}", clip.items.len());
    }
    // The whole signpost, since the bottom bar is gone. It yields the moment
    // the line needs the room for something it actually has to say.
    let with_hint = format!("{right}  ·  ? help");
    if width::str_width(&left) + width::str_width(&with_hint) + 2 <= width_cells {
        right = with_hint;
    }
    let pad = width_cells.saturating_sub(width::str_width(&left) + width::str_width(&right) + 1);
    left.push_str(&" ".repeat(pad));
    left.push_str(&right);
    left
}

fn draw(frame: &mut term::Frame, app: &mut App) {
    let [main, status] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());
    let [left, right] =
        Layout::horizontal([Constraint::Length(LEFT_WIDTH), Constraint::Min(20)]).areas(main);

    let peek_height = app.pane.height(right.height);
    let [files, peek] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(peek_height)]).areas(right);
    let right = files;

    app.screen = (frame.area().width, frame.area().height);
    app.left_height = left.height.saturating_sub(2) as usize;
    app.right_height = right.height.saturating_sub(2) as usize;
    app.left_offset = widgets::scroll_to(
        app.left_offset,
        app.left_cursor,
        app.left_height,
        app.nodes.len(),
    );
    app.right_offset = widgets::scroll_to(
        app.right_offset,
        app.right_cursor,
        app.right_height,
        app.items.len(),
    );

    frame.render_widget(
        List::new(left_rows(app))
            .block(Block::bordered().title(app.source.title()))
            .cursor(app.left_cursor)
            .offset(app.left_offset)
            .focused(app.focus == Focus::Left),
        left,
    );

    let here = app
        .current_path()
        .map(|p| shorten(&p))
        .unwrap_or_else(|| "—".to_string());
    let noun = match app.source {
        Source::Modified => "change(s)",
        _ => "file(s)",
    };
    let title = format!(
        " {} — {} {noun} ",
        width::truncate_start(&here, right.width.saturating_sub(22) as usize),
        app.items.len()
    );
    frame.render_widget(
        List::new(right_rows(app, right.width.saturating_sub(2) as usize))
            .block(Block::bordered().title(title))
            .cursor(app.right_cursor)
            .offset(app.right_offset)
            .focused(app.focus == Focus::Right),
        right,
    );

    if peek_height > 0 {
        draw_peek(frame, app, peek);
    }

    frame.render_widget(
        term::Paragraph::new(term::Line::from(term::Span::styled(
            status_line(app, status.width as usize),
            Style::new().fg(Color::Cyan),
        ))),
        status,
    );

    // The two that need the app as a whole are asked first; the editor is the
    // only one that changes while it draws, so it takes the borrow alone.
    match &app.modal {
        Some(Modal::Conflict(conflict)) => {
            draw_conflict(frame, conflict, app);
            return;
        }
        Some(Modal::Destination(pick)) => {
            draw_destination(frame, pick, app);
            return;
        }
        _ => {}
    }
    let editing_in_pane = app.pane == Pane::Split && peek_height > 0;
    match &mut app.modal {
        Some(Modal::Edit(buffer)) => {
            let area = if editing_in_pane { peek } else { frame.area() };
            draw_edit(frame, buffer, area)
        }
        Some(Modal::Rename(rename)) => draw_rename(frame, rename),
        Some(Modal::Inside(inside)) => draw_inside(frame, inside),
        Some(Modal::Fetch(ask)) => draw_fetch(frame, ask),
        Some(Modal::Help) => draw_help(frame),
        Some(Modal::Look(look)) => draw_look(frame, look),
        Some(Modal::Delete(ask)) => draw_delete(frame, ask),
        _ => {}
    }
}

/// A file in a box: as much of it as the screen holds, and the line you are on.
fn draw_look(frame: &mut term::Frame, look: &Look) {
    let area = frame.area();
    // Nearly the whole screen — a glance at a file wants room, unlike a
    // question, which wants to stay small.
    let w = area.width.saturating_sub(6).max(24);
    let h = area.height.saturating_sub(4).max(6);
    let box_area = Rect {
        x: area.width.saturating_sub(w) / 2,
        y: area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    let inner = w.saturating_sub(2) as usize;
    let rows = h.saturating_sub(2) as usize;

    let mut lines: Vec<term::Line> = Vec::new();
    if look.lines.is_empty() {
        lines.push(term::Line::from(term::Span::styled(
            look.note.clone().unwrap_or_default(),
            Style::new().fg(Color::DarkGray),
        )));
    } else {
        let numbers = if look.picture {
            0
        } else {
            look.lines.len().to_string().len().max(3)
        };
        for (i, line) in look
            .lines
            .iter()
            .enumerate()
            .skip(look.offset)
            .take(rows.saturating_sub(1))
        {
            let mut spans = Vec::new();
            let gutter = if numbers == 0 {
                0
            } else {
                spans.push(term::Span::styled(
                    format!("{:>numbers$} ", i + 1, numbers = numbers),
                    Style::new().fg(Color::DarkGray),
                ));
                numbers + 1
            };
            spans.extend(window_of(line, look.column, inner.saturating_sub(gutter)));
            lines.push(term::Line::from(spans));
        }
    }

    let footer = if look.lines.is_empty() {
        look.note.clone().unwrap_or_default()
    } else {
        // Where you are comes first: after a leap that is the thing you look
        // for. What the file is, and how to leave, follow.
        let mut parts = Vec::new();
        if !look.picture {
            parts.push(format!(
                "line {} of {}",
                (look.offset + 1).min(look.lines.len()),
                look.lines.len()
            ));
        }
        if look.column > 0 {
            parts.push(format!("column {}", look.column + 1));
        }
        if let Some(note) = &look.note {
            parts.push(note.clone());
        }
        if look.path.is_some() && !look.picture {
            parts.push("e edit".to_string());
        }
        parts.push("esc close".to_string());
        parts.join("   ·   ")
    };
    while lines.len() + 1 < rows {
        lines.push(term::Line::default());
    }
    lines.push(term::Line::from(term::Span::styled(
        width::truncate(&footer, inner),
        Style::new().fg(Color::DarkGray),
    )));

    frame.render_widget(term::Clear, box_area);
    frame.render_widget(
        term::Paragraph::new(term::Text::from(lines)).block(
            Block::bordered().title(format!(" {} ", width::truncate(&look.name, inner - 4))),
        ),
        box_area,
    );
}

/// Everything the bottom bar used to say, at the moment you ask for it: one
/// aligned grid, a row per key, the tree marks included — and it names the
/// leap it actually has.
fn draw_help(frame: &mut term::Frame) {
    let dim = Style::new().fg(Color::DarkGray);
    let n = leap_size();
    let row = |keys: &str, what: String| (keys.to_string(), what);
    let gap = || (String::new(), String::new());
    let rows: Vec<(String, String)> = vec![
        row("1 2 3", "folders · repos · unsaved".into()),
        row("Tab", "the other column".into()),
        row(
            "j k ↑ ↓",
            format!("one row · J K ctrl-↑↓ {n} rows · PgUp PgDn a screen"),
        ),
        row("g G", "top · bottom".into()),
        row("h l ← →", "close · open a folder, or switch column".into()),
        row("w W", "make this folder the root · lift the root".into()),
        row("Enter", "open with the system".into()),
        gap(),
        row("space", "tick a file".into()),
        row("c m v", "copy · move (asks where to) · paste".into()),
        row("R", "rename".into()),
        row("z", "pack the ticked into a zip".into()),
        row("x Del", "to the trash, asked first".into()),
        row("Esc", "clear the selection, then the clipboard".into()),
        gap(),
        row("p", "look into a file · e edits it · t the original".into()),
        row(
            "",
            format!("j k d f scroll · shift leaps {n} · 0 left edge"),
        ),
        row("u", "in an archive: unpack the member here".into()),
        row("e", "edit · ctrl-s saves · esc closes".into()),
        row("P", "layout: files only · a strip · half and half".into()),
        row("s S", "sort by name/type/date · reverse".into()),
        row(".", "show hidden files".into()),
        row("r", "refresh".into()),
        row("q", "quit, where you stood".into()),
        gap(),
        row(
            "▾ ▸ × ·",
            "unfolds · holds folders · truly empty · hidden only".into(),
        ),
    ];

    let mut lines: Vec<term::Line> = rows
        .iter()
        .map(|(keys, what)| {
            if keys.is_empty() && what.is_empty() {
                return term::Line::default();
            }
            term::Line::from(vec![
                term::Span::styled(
                    width::fit(keys, 10),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                term::Span::raw(what.clone()),
            ])
        })
        .collect();
    lines.push(term::Line::default());
    lines.push(term::Line::from(term::Span::styled("esc close", dim)));

    let area = frame.area();
    let w = area.width.clamp(24, 66);
    let h = (lines.len() as u16 + 2).min(area.height);
    let box_area = Rect {
        x: area.width.saturating_sub(w) / 2,
        y: area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(term::Clear, box_area);
    frame.render_widget(
        term::Paragraph::new(term::Text::from(lines)).block(Block::bordered().title(" help ")),
        box_area,
    );
}

fn draw_edit(frame: &mut term::Frame, buffer: &mut editor::Editor, area: Rect) {
    // Given the whole screen it centres itself; given the pane it fills it.
    let full = area == frame.area();
    let w = if full {
        area.width.saturating_sub(4).max(24)
    } else {
        area.width
    };
    let h = if full {
        area.height.saturating_sub(2).max(6)
    } else {
        area.height
    };
    let box_area = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    let inner = w.saturating_sub(2) as usize;
    let rows = h.saturating_sub(3) as usize;
    let numbers = buffer.lines.len().to_string().len().max(3);

    buffer.offset = widgets::scroll_to(buffer.offset, buffer.row, rows, buffer.lines.len());

    let mut lines: Vec<term::Line> = Vec::new();
    for (i, line) in buffer
        .lines
        .iter()
        .enumerate()
        .skip(buffer.offset)
        .take(rows)
    {
        let room = inner.saturating_sub(numbers + 1);
        let mut spans = vec![term::Span::styled(
            format!("{:>numbers$} ", i + 1, numbers = numbers),
            Style::new().fg(Color::DarkGray),
        )];
        if i == buffer.row {
            // The caret is drawn, not moved: one reversed cell, always where
            // the next character will land.
            let at = line
                .char_indices()
                .nth(buffer.col)
                .map(|(b, _)| b)
                .unwrap_or(line.len());
            let (before, after) = line.split_at(at);
            let (on, rest) = match after.char_indices().nth(1) {
                Some((i, _)) => after.split_at(i),
                None => (after, ""),
            };
            spans.push(term::Span::raw(width::truncate(before, room)));
            spans.push(term::Span::styled(
                if on.is_empty() {
                    " ".into()
                } else {
                    on.to_string()
                },
                Style::new().add_modifier(Modifier::REVERSED),
            ));
            spans.push(term::Span::raw(rest.to_string()));
        } else {
            spans.push(term::Span::raw(width::truncate(line, room)));
        }
        lines.push(term::Line::from(spans));
    }
    while lines.len() < rows {
        lines.push(term::Line::default());
    }

    let footer = if buffer.asking {
        "unsaved changes:  [s] save and close   [d] throw away   [esc] back".to_string()
    } else {
        format!(
            "{}{}   ·   line {} col {}   ·   ctrl-s save   ·   esc close",
            if buffer.dirty { "modified" } else { "saved" },
            buffer
                .note
                .as_ref()
                .map(|n| format!("   ·   {n}"))
                .unwrap_or_default(),
            buffer.row + 1,
            buffer.col + 1
        )
    };
    lines.push(term::Line::from(term::Span::styled(
        width::fit(&footer, inner),
        if buffer.asking {
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::DarkGray)
        },
    )));

    frame.render_widget(term::Clear, box_area);
    frame.render_widget(
        term::Paragraph::new(term::Text::from(lines)).block(Block::bordered().title(format!(
            " {}{} ",
            width::truncate(&buffer.name, inner.saturating_sub(6)),
            if buffer.dirty { " •" } else { "" }
        ))),
        box_area,
    );
}

fn draw_rename(frame: &mut term::Frame, rename: &Rename) {
    let area = frame.area();
    let w = area.width.clamp(24, 60);
    let h = 5u16.min(area.height);
    let box_area = Rect {
        x: area.width.saturating_sub(w) / 2,
        y: area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    let inner = w.saturating_sub(2) as usize;

    // The caret sits in the text, drawn rather than moved: the real cursor is
    // hidden, and one reversed cell says more than a blinking bar.
    let (before, after) = rename.text.split_at(rename.cursor);
    let (at, rest) = match after.char_indices().nth(1) {
        Some((i, _)) => after.split_at(i),
        None => (after, ""),
    };
    let line = term::Line::from(vec![
        term::Span::raw(before.to_string()),
        term::Span::styled(
            if at.is_empty() {
                " ".to_string()
            } else {
                at.to_string()
            },
            Style::new().add_modifier(Modifier::REVERSED),
        ),
        term::Span::raw(rest.to_string()),
    ]);

    frame.render_widget(term::Clear, box_area);
    frame.render_widget(
        term::Paragraph::new(term::Text::from(vec![
            line,
            term::Line::from(term::Span::raw("")),
            term::Line::from(term::Span::styled(
                width::fit("enter rename   ·   esc keep the old name", inner),
                Style::new().fg(Color::DarkGray),
            )),
        ]))
        .block(Block::bordered().title(" Rename ")),
        box_area,
    );
}

fn draw_destination(frame: &mut term::Frame, pick: &Destination, app: &App) {
    let area = frame.area();
    let w = area.width.clamp(24, 56);
    let h = area.height.saturating_sub(6).max(6);
    let box_area = Rect {
        x: (area.width.saturating_sub(w)) / 2,
        y: (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    let inner = w.saturating_sub(2) as usize;
    let rows = h.saturating_sub(3) as usize;
    let offset = widgets::scroll_to(pick.offset, pick.cursor, rows, pick.nodes.len());

    let count = app.clipboard.as_ref().map(|c| c.items.len()).unwrap_or(0);
    frame.render_widget(term::Clear, box_area);
    frame.render_widget(
        List::new(node_rows(&pick.nodes, inner))
            .block(Block::bordered().title(format!(" Where to with {count} item(s)? ")))
            .cursor(pick.cursor)
            .offset(offset)
            .focused(true),
        box_area,
    );

    let footer = Rect {
        x: box_area.x + 1,
        y: box_area.y + box_area.height - 2,
        width: inner as u16,
        height: 1,
    };
    frame.render_widget(
        term::Paragraph::new(term::Line::from(term::Span::styled(
            width::fit(
                "v to here   ·   l h open and close   ·   esc pick it yourself",
                inner,
            ),
            Style::new().fg(Color::DarkGray),
        ))),
        footer,
    );
}

fn draw_inside(frame: &mut term::Frame, inside: &mut Inside) {
    let area = frame.area();
    let w = area.width.saturating_sub(6).max(24);
    let h = area.height.saturating_sub(4).max(8);
    let box_area = Rect {
        x: area.width.saturating_sub(w) / 2,
        y: area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(term::Clear, box_area);

    // The same division as outside: folders on the left, files on the right.
    let tree_w = (w * 2 / 5).clamp(14, 40);
    let [tree_area, list_area] =
        Layout::horizontal([Constraint::Length(tree_w), Constraint::Min(12)]).areas(Rect {
            height: h.saturating_sub(1),
            ..box_area
        });
    let rows = h.saturating_sub(3) as usize;

    let folder_rows: Vec<Row> = inside
        .rows
        .iter()
        .map(|(depth, path)| {
            let open = *depth == 0 || inside.opened.contains(path);
            let has_children = !archive::folders_in(&inside.folders, path).is_empty();
            let marker = if !has_children {
                "  "
            } else if open {
                "▾ "
            } else {
                "▸ "
            };
            let label = if path.is_empty() {
                width::truncate(&inside.name, tree_w as usize)
            } else {
                archive::leaf_of(path)
            };
            Row::new(vec![
                (
                    format!("{}{marker}", "  ".repeat(*depth)),
                    Style::new().fg(Color::DarkGray),
                ),
                (
                    width::truncate(&label, (tree_w as usize).saturating_sub(depth * 2 + 6)),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
            ])
        })
        .collect();

    let files: Vec<archive::Member> = inside.files().into_iter().cloned().collect();
    let name_w = (list_area.width as usize).saturating_sub(14);
    let file_rows: Vec<Row> = files
        .iter()
        .map(|m| {
            Row::new(vec![
                (width::fit(&archive::leaf_of(&m.name), name_w), Style::new()),
                (
                    width::fit_right(&fsmodel::human_size(m.size, false), 9),
                    Style::new().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();

    inside.dir_offset = widgets::scroll_to(
        inside.dir_offset,
        inside.dir_cursor,
        rows,
        inside.rows.len(),
    );
    inside.file_offset =
        widgets::scroll_to(inside.file_offset, inside.file_cursor, rows, files.len());

    frame.render_widget(
        List::new(folder_rows)
            .block(Block::bordered().title(" in the archive "))
            .cursor(inside.dir_cursor)
            .offset(inside.dir_offset)
            .focused(inside.focus == Focus::Left),
        tree_area,
    );
    let here = inside.here();
    frame.render_widget(
        List::new(file_rows)
            .block(Block::bordered().title(format!(
                " {} — {} ",
                if here.is_empty() {
                    "/".to_string()
                } else {
                    width::truncate_start(&here, 24)
                },
                match files.len() {
                    1 => "1 file".to_string(),
                    n => format!("{n} files"),
                }
            )))
            .cursor(inside.file_cursor)
            .offset(inside.file_offset)
            .focused(inside.focus == Focus::Right),
        list_area,
    );

    let footer = Rect {
        x: box_area.x + 1,
        y: box_area.y + box_area.height - 2,
        width: box_area.width.saturating_sub(2),
        height: 1,
    };
    frame.render_widget(
        term::Paragraph::new(term::Line::from(term::Span::styled(
            width::fit(
                "tab column   ·   enter look   ·   u unpack here   ·   esc close",
                footer.width as usize,
            ),
            Style::new().fg(Color::DarkGray),
        ))),
        footer,
    );
}

fn draw_fetch(frame: &mut term::Frame, ask: &FetchAsk) {
    let lines = vec![
        term::Line::from(term::Span::styled(
            width::truncate(&ask.name, 54),
            Style::new().add_modifier(Modifier::BOLD),
        )),
        term::Line::from(term::Span::styled(
            format!(
                "in the cloud, not on this disk ({})",
                fsmodel::human_size(ask.size, false)
            ),
            Style::new().fg(Color::Cyan),
        )),
        term::Line::from(term::Span::raw("")),
        term::Line::from(term::Span::raw(if ask.hand_over {
            "[Enter] fetch and open        [Esc] leave it"
        } else {
            "[Enter] fetch and look        [Esc] leave it"
        })),
        term::Line::from(term::Span::styled(
            "The screen stands still while it comes in.",
            Style::new().fg(Color::DarkGray),
        )),
    ];
    let area = frame.area();
    let w = area.width.clamp(24, 60);
    let h = (lines.len() as u16 + 2).min(area.height);
    let box_area = Rect {
        x: area.width.saturating_sub(w) / 2,
        y: area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(term::Clear, box_area);
    frame.render_widget(
        term::Paragraph::new(term::Text::from(lines))
            .block(Block::bordered().title(" From the cloud ")),
        box_area,
    );
}

fn draw_delete(frame: &mut term::Frame, ask: &DeleteAsk) {
    let names: Vec<String> = ask
        .items
        .iter()
        .take(4)
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .collect();

    let mut lines = vec![term::Line::from(term::Span::styled(
        match (ask.items.len(), ask.folders) {
            (1, 1) => "This folder to the trash".to_string(),
            (1, _) => "This file to the trash".to_string(),
            (n, 0) => format!("{n} files to the trash"),
            (n, f) => format!("{n} items to the trash, {f} of them folder(s)"),
        },
        Style::new().add_modifier(Modifier::BOLD),
    ))];
    for name in &names {
        lines.push(term::Line::from(term::Span::styled(
            format!("  {name}"),
            Style::new().fg(Color::DarkGray),
        )));
    }
    if ask.items.len() > names.len() {
        lines.push(term::Line::from(term::Span::styled(
            format!("  … and {} more", ask.items.len() - names.len()),
            Style::new().fg(Color::DarkGray),
        )));
    }
    // The one thing the screen could not have told you by itself.
    for name in &ask.concealing {
        lines.push(term::Line::from(term::Span::styled(
            format!("⚠ {name} holds hidden content that goes along"),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(term::Line::from(term::Span::raw("")));
    lines.push(term::Line::from(term::Span::styled(
        "[x] Delete           [Esc] Cancel",
        Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
    )));
    lines.push(term::Line::from(term::Span::styled(
        "Recoverable from the trash.",
        Style::new().fg(Color::DarkGray),
    )));

    let area = frame.area();
    let w = area.width.clamp(24, 60);
    let h = (lines.len() as u16 + 2).min(area.height);
    let box_area = Rect {
        x: area.width.saturating_sub(w) / 2,
        y: area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(term::Clear, box_area);
    frame.render_widget(
        term::Paragraph::new(term::Text::from(lines)).block(Block::bordered().title(" Delete ")),
        box_area,
    );
}

fn draw_conflict(frame: &mut term::Frame, conflict: &Conflict, app: &App) {
    let area = frame.area();
    let w = area.width.clamp(24, 62);
    let h = 10u16.min(area.height);
    let box_area = Rect {
        x: area.width.saturating_sub(w) / 2,
        y: area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(term::Clear, box_area);

    let verb = match app.clipboard.as_ref().map(|c| c.mode) {
        Some(Mode::Cut) => "Moving",
        _ => "Copying",
    };
    let lines = vec![
        term::Line::from(term::Span::raw(format!(
            "{verb} to {}",
            shorten(&conflict.dest)
        ))),
        term::Line::from(term::Span::styled(
            format!(
                "{} item(s) · {} already exist",
                conflict.total, conflict.clashing
            ),
            Style::new().fg(Color::Yellow),
        )),
        term::Line::from(term::Span::raw("")),
        term::Line::from(term::Span::styled(
            "[B] Keep both      — the arrival becomes name-2",
            Style::new().fg(Color::Green),
        )),
        term::Line::from(term::Span::raw("[O] Overwrite      — folders are merged")),
        term::Line::from(term::Span::raw(
            "[S] Skip           — leaves the clashing ones",
        )),
        term::Line::from(term::Span::styled(
            "[Esc] Cancel",
            Style::new().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(
        term::Paragraph::new(term::Text::from(lines))
            .block(Block::bordered().title(" Name clash ")),
        box_area,
    );
}

// ---------------------------------------------------------------------- main --

/// Is our own stdout a terminal?
///
/// Without one there is nobody to draw for and no keys will ever arrive, and
/// the tool would sit there for ever looking like it did nothing. `test -t 1`
/// in a child answers it: the child inherits the very descriptor we are asking
/// about. Cheaper than an isatty binding, and true to the house rule.
fn stdout_is_terminal() -> bool {
    let Some(sh) = toolbox::get().sh.clone() else {
        return true;
    };
    std::process::Command::new(sh)
        .args(["-c", "test -t 1"])
        .status()
        .map(|status| status.success())
        // If we cannot even ask, assume the best and let the user see what
        // happens rather than refusing to start.
        .unwrap_or(true)
}

fn main() -> std::io::Result<()> {
    // Help answers first, and without a terminal: `fsctl --help | less`
    // should work the way every other tool's does.
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return Ok(());
    }
    // What this machine can and cannot do, before anything is drawn.
    if std::env::args().any(|a| a == "--doctor" || a == "-d") {
        print!("{}", toolbox::report());
        return Ok(());
    }
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("fsctl {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if !stdout_is_terminal() {
        eprintln!(
            "fsctl needs a terminal — its output is going somewhere else.\n\
             Start it in a terminal window, without a pipe or a redirect."
        );
        std::process::exit(2);
    }

    let start = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(dirs_home);
    let start = start.canonicalize().unwrap_or(start);

    let mut app = App::new(start);
    let mut terminal = term::init();

    while !app.quit {
        terminal.draw(|frame| draw(frame, &mut app))?;

        // Work announced on the frame that was just drawn.
        if let Some(job) = app.pending.take() {
            match job {
                Pending::ScanRepos => {
                    app.ensure_repos();
                    app.rebuild_left();
                    app.rebuild_right();
                }
                Pending::Trash(items) => {
                    let outcome = ops::trash(&items);
                    app.status = outcome.summary_of("moved to the trash");
                    app.selection.clear();
                    app.rebuild_left();
                    app.rebuild_right();
                }
                Pending::Fetch(ask) => {
                    if ask.hand_over {
                        app.status = match ops::open(&ask.path) {
                            Ok(()) => format!("opened: {}", ask.name),
                            Err(e) => format!("could not open: {e}"),
                        };
                    } else {
                        app.preview_of(ask.path.clone(), ask.name.clone());
                    }
                    app.rebuild_right();
                }
                Pending::Compress { items, dest } => {
                    let count = items.len();
                    app.status = match ops::compress(&items, &dest) {
                        Ok(target) => {
                            let name = target
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            format!("{name} made from {count} item(s)")
                        }
                        Err(e) => format!("packing failed: {e}"),
                    };
                    app.selection.clear();
                    app.rebuild_right();
                }
                Pending::Refresh => {
                    git::refresh(&mut app.repos);
                    app.rebuild_left();
                    app.rebuild_right();
                    app.status = "refreshed".into();
                }
            }
            continue;
        }

        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.on_key(key);
        }
    }

    term::restore();
    Ok(())
}
