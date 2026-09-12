# Changelog

## 2026-09-12 13:49 — the pointer says what is clickable, and rows are one target

- A hand cursor over a map block that opens into a folder. The map is a single
  widget spanning the band, so nothing previously distinguished a block that
  drills from one with nowhere to go. It is driven by the same rule as the
  double-click — extracted as `canvas::drill_target` and now shared with the
  action — so the cursor cannot promise a move that will not happen. Rows in
  the sidebar get the hand too; none of them look like controls.
- **Fixed: most of a sidebar row did not respond to clicks.** `row` registered
  its click sense with `allocate_exact_size` and only then drew its contents.
  egui hit-tests topmost-first, and a widget drawn later shadows what is under
  it even when it senses nothing but hover — so each row was dead wherever its
  own text fell. On a Kinds row the size and share numbers on the right
  swallowed the click outright, and the kind name did too along the row's
  centre line, leaving the colour swatch and the gaps between labels as the
  only places that worked. The hit rect is now registered after the contents,
  so the whole row is one target. Covered by a test that sweeps a row's full
  width at its middle; the old code fails it.

## 2026-09-12 13:11 — builds and runs on Linux

Same scanner, treemap and window on both platforms; `cargo build --release`
is now the whole install on Linux. Verified with the test suite and clippy on
Linux, and `cargo check --target aarch64-apple-darwin` for the macOS half.

- `getattrlistbulk(2)` is a Darwin syscall, so `scan::bulk` is now gated to
  macOS and the walker choice moved into a `try_bulk` that is a no-op
  elsewhere. `--no-bulk` is inert off macOS, where `posix` is the only walker.
- The `posix` walker was not actually portable: it called `__error()` for
  errno, which is macOS's spelling of glibc's `__errno_location()`, and read
  the name length from `d_namlen`, which Linux's `dirent` does not have. The
  name is now measured with `strlen` on both.
- `statfs` field widths differ between the two libcs, so the free-space
  arithmetic casts rather than assuming.
- Desktop integration is per-platform: `xdg-open` instead of `open`, volumes
  found under `/media`, `/mnt` and `/run/media/$USER` instead of `/Volumes`,
  and Ctrl rather than ⌘ in the shortcut hints. "Reveal" selects the item on
  macOS and opens its folder on Linux, which is as close as is portable.
- `bundle.sh` refuses to run off macOS; `screenshot.sh` falls back to
  ImageMagick where there is no `sips`, and builds its temp path in a way both
  mktemp implementations accept.
- The walker-equivalence test and the walker comparison in `examples/bench`
  are macOS-only now — with one walker they compared `posix` against itself
  and reported a meaningless 1.00×.

## 2026-09-12 00:00 — first working version

Cushion-treemap disk usage explorer for macOS, in the shape of Disk Inventory X.

### Engine (`diskscope-core`)

- Parallel directory walk, one task per directory on a rayon work-stealing
  pool, reading metadata through `getattrlistbulk(2)` — a batch of entries with
  their attributes per syscall instead of `readdir` plus one `lstat` per file.
  Measured 1.4–1.5× over the same walker using `readdir` + `fstatat`, and ~7×
  over `du -sk` end to end.
- `readdir` + `fstatat` walker underneath, taking over per directory on
  filesystems that reject the bulk call, and doubling as the reference
  implementation the bulk parser is tested against.
- Flat arena tree: 40-byte nodes, `u32` indices, names as raw bytes in one
  shared buffer, stored breadth-first so children are a contiguous range and
  size rollup is a single backwards pass. 407 k entries peak at 66 MB resident.
- Counts size on disk (all forks, allocated) or apparent size; hard links once;
  symlinks measured but never followed; mount points marked and not crossed;
  bundles flagged; free space added only at a volume root.
- Squarified treemap layout with nested cushion shading, culling any subtree
  below a pixel threshold so layout cost tracks the screen rather than the
  tree. 150 k blocks laid out and shaded in ~6 ms.
- Software rasteriser producing an RGBA image and a parallel id buffer, so
  hit-testing a block is one array index.
- File kinds and colours loaded from JSON, overridable at
  `~/.config/diskscope/kinds.json`.

### Application (`diskscope`)

- egui/wgpu window: toolbar with a proportional breadcrumb, sidebar with
  kinds / folders / largest-files, treemap canvas, and a readout bar.
- Drill into folders by double-click or breadcrumb, highlight a file kind,
  switch between size on disk and apparent size, collapse bundles.
- Reveal in Finder, open, copy path, and move to Trash behind a confirmation.
- Scan runs off the UI thread with live counters and can be stopped, keeping
  whatever was measured.
- `--text` mode printing the same breakdown, plus `--depth`, `--top`,
  `--apparent`, `--all-volumes`, `--keep-hard-links`, `--no-bulk`.
- Commit SHA stamped in at build time and shown in the readout bar.

### Tests

43 tests: synthetic-record parsing for the kernel ABI, differential testing of
the two walkers against each other and against `stat`, squarified layout
invariants (exact tiling, no overlap, proportional areas), and six headless GUI
tests that drive the real window through `egui::Context::run_ui`.

Also: `--screenshot FILE` makes the window photograph itself, which is how
`docs/screenshot.png` is generated (`scripts/screenshot.sh`) — `screencapture`
needs Screen Recording permission and cannot be scripted.
