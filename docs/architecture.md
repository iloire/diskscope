# Architecture

Two crates. `diskscope-core` is the engine — scanning, the tree, layout,
rasterising — and has no GUI dependency, which is what lets it be benchmarked
and driven from the command line. `diskscope` is the window.

```
crates/diskscope-core/
  src/scan/mod.rs     parallel walk, chunk flatten, size rollup
  src/scan/bulk.rs    getattrlistbulk(2) reader
  src/scan/posix.rs   readdir + fstatat reader (fallback and reference)
  src/tree.rs         the flat arena
  src/kinds.rs        extension -> kind -> colour, loaded from JSON
  src/treemap.rs      squarified layout + cushion coefficients
  src/raster.rs       per-pixel cushion shading, and the id buffer
  src/summary.rs      per-kind and largest-file rollups
  src/fmt.rs          human-readable sizes
crates/diskscope/
  src/app.rs          state, the three bands, actions, headless GUI tests
  src/canvas.rs       the map widget and its render cache
  src/sidebar.rs      kinds / folders / largest
  src/theme.rs        palette, type, egui style
  src/job.rs          scanning off the UI thread
  src/actions.rs      Finder, Trash, volume list
```

## The scan

One task per directory on a rayon work-stealing pool. A task reads its
directory in one or two syscalls, writes the result into a pre-reserved slot as
a **chunk** — a flat `Vec` of entries plus one shared name buffer — and spawns a
task per subdirectory. The only shared state is the slot vector and a sharded
hard-link set, both behind mutexes that are touched once per directory, not
once per file.

Names are never allocated per file. The walker hands out `&[u8]` borrowed from
the reply buffer, and the collector copies the bytes into the chunk's name
buffer at an offset. A million files cost a million offsets, not a million
`String`s.

### Descriptors

Subdirectories are opened with `openat` relative to the parent, which avoids
re-resolving the whole path and sidesteps `PATH_MAX` in deep trees. A queued
task therefore has to keep its parent's descriptor alive, which it does through
an `Arc<DirHandle>` — so the cost is one descriptor per directory that still
has unstarted children, not one per queued task, and the parent reference is
dropped the moment the child's own `openat` returns.

### Flatten

The chunks are then stitched into the arena by a single-threaded pass. It is a
plain queue walk: pop a node, append its chunk's entries, record the child
range. Because a chunk holds all the children of one directory contiguously,
the result comes out in breadth-first order for free, which is what gives every
node a contiguous child range. ~20 ms per million entries, and it is the last
point at which the staging chunks are all live — hence the memory peak.

Sizes roll up in one backwards pass over the vector: in breadth-first order a
child's index always exceeds its parent's, so every child is final by the time
it is read. Kinds are then classified in parallel over the finished nodes.

### Why the arena is breadth-first

The alternative is depth-first with a separate child-index array. Breadth-first
buys three things: `child_start` + `child_len` with no indirection, the
backwards rollup above, and a layout pass that walks the tree level by level in
index order. The cost is that a subtree is not a contiguous range, so rollups
scoped to a drilled-in folder need an explicit stack walk — which is fine,
because they are proportional to that subtree.

### What is deliberately not done

**Children are never sorted.** Squarified layout needs each directory's
children in descending size order, but only for the directories big enough to
be subdivided — a few thousand, not a few hundred thousand. Sorting on demand
during layout is proportional to what is drawn; sorting the arena up front
would mean permuting nodes and fixing every child's parent index, for work that
is mostly thrown away.

**No incremental or live tree.** Chunks only become a tree at the end, so a
scan in progress has counters but nothing to draw. Publishing a partial tree
would mean making the flatten pass incremental and the arena mutable, which
costs more than the feature is worth.

## Layout and rendering

`treemap::layout` walks down from the folder in view, running squarified layout
per directory, and emits a flat list of **leaf cells** plus the directories it
subdivided. A subtree whose rectangle is under `min_area` becomes one aggregate
cell and is not descended into; that single rule is what decouples layout cost
from tree size.

Two properties the rasteriser depends on:

- **Cells tile their parent exactly.** Positions accumulate (`cursor += extent`)
  rather than being recomputed, and the last block in a strip takes the
  remainder, so a block's far edge is bit-identical to its neighbour's near
  edge and pixel snapping cannot produce an overlap.
- **Only leaves are drawn.** Each directory insets its rectangle by the padding
  before laying out children, so the gaps between blocks are the canvas showing
  through. There are no borders to draw and no overlap between a parent's fill
  and a child's.

`raster::rasterize` then shades each cell per pixel from four cushion
coefficients, and writes a parallel **id buffer** of one `NodeId` per pixel.
Parallelism is by horizontal band: each band owns a `&mut` slice of the image,
with cell indices pre-bucketed per band. No `unsafe`, no atomics, and the
output does not depend on thread count.

The id buffer is why hit-testing is exact and free. Pointing at a 2×3 pixel
block is one array index, with no spatial structure to build or keep in sync.

Rendering 150,000 blocks over a 1400×900 canvas takes about 6 ms, and only
happens when the view, window size, size metric or highlighted kind changes.
Hover and selection marks are egui shapes drawn on top, so moving the pointer
costs nothing.

## The window

`App::show` is the whole UI and takes a plain `&mut egui::Ui`, with the
`eframe::App` impl doing nothing but delegating to it. That split exists so the
window can be driven headlessly by `egui::Context::run_ui` with synthesised
input — the GUI tests in `app.rs` scan a fixture, point at the map, click,
drill, switch tabs and metrics, and open the Trash dialog, all without a GPU.

UI code returns `Vec<Action>` rather than mutating state inline, and the frame
applies them after every panel has drawn. That keeps the borrow of `self.tree`
inside the panels immutable, and it means an action triggered from the sidebar
and the same action from a keystroke go through one code path.

## Correctness

The bulk reader is unsafe pointer work against an undocumented-in-practice
kernel ABI, so it is pinned two ways:

- **Synthetic records.** `scan/bulk.rs` builds records byte-for-byte the way
  the kernel does and asserts the parser extracts the right values — including
  the two details that are easy to get wrong and were found by dumping real
  replies: `ATTR_CMN_ERROR` is returned immediately after the returned-attribute
  set rather than in bit order, and the `fileattr` group is simply absent for
  directories.
- **Differential testing.** `tests/scan.rs` scans a fixture with both walkers
  and asserts the trees are identical, entry for entry, flag for flag, byte for
  byte — and separately that the total matches a `stat` walk of the same tree.

Both walkers agreeing on `~/Library` (419,496 entries, 73.2 GB, 141 errors, to
the byte) is the strongest single signal that the parser is right.
