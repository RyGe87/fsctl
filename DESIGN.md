# fsctl — design

*A zero-dependency TUI file manager in the line of sshctl.*
Written down 2026-08-04, after the design session; kept current as it was built.

## Why

To replace Finder as the everyday file manager, for two reasons: it writes
`.DS_Store` everywhere (macOS 26.6 has exactly one switch for that,
`DSDontWriteNetworkStores` — verified in the dyld shared cache;
`DSDontWriteUSBStores` and `DSDontWriteStores` do not exist), and it knows only
one perspective: the folder tree.

## Doctrine

1. **Zero dependencies.** As in sshctl. No ratatui, no crossterm, no libc crate.
2. **Whoever knows, does the work.** sshctl asks `ssh -G` instead of comparing
   text. Here: `cp`/`mv` move bytes, `git` knows repository state,
   `plutil`/`xmllint`/`sips`/`unzip` read what they already understand. We
   orchestrate; we do not reimplement.
3. **No manual metadata.** Tagging by hand does not survive contact with daily
   use. Everything shown is derived — or it does not exist.
4. **The index is a cache, never the truth.** Throwing it away is always
   allowed; rebuilding is seconds.

## Reused from sshctl

`term.rs` (1,247 lines when it arrived) is a zero-dependency terminal layer with
a ratatui-shaped API: `Color` `Style` `Span` `Line` `Text` `Rect` `Constraint`
`Layout` `Block` `Paragraph` `Tabs` `Clear` `Frame` `DefaultTerminal` `Event`
`KeyCode`. Raw mode goes through an `stty` subprocess — no FFI, no `unsafe`.

**Copied, not factored out** into a shared crate: two independent tools stay two
independent tools. Improvements travel back by hand.

Four things were added here, all forced by what file names are like:

- **A list widget** with cursor, ticks and scrolling (~150 lines). It serves the
  tree, the file pane, the archive listing and the destination picker.
- **Character width** (~140 lines). `term.rs` counted one column per character,
  which holds for host names and not for file names.
- **A combining mark per cell.** macOS stores names decomposed, so `café`
  arrives as five characters and the accent has to ride along in the same cell
  or the row shifts.
- **256-colour slots** (`Color::Indexed`), for the image thumbnails.

## The screen

```
┌─ sources ────────┬─ items ─────────────────────────┐
│ Folders          │ ▣ name            type    date  │
│ Repos            │ ▢ …                             │
│ Unsaved          │                                 │
└──────────────────┴─────────────────────────────────┘
```

A **source** on the left (a tree), on the right always "the items of the
selected node". One drawing path, several sources. Sorting by name, type or
date; folders live only in the tree. Ticking with space, drawn as ▣/▢.

## Delegated to the system (measured on macOS 26.6)

| action | command | proven |
|---|---|---|
| copy | `/bin/cp -Rc` → fallback `-R` | keeps xattrs, symlinks and permissions; `-c` is an APFS clone (instant, no extra disk) |
| move | `/bin/mv` | handles the volume boundary itself; no EXDEV code needed |
| repository state | `git status --porcelain --branch` | 0.095 s per repository — **per repository, never per file** |
| cache valid? | `stat` on `.git/index` + `.git/HEAD` | unchanged means do not run git |
| formatting | `plutil`, `xmllint` | both ship with macOS; plutil names the line and column of a broken JSON |
| images | `sips` → small BMP | reads everything Apple reads; a BMP is a header and then pixels |
| archives | `unzip -l`, `unzip -p`, `tar` | listing and streaming a single member, with no temporary file |
| trash | `osascript` → Finder | put-back is recorded by whoever moves the file |
| cloud flags | `stat -f %Sf` | `dataless` is not visible through Rust's std |

Absolute paths (`/bin/cp`, not `cp`). Rust's `Command` passes arguments without
a shell, so names with spaces, quotes or newlines carry no risk.

**Trap:** `ditto src dst` copies the *contents* of `src` into `dst`, not the
folder itself. `cp -R` does include it.

**Clashes:** no system tool can "ask on conflict"; `cp -n` skips silently and
still exits 0. So existence is checked before the call, the choice is put to the
user, and only then the right command runs. The decision is UI; moving bytes is
system work.

## Metadata: all derived

Providers of facts, each contributing columns:

- **git** — repository, branch, tracked?, changed?, ignored?, last commit
- **filesystem** — name, type, size, mtime, permissions
- **macOS** — `dataless` (in the cloud, not here), and available but unused:
  `kMDItemWhereFroms`, `kMDItemDownloadedDate`, `com.apple.lastuseddate#PS`

Manual fields (Finder tags via `com.apple.metadata:_kMDItemUserTags`, the Finder
comment via `kMDItemFinderComment`) were proven to work through `plutil` +
`xattr` + `xxd`, and survive `cp` and `mv` — but are **not built**. Across
`~/Development` (465,061 files) there was not a single tag.

**Derived data never becomes an xattr on a file.** That way lies becoming the
new `.DS_Store`: thousands of files smeared with facts that will be wrong
tomorrow. Everything derived lives in memory, rebuilt on demand.

## What was measured, and what it changed

- **26 repositories** under `~/Development`, found by `find` in 0.027 s. A full
  sweep is ≈2.5 s — which is why there is **no background service**.
- **An archived repository took 209 s** for a single `git status`. The 0.1 s
  average held only for active work. Hence a patience of 1.5 s, after which a
  repository is listed as unread rather than waited on.
- **`ls -lR@`** lists a whole tree with its xattr names in one process: 561
  files in 0.07 s. Kept in reserve should tags ever return.
- **Terminal.app knows 256 colours** (`infocmp`) and none of the inline-image
  protocols, which is why pictures are drawn as half blocks.

## Deliberately not built

- **Manual tagging.** It only works if it is automatic.
- **A daemon.** Lazy computation is imperceptible; the global views cost
  seconds once.
- **Opening a file from inside an archive.** You could save, and the saving
  would go nowhere. Extracting into the folder you are standing in is the honest
  version.
- **A parser for anything a system tool already reads.** Markdown is the single
  exception, because macOS ships nothing for it.

## Linux

Done, and verified in a container on the Unraid box rather than reasoned about:
it builds on rustc 1.97, all tests pass, the interface draws, and delete, move,
pack and the JSON preview do what they say. A `toolbox` resolves the tools once
at startup, so one code path serves both systems and a missing tool is a
sentence in `--doctor` instead of a mystery.

What genuinely differs: `cp -Rc` is an Apple flag (`-a --reflink=auto` makes the
same two promises with GNU), `sips` writes to a file while ImageMagick writes to
its output (so on Linux the thumbnail needs no scratch file at all), GNU
`stat -f` asks about the filesystem rather than about file flags (so the cloud
markers exist only where the question does), and there is no Finder to inherit
"Put Back" from — hence the freedesktop pair, written by hand: the file under
`Trash/files` and a `.trashinfo` beside it naming where it came from.

## Open
- **Progress while copying.** `ditto -V` writes a line per file and could feed a
  bar.
- **Saved filters** as a fourth source, over the same derived facts.
