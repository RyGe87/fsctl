# Changelog

## 0.1.1 — 2026-08-05

- Two fixes out of a full review. Lowercasing an HTML page could grow a
  letter ('İ' becomes two) and shift the heading scan off its indices; and
  a wide or decomposed character in a window title bent the border row.
  Both carry a regression test now.
- The help window lists Esc, and x admits that Del does the same.
- Install by `cargo install --path .`, which works on both systems the
  tool runs on.
- The releases build themselves: a universal macOS binary, signed and
  notarized, a Linux x86_64 build, checksums, and build-provenance
  attestations — the same route sshctl takes. Clippy stands at zero and
  rustfmt owns the layout; CI holds both.

## 0.1.0 — 2026-08-05

The first release: a file manager for the terminal, in two columns, that
asks the system instead of reimplementing it — `cp` and `mv` move the
bytes, `git` knows the repositories, and the preview is read by the tools
the machine already has.

- Three sources over the same files: the folder tree, every git repository
  under your roots, and only the ones with unsaved work.
- A preview that reads the file: JSON, XML and plists formatted, markdown
  rendered, HTML read as a page, pictures as half-block thumbnails,
  archives as a tree — none of it parsed by us.
- Copy, move, pack, rename, delete — clashes counted before anything
  moves, deletes go to the trash your desktop understands.
- A small writer that never leaves a file half-written, and three layouts
  on one key.
- macOS and Linux; `fsctl --doctor` says in one screen what this machine
  can do.

Built in collaboration with Claude (Anthropic): the first version together
with Claude Opus 5; the review, the fixes and the release pipeline with
Claude Fable 5.
