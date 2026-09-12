//! End-to-end checks against a synthetic tree with known sizes.
//!
//! The point of most of these is to pin the `getattrlistbulk` path to the
//! plain `readdir` + `fstatat` path: the bulk parser is unsafe pointer work
//! against a kernel ABI, so it is only trustworthy while it keeps producing
//! byte-identical trees to the boring implementation.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use diskscope_core::kinds::KindTable;
use diskscope_core::scan::{scan, Progress, ScanOptions};
use diskscope_core::tree::{flags, NodeId, Tree, ROOT};

/// Builds the fixture and returns the total bytes of file content written.
fn fixture(root: &Path) -> u64 {
    let mut written = 0u64;
    let mut file = |path: &Path, len: usize| {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = fs::File::create(path).unwrap();
        f.write_all(&vec![b'x'; len]).unwrap();
        written += len as u64;
    };

    file(&root.join("top.txt"), 1000);
    file(&root.join("empty.bin"), 0);
    file(&root.join("media/clip.mp4"), 40_000);
    file(&root.join("media/raw/shot.dng"), 70_000);
    file(&root.join("code/main.rs"), 3_000);
    file(&root.join("code/deep/deeper/deepest/note.md"), 500);
    file(&root.join("Thing.app/Contents/MacOS/thing"), 12_000);
    file(&root.join("Thing.app/Contents/Info.plist"), 800);

    // Many small entries, to exercise more than one bulk batch.
    for i in 0..400 {
        file(&root.join("wide").join(format!("f{i:04}.dat")), 64);
    }

    // Multi-byte name: APFS rejects invalid UTF-8 outright, so the thing
    // worth pinning is that names are carried as bytes and never re-encoded.
    file(&root.join(std::ffi::OsStr::from_bytes(ODD_NAME)), 128);

    // Hard link: two names, one inode, one allocation.
    let target = root.join("links/original.bin");
    file(&target, 20_000);
    fs::hard_link(&target, root.join("links/second.bin")).unwrap();

    // Symlinks are measured, never followed. The loop would hang a walker
    // that followed them.
    std::os::unix::fs::symlink("original.bin", root.join("links/alias")).unwrap();
    std::os::unix::fs::symlink(root, root.join("loop")).unwrap();

    fs::create_dir_all(root.join("empty-dir")).unwrap();

    written
}

fn scan_with(root: &Path, opts: ScanOptions) -> Tree {
    let kinds = KindTable::builtin();
    let progress = Progress::default();
    scan(root, &opts, &kinds, &progress).expect("scan succeeds")
}

fn opts(prefer_bulk: bool) -> ScanOptions {
    ScanOptions {
        prefer_bulk,
        // The fixture lives under a temp dir on the boot volume; free space
        // would be identical either way, but leaving it out keeps the totals
        // comparable to `stat`.
        include_free_space: false,
        ..Default::default()
    }
}

/// Path relative to the root -> (flags, logical, physical), for comparison.
fn snapshot(tree: &Tree) -> BTreeMap<String, (u8, u64, u64)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![ROOT];
    while let Some(id) = stack.pop() {
        if id != ROOT {
            let rel = tree
                .path(id)
                .strip_prefix(&tree.root_path)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let n = tree.node(id);
            out.insert(rel, (n.flags, n.logical, n.physical));
        }
        stack.extend(tree.children(id));
    }
    out
}

/// Bytes actually allocated to non-directory entries, straight from `stat`.
fn allocated_by_stat(root: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    fn walk(dir: &Path, total: &mut u64) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            if meta.is_dir() {
                walk(&entry.path(), total);
            } else {
                *total += meta.blocks() * 512;
            }
        }
    }
    let mut total = 0;
    walk(root, &mut total);
    total
}

#[test]
fn bulk_and_posix_walkers_agree_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());

    let bulk = scan_with(tmp.path(), opts(true));
    let posix = scan_with(tmp.path(), opts(false));

    assert!(
        !bulk.stats.used_fallback,
        "the bulk path should work on a temp dir"
    );

    let a = snapshot(&bulk);
    let b = snapshot(&posix);
    assert_eq!(
        a.len(),
        b.len(),
        "different entry counts: {} vs {}",
        a.len(),
        b.len()
    );
    for (path, left) in &a {
        let right = b
            .get(path)
            .unwrap_or_else(|| panic!("{path} missing from the POSIX walk"));
        assert_eq!(
            left, right,
            "{path} differs: bulk {left:?} vs posix {right:?}"
        );
    }

    assert_eq!(bulk.stats.files, posix.stats.files);
    assert_eq!(bulk.stats.dirs, posix.stats.dirs);
    assert_eq!(bulk.node(ROOT).physical, posix.node(ROOT).physical);
}

