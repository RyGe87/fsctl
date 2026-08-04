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

mod fsmodel;
mod git;
mod ops;
mod term;
mod widgets;
mod width;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fsmodel::{Entry, Sort};
use git::Repo;
use ops::{Clipboard, Mode, Resolution};
use term::event::{self, Event, KeyCode, KeyEventKind};
use term::{Block, Color, Constraint, Layout, Modifier, Rect, Style};
use widgets::{List, Row};

const LEFT_WIDTH: u16 = 34;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Folders,
    Repos,
    Modified,
}

impl Source {
    fn title(self) -> &'static str {
        match self {
            Source::Folders => " Mappen ",
            Source::Repos => " Repo's ",
            Source::Modified => " Onopgeslagen ",
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
    Refresh,
}

struct Conflict {
    dest: PathBuf,
    clashing: usize,
    total: usize,
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
    conflict: Option<Conflict>,
    pending: Option<Pending>,
    left_height: usize,
    right_height: usize,
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
            status: format!("{}", shorten(&root)),
            conflict: None,
            pending: None,
            left_height: 20,
            right_height: 20,
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
            Source::Folders => {
                let probe = fsmodel::probe(&self.root, self.show_hidden);
                let mut nodes = vec![Node {
                    label: root_label(&self.root),
                    detail: String::new(),
                    path: self.root.clone(),
                    depth: 0,
                    expandable: probe.has_subdir,
                    expanded: self.expanded.contains(&self.root),
                    empty: probe.empty,
                    hidden_only: probe.hidden_only,
                }];
                if self.expanded.contains(&self.root) {
                    let root = self.root.clone();
                    self.push_children(&root, 1, &mut nodes);
                }
                nodes
            }
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
                    detail: format!("{} gewijzigd", r.changes.len()),
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

    fn push_children(&self, dir: &Path, depth: usize, out: &mut Vec<Node>) {
        // Deep trees are legal but unreadable; the right pane is where you go
        // deeper.
        if depth > 12 {
            return;
        }
        for child in fsmodel::subdirectories(dir, self.show_hidden) {
            let expanded = self.expanded.contains(&child.path);
            // A folder with nothing to unfold gets no triangle: the mark
            // should promise something.
            let probe = fsmodel::probe(&child.path, self.show_hidden);
            out.push(Node {
                label: child.name.clone(),
                detail: String::new(),
                path: child.path.clone(),
                depth,
                expandable: probe.has_subdir,
                expanded,
                empty: probe.empty,
                hidden_only: probe.hidden_only,
            });
            if expanded {
                self.push_children(&child.path, depth + 1, out);
            }
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
            // Mappen horen links, bestanden rechts: de boom is de enige plek
            // waar een map staat, dus de rechterkolom toont er geen.
            _ => {
                let mut items: Vec<Entry> = fsmodel::read_dir(&path, self.show_hidden)
                    .into_iter()
                    .filter(|e| !e.is_dir)
                    .collect();
                fsmodel::sort(&mut items, self.sort, self.reverse);
                self.attach_git(&path, &mut items);
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
            0 => format!("{} repo's gevonden", self.repos.len()),
            n => format!(
                "{} repo's gevonden · {n} te traag om te lezen",
                self.repos.len()
            ),
        };
    }

    // ----------------------------------------------------------------- keys --

    fn on_key(&mut self, code: KeyCode) {
        if self.conflict.is_some() {
            self.on_conflict_key(code);
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
                self.status = format!("gesorteerd op {}", self.sort.label());
                self.rebuild_right();
            }
            KeyCode::Char('u') => {
                self.reverse = !self.reverse;
                self.status = if self.reverse {
                    "omgekeerde volgorde".into()
                } else {
                    "gewone volgorde".into()
                };
                self.rebuild_right();
            }
            KeyCode::Char('.') => {
                self.show_hidden = !self.show_hidden;
                self.status = if self.show_hidden {
                    "verborgen bestanden zichtbaar".into()
                } else {
                    "verborgen bestanden verborgen".into()
                };
                self.rebuild_left();
                self.rebuild_right();
            }
            KeyCode::Char('w') => self.root_here(),
            KeyCode::Char('W') => self.root_up(),
            KeyCode::Char('c') => self.yank(Mode::Copy),
            KeyCode::Char('x') => self.yank(Mode::Cut),
            KeyCode::Char('v') => self.paste(),
            KeyCode::Char('r') => {
                self.status = "verversen…".into();
                self.pending = Some(Pending::Refresh);
            }
            KeyCode::Esc => {
                if self.selection.is_empty() {
                    self.clipboard = None;
                    self.status = "klembord leeg".into();
                } else {
                    self.selection.clear();
                    self.status = "selectie gewist".into();
                }
            }
            _ => {}
        }
    }

