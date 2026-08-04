# fsctl

A file manager for the terminal, in two columns: a **source** on the left, its
items on the right. The folder tree is only one of those sources — git knows
which repositories exist and what is unsaved in them, and that gives two more
views of the same files without a second way of drawing anything.

**Zero dependencies.** The terminal layer is ours (carried over from
[sshctl](https://github.com/RyGe87/sshctl)), and all the real work goes to the
system: `cp` and `mv` move the bytes, `git status` reports the repositories,
`open` opens files, `stty` puts the terminal in raw mode.

macOS and Linux. Which features are available depends on what the machine has;
`fsctl --doctor` tells you in one screen.

**What it does**

- **Three sources** over the same files: the folder tree, every git repository
  under your roots, and only the repositories with unsaved work
- **A preview that reads the file**: JSON, XML and property lists formatted,
  markdown rendered, HTML read as a page, pictures as a thumbnail of half
  blocks, archives as a tree — none of it parsed by us
- **Archives without unpacking**: walk a zip in two columns, read a member
  straight out of it, take one out when you want it
- **Copy, move, pack, rename, delete** — with the destination asked for in the
  tree you already know, clashes counted before anything moves, and deletes
  going to the trash your desktop understands
- **A small writer** for text files, which never leaves a file half-written
- **Three layouts**, from all-files to half-and-half, on one key

## Doctor

```
$ fsctl --doctor
  copy       ✓  /usr/bin/cp                  cp -a --reflink=auto
  move       ✓  /usr/bin/mv                  mv -f
  trash      ✓  ~/.local/share/Trash
  json       ✓  /usr/bin/python3 (python3)   formatting in the preview
  images     ✗  not found                    thumbnails in the preview
```

A missing tool turns off one feature; nothing else changes. The tools are
resolved once at startup by absolute path, never through `$PATH` — a file
manager that copies your files should not be steered by an environment
variable.

| | macOS | Linux |
|---|---|---|
| copy | `cp -Rc` (APFS clone) | `cp -a --reflink=auto` |
| open | `open` | `xdg-open` |
| trash | Finder, so "Put Back" works | `~/.local/share/Trash` + `.trashinfo`, so "Restore" works |
| json | `plutil` | `jq` or `python3 -m json.tool` |
| xml | `xmllint` | `xmllint` |
| images | `sips` | ImageMagick (and then without a scratch file) |
| plists | `plutil` | — |
| cloud ☁ | `stat -f %Sf` | — (placeholders differ per provider) |

## Install

```sh
cargo build --release
ln -sf "$PWD/target/release/fsctl" /opt/homebrew/bin/fsctl
```

### Walking with your shell

`q` writes down the folder you were standing in, so your shell can follow you
there. Put this in `~/.zshrc`:

```sh
f() {
  local out="$(mktemp)"
  FSCTL_CWD_FILE="$out" command fsctl "$@"
  local dir="$(cat "$out")"; rm -f "$out"
  [ -n "$dir" ] && [ -d "$dir" ] && cd "$dir"
}
```

After that `f` is your file manager, and where you leave it is where you stand.

## Keys

| | |
|---|---|
| `1` `2` `3` | switch source |
| `Tab` | switch column |
| `j` `k` · arrows | up and down |
| `J` `K` · `Ctrl`+arrow | ten at a time |
| `l` `→` | open a folder, or move right |
| `h` `←` | close a folder, or move back left |
| `Enter` | open the file with `open` |
| `w` | make the folder under the cursor the root of the tree |
| `W` | lift the root one level |
| `space` | tick a file |
| `c` `m` `v` | copy · move (asks where to) · paste |
| `z` | pack the selection into a zip, here |
| `p` | look into the file (`j`/`k` up and down, `d`/`f` sideways, `t` raw) |
| `e` | edit a text file (`ctrl-s` saves, `esc` closes) |
| `u` | in an archive: unpack the selected member here |
| `R` | rename what the cursor is on |
| `P` | the layout: files only · a strip below · half and half |
| `x` `Del` | to the trash (asks first) |
| `s` `S` | sort by name/type/date · reverse the order |
| `.` | show hidden files |
| `r` | refresh |
| `Esc` | clear the selection, then the clipboard |
| `?` | the help, with everything on this list |
| `q` | quit |

In the tree, a mark in front of a folder says what to expect:

```
▾  open                  ▸  holds folders
×  truly empty           ·  looks empty, holds hidden content
(nothing)  files only — and those are on the right
```

The triangle and the cross answer **different** questions on purpose. The
triangle is about what unfolding would show, so it follows your `.` setting — a
triangle that opens onto nothing is a lie. The cross is about what is actually
there, hidden files included, because `cp` and `mv` act on the folder as it is
on disk and not on our filtered view of it. A folder holding nothing but a
`.env` may never be called empty; it gets a dot.

**Folders on the left, files on the right.** The tree is the only place a folder
lives; the right column shows none. Copying a whole folder is therefore done
from the tree: `c` or `m` takes the folder under the cursor. On the right they
work on what you ticked, or else on the row you are standing on.

## The sources

- **Folders** — the ordinary tree, natural sorting (`v2` before `v10`),
  symlinks kept as symlinks.
- **Repos** — every git repository under your search roots, with branch, number
  of changes, and ↑↓ against the remote.
- **Unsaved** — only the repositories that have changes; on the right, every
  modified or untracked file.

In any folder view that falls inside a repository, the files carry their git
state as a column. That costs no extra `git` call: the status is already in
hand.

Search roots default to `~/Development`. Override with `FSCTL_ROOTS`,
colon-separated like a `PATH`:

```sh
FSCTL_ROOTS="$HOME/Development:$HOME/Work" fsctl
```

## What it leaves to the system

| action | command | why |
|---|---|---|
| copy | `/bin/cp -Rc`, falling back to `-R` | keeps xattrs, symlinks and permissions; `-c` clones on APFS (instant, no extra disk) |
| move | `/bin/mv -f` | handles the volume boundary itself |
| open | `/usr/bin/open` | macOS knows which app belongs to it |
| find repositories | `/usr/bin/find` | one process over hundreds of thousands of files |
| repository state | `git --no-optional-locks status --porcelain --branch` | per repository, never per file |
| json and plists | `/usr/bin/plutil` | knows both, and says exactly where a JSON is broken |
| xml | `/usr/bin/xmllint` | ships with macOS |
| images | `/usr/bin/sips` | reads everything Apple reads |
| html | `/usr/bin/textutil` | WebKit's importer, so entities and scripts are handled |
| archives | `/usr/bin/unzip`, `/usr/bin/zip`, `/usr/bin/tar` | listing and streaming, without unpacking |
| trash | `/usr/bin/osascript` → Finder | put-back is recorded by whoever moves the file |
| cloud flags | `/usr/bin/stat` | `st_flags` is not something Rust's std will show |
| timezone | `/bin/date +%z` | cheaper than parsing `/etc/localtime` ourselves |

Arguments go straight to the process, never through a shell. A file called
`; rm -rf ~` is just a file with an awkward name.

## Copying, and what happens on a clash

`cp -R` **merges** folders; it does not replace them. A file that existed only
at the destination survives — Finder, in the same situation, throws the whole
destination folder away. We inherit the safer semantics for free by handing the
work over.

Clashing names are counted **before** anything moves, not discovered along the
way: you get one question with the full picture instead of seven interruptions.

- **[B] Keep both** — the arrival becomes `name-2`; nothing at the destination
  is touched
- **[O] Overwrite** — folders are merged, files of the same name replaced
- **[S] Skip** — the clashing ones stay put, the rest goes
- **[Esc]** — cancel

## Moving: where to?

`m` takes what you ticked and asks straight away where it should go, in the same
tree you already know:

```
┌ Where to with 1 item(s)? ────────────────────────────┐
│  ▾ movetest                                          │
│      source                                          │
│    ▸ destination                                     │
│                                                      │
│v to here  ·  l h open and close  ·  esc pick yourself│
└──────────────────────────────────────────────────────┘
```

`Esc` closes the tree but **leaves the clipboard filled**, so you can always
walk somewhere yourself and press `v` there, as before. `c` is unchanged; only
`m` asks.

## Packing

`z` makes a zip of what you ticked — or of the row you are on, or of the folder
under the tree cursor. `zip -r` runs from the deepest folder that holds them
all, with relative names, so the archive carries the shape the files had rather
than the `/Users/you/…` of the machine that made it.

One item lends its own name; several take the name of the folder they land in.
Pack the folder you are standing in and the archive lands beside it — an archive
that contains itself is a riddle. An existing name is never overwritten; it
becomes `name-2.zip`.

## Three layouts

`P` cycles the right column through three divisions:

| | the listing | the pane below |
|---|---|---|
| **files only** | all of it | — · `p` and `e` open a window |
| **a strip below** | most of it | the head of the text, and only what is free |
| **half and half** | half | the whole job: formatted, and written in place |

**Half and half** is the default. There the pane is not a glance but the reading
itself, so it shows the file exactly as the window would: JSON and XML laid out,
markdown rendered, a picture as a thumbnail, and a broken JSON with plutil's
complaint under it. `e` then writes *in that pane* instead of over the whole
screen, so the listing stays where it was while you type.

**A strip below** keeps the same pane cheap: the head of the text and nothing
that costs a process, because there it runs on every arrow key. Formatters and
thumbnails stay behind `p`.

Everything is recomputed only when the file under the cursor changes, or when
the pane changes size.

## Renaming

`R` offers the old name with the caret parked at the end of the stem, so a typo
in the name is one keystroke away and the extension is not in the way. A slash
is refused — moving is what `m` is for — and so is a name that already exists.

## Writing

`e` opens a text file for editing: type, `Enter` splits a line, `Backspace`
joins it again, the arrows and `Home`/`End`/`PgUp`/`PgDn` move about. `ctrl-s`
saves, `Esc` closes — and if there are unsaved changes it asks first, with
`[s] save and close`, `[d] throw away` and `[esc] back`.

Saving writes a neighbouring file and renames it over the original, so a crash
halfway leaves you with the old file whole rather than half a new one. The
permissions come along, because an executable script has to stay executable, and
a file that did not end in a newline does not silently gain one.

It edits what fits in memory (up to 4 MB) and refuses anything that is not text.
No undo, no syntax colouring, no autosave — for that, `Enter` still hands the
file to the editor that was built for it.

## A look inside a file

`p` opens the file under the cursor in a window: numbered lines, scrolling with
`j`/`k`, `PgUp`/`PgDn` and `g`/`G`, and **sideways** with `d`/`f` in steps of
eight columns (`0` returns to the start; `h`/`l` and the arrows do the same).
Long lines are cut rather than wrapped — code keeps its shape, and what falls
off the edge you slide into view.

**Markdown is rendered**: headings bold without their hashes, `-` becomes `•`,
`**strong**` and `_emphasis_` lose their markers and keep their weight, code
colours, a quote gets a rule down its side, `---` becomes a line, and inside a
fence everything stays exactly as typed. Every source line stays one screen
line, so the numbers keep telling the truth and `t` shows you the same file
rather than a different shape of it.

This is the only format we lay out ourselves — macOS ships no tool for it.
`snake_case_names` are left alone: an underscore inside a word is not emphasis.

**HTML shows its source in the pane and reads as a page in the window.** The
pane is where `e` writes, and you cannot write in a rendering — so there you get
the tags, with `source · p renders it` underneath. Press `p` and the page reads
as a page: `textutil` hands over WebKit's own importer,
which already knows entities, encodings, tables and that a `<script>` is not
text. On Linux `w3m`, `lynx` or `html2text` do the same. What a terminal misses
afterwards we add ourselves — which lines were headings, found by scanning the
source for `<h1>`…`<h6>` — so a page reads with its structure intact. `t` shows
the source.

**JSON, XML and property lists are formatted** before you see them, by the tools
macOS already brings: `plutil` for JSON and plists, `xmllint` for XML. No
parsers here. `t` shows the original as it sits on disk.

When the formatter refuses, that is the news: a JSON file that will not format
is broken, and plutil says where. You get the raw text plus the complaint:

```
  1 {"broken":}
⚠ Invalid value around line 1, column 9.   ·   esc close
```

A **binary** plist is not text by any honest test, and is perfectly readable
once `plutil` has turned it back into XML — so that one case is allowed through.

Whether something is text at all is decided by its **content**, not its
extension: a `Makefile` or `.zshrc` has none, and a `.log` is sometimes binary.
A zero byte in the first 128 KB settles it, and the window says so. Nothing
beyond 128 KB is read; if the file runs on, that is noted at the bottom.

**Images become a thumbnail** of half blocks: every `▀` carries two pixels — its
ink the upper one, its paper the lower — which buys back the vertical resolution
a character cell costs. See-through pixels are left to the terminal, so an icon
keeps its shape. `sips` does the decoding and reads everything Apple reads: png,
jpeg, heic, tiff, gif, bmp, pdf. Terminal.app knows 256 colours, so the colours
land in that palette — a 6×6×6 cube, with the grey ramp for what is grey.

## Looking inside an archive

`p` on a `.zip`, `.dls`, `.jar`, `.epub`, `.tar.gz` or family shows what is in
there — **without unpacking**:

```
┌ test.zip — 3 items ────────────────────────────────────┐
│  text.txt                                         17 B │
│  dir/                                                — │
│  dir/data.json                                     8 B │
│                                                        │
│enter look   ·   e extract here   ·   esc close         │
└────────────────────────────────────────────────────────┘
```

In the fixed pane the archive shows its shape without opening anything —
indented by depth, folders in bold:

```
┌ project.zip ───────────────────────────────┐
│w/                                        — │
│  docs/                                   — │
│    gids.md                             2 B │
│  leesmij.md                            2 B │
│  src/                                    — │
```

Reading a zip's central directory costs no unpacking, so this is as cheap as
showing the first lines of a text file.

`Enter` on a member reads it straight out of the archive — `unzip -p` writes it
to its output and we read along. **No temporary file** is involved: nothing to
clean up, and nothing you might edit by accident in a place that is about to
vanish. `Esc` takes you back to the listing.

Opening a member in another app is deliberately absent, for exactly that reason:
you could save, and the saving would go nowhere. `e` is the honest answer — it
extracts the selected member **into the folder you are standing in**, as a real
file, in a place that will still be there tomorrow. An existing name is never
overwritten.

## Cloud folders

iCloud, OneDrive and Proton Drive are ordinary folders on your disk, so browsing
them needs nothing special:

```
~/Library/Mobile Documents/com~apple~CloudDocs    iCloud Drive
~/Library/CloudStorage/OneDrive-…                 OneDrive
~/Library/CloudStorage/ProtonDrive-…              Proton Drive
```

What you see there is not necessarily *there*. macOS flags files that exist only
in the listing as `dataless`: full name, full size, no bytes. Those get a **☁**
in the type column:

```
▢ annual-report-2025.pdf        ☁ pdf
```

Looking (`p`) or opening (`Enter`) then forces a download, and fsctl asks first —
with the size, because that is what it costs:

```
┌ From the cloud ──────────────────────────────────────────┐
│annual-report-2025.pdf                                    │
│in the cloud, not on this disk (4.2 M)                    │
│                                                          │
│[Enter] fetch and look        [Esc] leave it              │
│The screen stands still while it comes in.                │
└──────────────────────────────────────────────────────────┘
```

The flags come from one `stat` for the whole listing, and only inside a cloud
folder — an ordinary directory pays nothing for a question that cannot arise
there.

Two things stand: **copying** a ☁ file fetches it too, without asking (that is a
deliberate act), and do not point `FSCTL_ROOTS` at a cloud folder — a repository
sweep through thousands of non-local files takes forever.

## Deleting

To the trash, never straight out. Finder does it through `osascript`, because
the put-back information for "Put Back" is recorded by whoever moves the file —
and only Finder records it. Paths travel as arguments, not inside the script
text, so a quote in a name cannot become part of the program.

The question up front says what it costs: how many items, how many folders, and
— the one thing your screen could not have told you — **which folders hold
hidden content that goes along**. That is the same `·` from the tree, now as a
warning.

If Finder will not play along (automation not permitted, Finder busy), the files
still go to `~/.Trash`, moved by hand. Recoverable, but without put-back; the
status line says so. To keep Finder out of it entirely: `FSCTL_TRASH=plain`.

The root of the tree cannot be deleted — the view would have nothing to stand
on.

## Limits of v0.1

- **Cloud markers are macOS-only.** The `dataless` flag is an Apple thing; on
  Linux every provider marks placeholders differently, so the ☁ column stays
  off there.
- **`Ctrl-J` does not exist.** That is byte `0x0A`, and that *is* Enter — no
  terminal can tell the two apart. Hence `J`/`K` for the leap of ten, and
  `Ctrl`+arrow where your terminal forwards it. Note that macOS keeps
  `Ctrl`+↑/↓ for Mission Control by default.
- **No progress bar.** An APFS clone is instant; a real copy across a volume
  boundary makes the tool stand still for a moment. `ditto -V` could feed one
  later.
- **Slow repositories are skipped.** Measured here: an ordinary repository
  answers in ~0.1 s, but an archived one with a large untracked tree took 209 s.
  After a second and a half fsctl stops waiting and shows "too slow — not read".
  The repository stays in the list.
- **No background service.** A sweep over 26 repositories costs a few seconds;
  `r` refreshes only what has moved since last time (measured against
  `.git/index` and `.git/HEAD`).
- **Character width is a compact table**, not the full Unicode database. A rare
  character can cost one cell of alignment, never more.

## Design

See [DESIGN.md](DESIGN.md) — including the measurements the choices rest on, and
what was deliberately *not* built: manual tagging, a daemon, metadata written as
xattrs onto your files.

## Licence

MIT — see [LICENSE](LICENSE).