#[test]
fn logical_total_matches_what_we_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let written = fixture(tmp.path());

    let tree = scan_with(tmp.path(), opts(true));
    // `written` already counts the hard-linked content once, and dedup means
    // the tree counts it once too. Symlinks contribute their own tiny length
    // rather than their target's.
    let symlink_bytes: u64 = (0..tree.len() as NodeId)
        .filter(|&id| tree.node(id).has(flags::SYMLINK))
        .map(|id| tree.node(id).logical)
        .sum();
    let expected = written + symlink_bytes;
    assert_eq!(
        tree.node(ROOT).logical,
        expected,
        "logical total {} vs expected {expected}",
        tree.node(ROOT).logical
    );
}

#[test]
fn physical_total_matches_stat() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());

    // With dedup off we count every link, which is what a plain `stat` walk
    // does, so the two are directly comparable.
    let tree = scan_with(
        tmp.path(),
        ScanOptions {
            dedupe_hardlinks: false,
            ..opts(true)
        },
    );
    assert_eq!(tree.node(ROOT).physical, allocated_by_stat(tmp.path()));
}

#[test]
fn hard_links_are_counted_once() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());

    let deduped = scan_with(tmp.path(), opts(true));
    let raw = scan_with(
        tmp.path(),
        ScanOptions {
            dedupe_hardlinks: false,
            ..opts(true)
        },
    );

    assert_eq!(deduped.stats.hardlink_dups, 1);
    assert_eq!(raw.stats.hardlink_dups, 0);
    assert!(
        raw.node(ROOT).physical - deduped.node(ROOT).physical >= 20_000,
        "dedup should drop about 20 KB"
    );
    // Both names are still present; one of them just has no size.
    let zeroed = (0..deduped.len() as NodeId)
        .filter(|&id| deduped.node(id).has(flags::HARDLINK_DUP))
        .count();
    assert_eq!(zeroed, 1);
}

#[test]
fn symlinks_are_not_followed() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let tree = scan_with(tmp.path(), opts(true));

    let loop_node = (0..tree.len() as NodeId)
        .find(|&id| tree.name(id) == "loop")
        .expect("the self-referential symlink is in the tree");
    let node = tree.node(loop_node);
    assert!(node.has(flags::SYMLINK));
    assert!(!node.is_dir());
    assert_eq!(node.child_len, 0);
}

/// Accents, an em dash, a combining mark and an emoji.
const ODD_NAME: &[u8] = "ødd—na\u{0308}me \u{1F600}.txt".as_bytes();

#[test]
fn multibyte_names_survive() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let tree = scan_with(tmp.path(), opts(true));

    let found = (0..tree.len() as NodeId)
        .find(|&id| tree.name_bytes(id) == ODD_NAME)
        .expect("the multi-byte name is preserved byte for byte");
    // And it is still classified by its extension, and still openable.
    assert_eq!(tree.node(found).logical, 128);
    assert!(tree.path(found).exists());
}

#[test]
fn packages_are_flagged_but_still_measured() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let tree = scan_with(tmp.path(), opts(true));

    let app = (0..tree.len() as NodeId)
        .find(|&id| tree.name(id) == "Thing.app")
        .expect("the bundle is in the tree");
    assert!(tree.node(app).has(flags::PACKAGE));
    assert!(tree.node(app).is_dir());
    assert!(tree.node(app).child_len > 0, "contents are still scanned");
    assert!(tree.node(app).logical >= 12_800, "and still counted");
}

#[test]
fn max_depth_stops_descending_but_keeps_the_stub() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let tree = scan_with(
        tmp.path(),
        ScanOptions {
            max_depth: 1,
            ..opts(true)
        },
    );

    let media = (0..tree.len() as NodeId)
        .find(|&id| tree.name(id) == "media")
        .expect("depth-1 entries are present");
    assert!(tree.node(media).is_dir());
    assert_eq!(tree.node(media).child_len, 0, "but not descended into");
    for id in 0..tree.len() as NodeId {
        assert!(tree.node(id).depth <= 1);
    }
}

