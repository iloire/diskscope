# Changelog

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