    fn on_conflict_key(&mut self, code: KeyCode) {
        let how = match code {
            KeyCode::Char('o') | KeyCode::Char('O') => Resolution::Overwrite,
            KeyCode::Char('b') | KeyCode::Char('B') => Resolution::KeepBoth,
            KeyCode::Char('s') | KeyCode::Char('S') => Resolution::Skip,
            KeyCode::Esc | KeyCode::Char('q') => {
                self.conflict = None;
                self.status = "afgebroken".into();
                return;
            }
            _ => return,
        };
        let Some(conflict) = self.conflict.take() else {
            return;
        };
        self.execute_paste(&conflict.dest, how);
    }

    fn switch(&mut self, source: Source) {
        self.source = source;
        self.left_cursor = 0;
        self.left_offset = 0;
        self.right_cursor = 0;
        self.right_offset = 0;
        if source != Source::Folders && !self.scanned {
            self.status = "repo's zoeken…".into();
            self.pending = Some(Pending::ScanRepos);
            return;
        }
        self.rebuild_left();
        self.rebuild_right();
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
            self.status = match ops::open(&path) {
                Ok(()) => format!("geopend: {name}"),
                Err(e) => format!("openen mislukt: {e}"),
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
        self.status = format!("wortel: {}", shorten(&path));
        self.rebuild_left();
        self.rebuild_right();
    }

    /// Lifts the tree one level, keeping the old root open and under the
    /// cursor so you can see where you came from.
    fn root_up(&mut self) {
        let Some(parent) = self.root.parent().map(|p| p.to_path_buf()) else {
            self.status = "hier houdt het op".into();
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
        self.status = format!("wortel: {}", shorten(&parent));
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
            self.status = "niets om te nemen".into();
            return;
        }
        let verb = match mode {
            Mode::Copy => "kopiëren",
            Mode::Cut => "verplaatsen",
        };
        self.status = if self.focus == Focus::Left {
            let name = items
                .first()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            format!("map {name} klaar om te {verb}")
        } else {
            format!("{} bestand(en) klaar om te {verb}", items.len())
        };
        self.clipboard = Some(Clipboard { items, mode });
    }

    fn paste(&mut self) {
        let Some(clip) = self.clipboard.clone() else {
            self.status = "klembord is leeg".into();
            return;
        };
        let Some(dest) = self.current_path() else {
            return;
        };
        let clashing = ops::conflicts(&clip.items, &dest).len();
        if clashing == 0 {
            self.execute_paste(&dest, Resolution::Overwrite);
            return;
        }
        self.conflict = Some(Conflict {
            dest,
            clashing,
            total: clip.items.len(),
        });
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

fn left_rows(app: &App) -> Vec<Row> {
    app.nodes
        .iter()
        .map(|node| {
            // ▾ ▸ er valt iets uit te klappen · × écht leeg · · alleen
            // verborgen inhoud · niets: bestanden, en die staan rechts.
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
            let room = (LEFT_WIDTH as usize).saturating_sub(indent.len() + 6);
            // Emphasis by weight, not by colour: sshctl keeps Blue for a
            // highlight background only, and dark blue text is unreadable on
            // half the themes out there.
            let mut segments = vec![
                (
                    format!("{indent}{marker}"),
                    Style::new().fg(Color::DarkGray),
                ),
                (
                    width::truncate(&node.label, room),
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

fn right_rows(app: &App, width_cells: usize) -> Vec<Row> {
    const KIND: usize = 11;
    const SIZE: usize = 8;
    const DATE: usize = 17;
    // Narrow panes drop columns from the right instead of squeezing the names
    // down to three letters and an ellipsis.
    let show_date = width_cells >= 66;
    let show_size = width_cells >= 52;
    let fixed = 4 + KIND
        + if show_size { SIZE + 1 } else { 0 }
        + if show_date { DATE + 1 } else { 0 };
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
                        "ongetrackt" | "verwijderd" | "conflict" => Color::Red,
                        "toegevoegd" => Color::Green,
                        _ => Color::Yellow,
                    };
                    segments.push((
                        width::fit(label, KIND),
                        Style::new().fg(colour).add_modifier(Modifier::BOLD),
                    ));
                }
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

fn status_line(app: &App, width_cells: usize) -> String {
    let mut left = format!(" {}", app.status);
    let mut right = format!("sortering: {}", app.sort.label());
    if !app.selection.is_empty() {
        right = format!("{} geselecteerd  ·  {right}", app.selection.len());
    }
    if let Some(clip) = &app.clipboard {
        let verb = if clip.mode == Mode::Copy {
            "kopie"
        } else {
            "verplaats"
        };
        right = format!("klembord: {} ({verb})  ·  {right}", clip.items.len());
    }
    let pad = width_cells.saturating_sub(width::str_width(&left) + width::str_width(&right) + 1);
    left.push_str(&" ".repeat(pad));
    left.push_str(&right);
    left
}

fn draw(frame: &mut term::Frame, app: &mut App) {
    let [main, status, help] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    let [left, right] =
        Layout::horizontal([Constraint::Length(LEFT_WIDTH), Constraint::Min(20)]).areas(main);

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
        Source::Modified => "wijziging(en)",
        _ => "bestand(en)",
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

    frame.render_widget(
        term::Paragraph::new(term::Line::from(term::Span::styled(
            status_line(app, status.width as usize),
            Style::new().fg(Color::Cyan),
        ))),
        status,
    );
    frame.render_widget(
        term::Paragraph::new(term::Line::from(term::Span::styled(
            " 1/2/3 bron · Tab kolom · w wortel hier · W wortel omhoog · spatie vink aan · c kopieer · x knip · v plak · s sorteer · . verborgen · r ververs · q sluit",
            Style::new().fg(Color::DarkGray),
        ))),
        help,
    );

    if let Some(conflict) = &app.conflict {
        draw_conflict(frame, conflict, app);
    }
}

fn draw_conflict(frame: &mut term::Frame, conflict: &Conflict, app: &App) {
    let area = frame.area();
    let w = area.width.min(62).max(24);
    let h = 10u16.min(area.height);
    let box_area = Rect {
        x: area.width.saturating_sub(w) / 2,
        y: area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(term::Clear, box_area);

    let verb = match app.clipboard.as_ref().map(|c| c.mode) {
        Some(Mode::Cut) => "Verplaatsen",
        _ => "Kopiëren",
    };
    let lines = vec![
        term::Line::from(term::Span::raw(format!(
            "{verb} naar {}",
            shorten(&conflict.dest)
        ))),
        term::Line::from(term::Span::styled(
            format!(
                "{} item(s) · {} bestaan al",
                conflict.total, conflict.clashing
            ),
            Style::new().fg(Color::Yellow),
        )),
        term::Line::from(term::Span::raw("")),
        term::Line::from(term::Span::styled(
            "[B] Beide bewaren  — aankomst wordt naam-2",
            Style::new().fg(Color::Green),
        )),
        term::Line::from(term::Span::raw(
            "[O] Overschrijven  — mappen worden samengevoegd",
        )),
        term::Line::from(term::Span::raw("[S] Overslaan      — laat de botsers staan")),
        term::Line::from(term::Span::styled(
            "[Esc] Afbreken",
            Style::new().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(
        term::Paragraph::new(term::Text::from(lines))
            .block(Block::bordered().title(" Naamconflict ")),
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
    std::process::Command::new("/bin/sh")
        .args(["-c", "test -t 1"])
        .status()
        .map(|status| status.success())
        // If we cannot even ask, assume the best and let the user see what
        // happens rather than refusing to start.
        .unwrap_or(true)
}

fn main() -> std::io::Result<()> {
    if !stdout_is_terminal() {
        eprintln!(
            "fsctl heeft een terminal nodig — de uitvoer gaat nu ergens anders heen.\n\
             Start hem rechtstreeks in een terminalvenster, zonder pipe of omleiding."
        );
        std::process::exit(2);
    }

    let start = std::env::args()
        .nth(1)
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
                Pending::Refresh => {
                    git::refresh(&mut app.repos);
                    app.rebuild_left();
                    app.rebuild_right();
                    app.status = "ververst".into();
                }
            }
            continue;
        }

        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.on_key(key.code);
        }
    }

    term::restore();
    Ok(())
}