#[test]
fn cancelling_returns_a_partial_tree_not_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());

    let kinds = KindTable::builtin();
    let progress = Progress::default();
    progress.cancel();
    let tree = scan(tmp.path(), &opts(true), &kinds, &progress).expect("cancel is not an error");
    assert_eq!(tree.len(), 1, "only the root survives an immediate cancel");
}

#[test]
fn a_single_file_root_is_a_one_node_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("solo.mp4");
    fs::write(&path, vec![0u8; 4096]).unwrap();

    let tree = scan_with(&path, opts(true));
    assert_eq!(tree.len(), 1);
    assert_eq!(tree.node(ROOT).logical, 4096);
    assert_eq!(tree.name(ROOT), "solo.mp4");
}

#[test]
fn every_node_path_resolves_back_to_a_real_entry() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let tree = scan_with(tmp.path(), opts(true));

    for id in 0..tree.len() as NodeId {
        let path = tree.path(id);
        assert!(
            fs::symlink_metadata(&path).is_ok(),
            "node {id} ({}) does not resolve to {}",
            tree.name(id),
            path.display()
        );
    }
}

#[test]
fn layout_and_raster_cover_the_view() {
    use diskscope_core::raster::{rasterize, RasterOptions, NO_NODE};
    use diskscope_core::treemap::{layout, LayoutOptions, Rect};

    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let tree = scan_with(tmp.path(), opts(true));
    let kinds = KindTable::builtin();

    let view = Rect::new(0.0, 0.0, 600.0, 400.0);
    let lay = layout(&tree, ROOT, view, &LayoutOptions::default());
    assert!(lay.block_count() > 50, "only {} blocks", lay.block_count());

    let img = rasterize(&lay, &tree, &kinds, &RasterOptions::default());
    assert_eq!(img.rgba.len(), 600 * 400 * 4);

    // Most of the canvas is covered; what is left is the padding gutters
    // between directories.
    let background = img.ids.iter().filter(|&&id| id == NO_NODE).count();
    let coverage = 1.0 - background as f64 / img.ids.len() as f64;
    assert!(
        coverage > 0.75,
        "only {:.0}% of the view was painted",
        coverage * 100.0
    );

    // Every painted pixel names a node that exists, and hit-testing agrees
    // with the buffer.
    for (i, &id) in img.ids.iter().enumerate() {
        if id != NO_NODE {
            assert!((id as usize) < tree.len(), "pixel {i} names node {id}");
        }
    }
    let (x, y) = (300, 200);
    assert_eq!(img.node_at(x, y), {
        let id = img.ids[y as usize * 600 + x as usize];
        (id != NO_NODE).then_some(id)
    });
    assert_eq!(img.node_at(-1, 0), None);
    assert_eq!(img.node_at(600, 0), None);
}

#[test]
fn kind_totals_add_up_to_the_tree() {
    use diskscope_core::summary::{kind_totals, kind_totals_parallel, largest_files};

    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let tree = scan_with(tmp.path(), opts(true));
    let kinds = KindTable::builtin();

    let totals = kind_totals(&tree, ROOT, kinds.len(), true);
    let summed: u64 = totals.iter().map(|t| t.physical).sum();
    assert_eq!(summed, tree.node(ROOT).physical);

    let files: u64 = totals.iter().map(|t| t.files).sum();
    assert_eq!(files, tree.stats.files);

    // The parallel whole-tree variant must match the sequential one.
    let mut par = kind_totals_parallel(&tree, kinds.len());
    par.sort_by_key(|t| t.kind);
    let mut seq = kind_totals(&tree, ROOT, kinds.len(), true);
    seq.sort_by_key(|t| t.kind);
    assert_eq!(par.len(), seq.len());
    for (a, b) in par.iter().zip(&seq) {
        assert_eq!((a.kind, a.physical, a.files), (b.kind, b.physical, b.files));
    }

    // Largest file is the 70 KB raw photo.
    let top = largest_files(&tree, ROOT, 3, false);
    assert_eq!(tree.name(top[0]), "shot.dng");
    assert!(tree.node(top[1]).logical <= tree.node(top[0]).logical);
}
