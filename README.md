# diskscope

A cushion-treemap disk usage explorer for macOS, in the shape of
[Disk Inventory X](https://www.derlien.com/) but built to be fast enough that
scanning is not an event you plan around.

On a warm cache it measures a 407,000-entry, 24 GB tree in **0.84 s** — about
**7× faster than `du -sk`** on the same tree, while also building a browsable
tree, classifying every file, and drawing a 150,000-block treemap in 6 ms.

![Diskscope showing 26.8 GB across 416,410 entries of ~/code. Each rectangle is
a file, sized by its share of the disk and coloured by kind; the shading is a
lit cushion per file, nested by directory.](docs/screenshot.png)

```
$ diskscope --text ~/code --top 3
/Users/ivan/code
  24.2 GB  ·  407 166 entries (350 863 files, 56 303 folders)  ·  0.84s
  volume: 108 GB of 494 GB free
  1 447 extra hard links not counted

BY KIND
  Library             1.61 GB   69%       1 282 files
  Executable           475 MB   20%       2 379 files
  ...
```

## Why it is faster

Disk Inventory X is a 2005 Cocoa app: it walks one directory at a time and
calls `lstat` once per file, then keeps an Objective-C object per entry. Four
changes account for the difference, in roughly this order of impact:

1. **`getattrlistbulk(2)` instead of `readdir` + `lstat`.** A macOS-specific
   syscall that returns a whole batch of directory entries *with their metadata
   attached*. With a 256 KiB buffer one call typically covers a couple of
   thousand entries, so a directory of 5,000 files costs about 3 syscalls
   instead of 5,001. Worth **1.4–1.5×** on its own, measured against the same
   walker using `readdir` + `fstatat`.
2. **One task per directory on a work-stealing pool.** Metadata reads are
   latency-bound, not bandwidth-bound, so they parallelise nearly linearly
   until the device runs out of IOPS.
3. **A flat arena instead of an object graph.** A node is 40 bytes with a `u32`
   parent index, and names live as raw bytes in one shared buffer. Nodes are
   stored breadth-first, so a node's children are a contiguous index range (no
   child pointers) and rolling sizes up is a single backwards pass. A 407 k
   entry scan peaks at **66 MB** resident.
4. **Layout cost decoupled from tree size.** A subtree whose rectangle would be
   under ~9 px² is drawn as one aggregate block and never descended into, so
   the treemap costs what the screen can show, not what the disk holds.

`getattrlistbulk` is not available everywhere — network shares and some FUSE
volumes reject it — so there is a `readdir` + `fstatat` walker underneath that
takes over per directory. It is also the reference implementation: the test
suite asserts that both produce byte-identical trees, which is the only reason
to trust unsafe pointer work against a kernel ABI.

## Install

```sh
git clone https://github.com/iloire/diskscope && cd diskscope
scripts/bundle.sh          # builds Diskscope.app
open Diskscope.app
```

Or just `cargo build --release` and run `target/release/diskscope`.

**To scan more than your home folder, grant Full Disk Access** in System
Settings ▸ Privacy & Security ▸ Full Disk Access, and add `Diskscope.app`.
Without it a scan of `/` silently skips most of `~/Library`, `/System/Data`
and every other protected path — the status bar says how many items it could
not read.

## Using it

The window is three bands: what to scan, the map, and a readout for whatever
you are pointing at.

| | |
|---|---|
| **Click** a block | select it; the readout shows its path, kind, size and share |
| **Double-click** | drill into that folder (a file drills into its folder) |
| **Breadcrumb** | jump back to any level |
| ⌫ | up one level |
| ⎋ | clear the selection and any highlighted kind |
| ↩ | reveal the selection in the Finder |
| ⌘⌫ | move the selection to the Trash (asks first) |
| ⌘R / ⌘O | scan again / choose a folder |

Click a row in **Kinds** to pick that kind out of the map — everything else
greys back. **Folders** is the tree, sorted by size, and **Largest** is the
biggest files in view. `ON DISK` / `APPARENT` switches between blocks actually
allocated and apparent length; on disk is the default because it answers the
question that matters, which is how much you would get back.

### Reading the map

Every rectangle is a file, sized by its share of the disk and coloured by kind.
The shading is not decoration: each block is lit as a rounded cushion, and the
cushions nest, so a directory reads as a bump made of smaller bumps. That is
what lets you see structure with no borders drawn at all — the dark gutters
between blocks are the background showing through.

### Command line

```
diskscope [PATH]              open the window, optionally scanning PATH
diskscope --text PATH         print a breakdown instead

  --apparent            size by apparent length rather than blocks on disk
  --all-volumes         descend into other volumes mounted inside the tree
  --keep-hard-links     count every name of a hard-linked file
  --no-bulk             use the portable readdir walker
  --depth N             stop N levels below the root
  --top N               how many largest files to list (default 15)
```

## What it counts

Getting these right is most of what a disk analyser is for:

- **Size on disk** is `ATTR_FILE_ALLOCSIZE` — every fork, rounded to the
  volume's block size. For an HFS-compressed file that is the compressed size,
  while apparent size is the uncompressed length, so the two can differ a lot.
- **Hard links** are counted once, at the first name found. The other names
  still appear in the map, marked, with no size. Turn it off with
  `--keep-hard-links` to match what `du` without `-l` does not do.
- **Symlinks** are measured, never followed, so a link loop cannot hang a scan.
- **Other volumes** mounted inside the tree are marked and not descended into
  unless you ask.
- **Bundles** (`.app`, `.framework`, …) are scanned in full but drawn as one
  block, which is how you actually think about them. Toggle with `BUNDLES`.
- **Free space** appears as a block only when the scan starts at a volume root,
  because anywhere else it is not part of the tree and would distort every
  share.
- **Directories** contribute only what is inside them. APFS reports no
  allocation for a directory record itself.
- **APFS clones** are not detected. Two cloned files share their blocks on
  disk but report full size each, so a tree full of clones reads high. There is
  no cheap way to tell; `du` has the same blind spot.

## Configuring colours and kinds

The file-kind table is data, not code:
[`crates/diskscope-core/data/kinds.json`](crates/diskscope-core/data/kinds.json).
Copy it to `~/.config/diskscope/kinds.json` to recolour or reclassify without
rebuilding; the schema is documented
[next to it](crates/diskscope-core/data/README.md). A malformed override is
reported in the status bar and the built-in table is used instead.

## Development

```sh
cargo test --workspace       # 43 tests: scanner, layout, and the GUI headless
cargo clippy --all-targets
scripts/bench.sh ~/code      # against du, dust and find
```

The GUI is tested by driving the real window against a bare `egui::Context`
with synthesised input — no eframe, no GPU — so a panic in layout, painting or
an action handler fails the build rather than the app. To look at a map without
a window:

```sh
cargo run --release -p diskscope-core --example render_bmp -- ~/code map.bmp
sips -s format png map.bmp --out map.png
```

The screenshot above is regenerated by `scripts/screenshot.sh <path>`. The app
photographs its own framebuffer, because `screencapture` needs Screen Recording
permission and cannot be scripted on a fresh machine.

Architecture notes are in [docs/architecture.md](docs/architecture.md).

## Prior art

[Disk Inventory X](https://www.derlien.com/) by Tjark Derlien, and
[SequoiaView](https://www.win.tue.nl/sequoiaview/) before it. The two papers
they are built on are worth reading and are what this implements:
Bruls, Huizing & van Wijk, *Squarified Treemaps* (2000), and van Wijk & van de
Wetering, *Cushion Treemaps* (1999).

## Licence

MIT.
