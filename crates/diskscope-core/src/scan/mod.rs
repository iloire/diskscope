//! The parallel walk.
//!
//! # Shape of the algorithm
//!
//! One task per directory, run on a rayon work-stealing pool. A task reads its
//! directory in one or two [`bulk`] syscalls, writes the result into a
//! pre-reserved slot as a **chunk** (a flat `Vec` of entries plus one shared
//! name buffer), and spawns a task per subdirectory. Nothing is shared between
//! tasks except the slot vector and a hard-link set, so the walk scales with
//! cores until the device runs out of IOPS.
//!
//! A second, single-threaded pass then *flattens* the chunks into the
//! breadth-first arena the rest of the program uses. Because a chunk holds all
//! the children of one directory contiguously, flattening is a plain queue
//! walk: pop a node, append its chunk, record the child range. That pass is
//! ~20 ms per million entries and it is what makes the resulting tree compact
//! and cache-friendly.
//!
//! # Descriptor discipline
//!
//! Subdirectories are opened with `openat` relative to the parent, which
//! avoids re-resolving the path (and sidesteps `PATH_MAX` in deep trees). A
//! queued task therefore has to keep the parent's descriptor alive, which it
//! does through an `Arc<DirHandle>` — one descriptor per directory with
//! unstarted children, not one per queued task. The parent reference is
//! dropped the moment the child's own `openat` returns.

pub mod bulk;
pub mod posix;

use std::ffi::{CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use rayon::prelude::*;

use crate::kinds::{KindTable, KIND_FOLDER, KIND_FREE};
use crate::tree::{flags, make_node, Node, NodeId, ScanStats, Tree, ROOT};

/// One directory entry as reported by either walker. `name` borrows the
/// walker's buffer, so it must be copied out before the callback returns.
pub struct Ent<'a> {
    pub name: &'a [u8],
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_regular: bool,
    pub dev: i64,
    pub ino: u64,
    pub nlink: u32,
    pub logical: u64,
    pub physical: u64,
    /// `errno` for entries the filesystem could not describe; 0 when fine.
    pub error: i32,
}

#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// Descend into directories that live on another device.
    pub cross_mounts: bool,
    /// Count only the first link to a multiply-linked inode.
    pub dedupe_hardlinks: bool,
    /// Flag `.app`/`.framework`/… so the treemap can draw them as one block.
    pub detect_packages: bool,
    /// Depth below the root at which to stop descending.
    pub max_depth: u8,
    /// Try `getattrlistbulk` before falling back to `readdir` + `fstatat`.
    pub prefer_bulk: bool,
    /// Add a synthetic "free space" node when the root is a volume.
    pub include_free_space: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            cross_mounts: false,
            dedupe_hardlinks: true,
            detect_packages: true,
            max_depth: u8::MAX,
            prefer_bulk: true,
            include_free_space: true,
        }
    }
}

/// Live counters, polled by the UI while a scan runs.
#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub symlinks: AtomicU64,
    pub hardlink_dups: AtomicU64,
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
    pub cancel: AtomicBool,
    pub finished: AtomicBool,
}

impl Progress {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn entries(&self) -> u64 {
        self.files.load(Ordering::Relaxed) + self.dirs.load(Ordering::Relaxed)
    }
}

/// How many errors we keep the text of. A scan of `/` without Full Disk Access
/// produces tens of thousands of identical denials; a handful is enough to
/// explain what happened.
const MAX_ERROR_SAMPLES: usize = 64;

/// Hard-link sets are sharded to keep the lock off the hot path. Only inodes
/// with `nlink > 1` are ever inserted, so contention is low to begin with.
const HARDLINK_SHARDS: usize = 64;

/// Entry as stored in a chunk, before flattening. 32 bytes, no allocation:
/// the name is an offset into the chunk's shared name buffer.
#[derive(Clone, Copy)]
struct RawEntry {
    name_off: u32,
    name_len: u16,
    flags: u8,
    _pad: u8,
    /// Slot holding this entry's children, or `NO_CHUNK`.
    chunk: u32,
    logical: u64,
    physical: u64,
}

const NO_CHUNK: u32 = u32::MAX;

#[derive(Default)]
struct DirChunk {
    entries: Vec<RawEntry>,
    names: Vec<u8>,
    /// The directory itself could not be read.
    unreadable: bool,
}

/// An open directory, closed when the last queued child lets go of it.
struct DirHandle {
    fd: libc::c_int,
    path: PathBuf,
}

impl Drop for DirHandle {
    fn drop(&mut self) {
        // SAFETY: we own `fd` for the lifetime of the handle and close it once.
        unsafe { libc::close(self.fd) };
    }
}

struct Ctx<'k> {
    opts: ScanOptions,
    kinds: &'k KindTable,
    progress: &'k Progress,
    chunks: Mutex<Vec<Option<DirChunk>>>,
    hardlinks: Vec<Mutex<std::collections::HashSet<(i64, u64)>>>,
    errors: Mutex<Vec<(PathBuf, String)>>,
    root_dev: i64,
    used_fallback: AtomicBool,
}

impl Ctx<'_> {
    fn reserve_slot(&self) -> u32 {
        let mut chunks = self.chunks.lock();
        chunks.push(None);
        (chunks.len() - 1) as u32
    }

    fn store(&self, slot: u32, chunk: DirChunk) {
        self.chunks.lock()[slot as usize] = Some(chunk);
    }

    fn record_error(&self, path: PathBuf, err: &io::Error) {
        self.progress.errors.fetch_add(1, Ordering::Relaxed);
        let mut errors = self.errors.lock();
        if errors.len() < MAX_ERROR_SAMPLES {
            errors.push((path, err.to_string()));
        }
    }

    /// Returns true when this is the first link we have seen to the inode.
    fn claim_inode(&self, dev: i64, ino: u64) -> bool {
        let shard = &self.hardlinks[(ino as usize) % HARDLINK_SHARDS];
        shard.lock().insert((dev, ino))
    }
}

struct Task {
    /// Kept alive only so `openat` below has a base; `None` for the root.
    parent: Option<Arc<DirHandle>>,
    name: Box<[u8]>,
    slot: u32,
    depth: u8,
}

thread_local! {
    static BULK_BUF: std::cell::RefCell<bulk::Buffer> =
        std::cell::RefCell::new(bulk::Buffer::new());
}

/// Walks `root` and returns the tree.
///
/// Runs on the ambient rayon pool. Cancel by setting [`Progress::cancel`]; the
/// scan then returns the partial tree it has built rather than an error.
pub fn scan(
    root: &Path,
    opts: &ScanOptions,
    kinds: &KindTable,
    progress: &Progress,
) -> io::Result<Tree> {
    let started = Instant::now();
    let root = root.canonicalize()?;
    let meta = std::fs::symlink_metadata(&root)?;

    if !meta.is_dir() {
        return Ok(single_file_tree(root, &meta, kinds, started));
    }

    let root_dev = {
        use std::os::unix::fs::MetadataExt;
        meta.dev() as i64
    };

    let ctx = Ctx {
        opts: opts.clone(),
        kinds,
        progress,
        chunks: Mutex::new(Vec::with_capacity(1024)),
        hardlinks: (0..HARDLINK_SHARDS)
            .map(|_| Mutex::new(Default::default()))
            .collect(),
        errors: Mutex::new(Vec::new()),
        root_dev,
        used_fallback: AtomicBool::new(false),
    };
    let root_slot = ctx.reserve_slot();
    debug_assert_eq!(root_slot, 0);

    rayon::scope(|s| {
        run(
            s,
            &ctx,
            Task {
                parent: None,
                name: root.as_os_str().as_bytes().into(),
                slot: root_slot,
                depth: 0,
            },
        );
    });

    let chunks = ctx.chunks.into_inner();
    let (nodes, names) = flatten(chunks, kinds);

    let mut stats = ScanStats {
        files: progress.files.load(Ordering::Relaxed),
        dirs: progress.dirs.load(Ordering::Relaxed),
        symlinks: progress.symlinks.load(Ordering::Relaxed),
        hardlink_dups: progress.hardlink_dups.load(Ordering::Relaxed),
        errors: progress.errors.load(Ordering::Relaxed),
        error_samples: ctx.errors.into_inner(),
        used_fallback: ctx.used_fallback.load(Ordering::Relaxed),
        elapsed: started.elapsed(),
        free_space: None,
        volume_total: None,
    };
    if let Some((free, total)) = volume_space(&root) {
        stats.free_space = Some(free);
        stats.volume_total = Some(total);
    }

    let root_name = root
        .file_name()
        .map(|n| n.as_bytes().to_vec())
        .unwrap_or_else(|| root.as_os_str().as_bytes().to_vec());

    let mut tree = Tree::new(root, nodes, names, stats);
    tree.set_name(ROOT, &root_name);

    // Only meaningful when the root *is* the volume: anywhere else the free
    // space is not "inside" the tree and adding it would distort every share.
    if opts.include_free_space && is_volume_root(&tree.root_path) {
        if let Some(free) = tree.stats.free_space {
            tree.push_free_space(free, KIND_FREE);
        }
    }

    progress.finished.store(true, Ordering::Relaxed);
    Ok(tree)
}

fn run<'s, 'k: 's>(s: &rayon::Scope<'s>, ctx: &'s Ctx<'k>, task: Task) {
    if ctx.progress.is_cancelled() {
        ctx.store(task.slot, DirChunk::default());
        return;
    }

    let name = OsStr::from_bytes(&task.name);
    let path = match &task.parent {
        Some(p) => p.path.join(name),
        None => PathBuf::from(name),
    };

    let fd = match open_dir(task.parent.as_deref(), &task.name, &path) {
        Ok(fd) => fd,
        Err(e) => {
            ctx.record_error(path, &e);
            ctx.store(
                task.slot,
                DirChunk {
                    unreadable: true,
                    ..Default::default()
                },
            );
            return;
        }
    };
    // The parent descriptor has done its job; let it close before we start
    // reading, so deep trees hold at most one descriptor per active level.
    drop(task.parent);

    let dir = Arc::new(DirHandle { fd, path });
    // Deliberately not pre-reserved: the median directory holds a handful of
    // entries, and reserving for the mean instead cost ~90 MB of slack across
    // a 56 k-directory tree for no measurable time saving.
    let mut chunk = DirChunk::default();
    let mut subdirs: Vec<(usize, Box<[u8]>)> = Vec::new();

    let result = enumerate(ctx, dir.fd, &mut |ent: Ent<'_>| {
        collect(ctx, &mut chunk, &mut subdirs, ent, task.depth)
    });

    if let Err(e) = result {
        ctx.record_error(dir.path.clone(), &e);
        chunk.unreadable = true;
    }

    ctx.progress.dirs.fetch_add(1, Ordering::Relaxed);

    // Slots must exist before the chunk is published, so that flattening never
    // sees an entry pointing at a slot that has not been reserved.
    for (idx, _) in &subdirs {
        chunk.entries[*idx].chunk = ctx.reserve_slot();
    }
    let spawns: Vec<Task> = subdirs
        .into_iter()
        .map(|(idx, name)| Task {
            parent: Some(Arc::clone(&dir)),
            name,
            slot: chunk.entries[idx].chunk,
            depth: task.depth.saturating_add(1),
        })
        .collect();
    ctx.store(task.slot, chunk);
    drop(dir);

    for t in spawns {
        s.spawn(move |s| run(s, ctx, t));
    }
}

/// Reads one directory, preferring the bulk syscall and falling back per
/// directory when the filesystem does not support it.
fn enumerate(ctx: &Ctx<'_>, fd: libc::c_int, push: &mut impl FnMut(Ent<'_>)) -> io::Result<()> {
    if ctx.opts.prefer_bulk {
        let mut pushed = 0usize;
        let outcome = BULK_BUF.with(|buf| {
            bulk::read_dir(fd, &mut buf.borrow_mut(), |ent| {
                pushed += 1;
                push(ent);
            })
        });
        match outcome {
            Ok(()) => return Ok(()),
            // Only retry from scratch if nothing was emitted yet. A failure
            // part-way through cannot be undone, so it is reported as-is
            // rather than replayed into duplicate entries.
            Err(e) if pushed == 0 && bulk::is_unsupported(&e) => {
                ctx.used_fallback.store(true, Ordering::Relaxed);
                rewind(fd)?;
            }
            Err(e) => return Err(e),
        }
    }
    posix::read_dir(fd, push)
}

fn rewind(fd: libc::c_int) -> io::Result<()> {
    // SAFETY: `fd` is a live directory descriptor.
    if unsafe { libc::lseek(fd, 0, libc::SEEK_SET) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Turns one `Ent` into a chunk entry, queuing subdirectories for descent.
fn collect(
    ctx: &Ctx<'_>,
    chunk: &mut DirChunk,
    subdirs: &mut Vec<(usize, Box<[u8]>)>,
    ent: Ent<'_>,
    depth: u8,
) {
    let name_off = chunk.names.len() as u32;
    let name_len = ent.name.len().min(u16::MAX as usize);
    chunk.names.extend_from_slice(&ent.name[..name_len]);

    let mut flags = 0u8;
    let mut logical = ent.logical;
    let mut physical = ent.physical;

    if ent.error != 0 {
        flags |= flags::UNREADABLE;
        logical = 0;
        physical = 0;
    } else if ent.is_dir {
        flags |= flags::DIR;
        // A directory's own size is whatever its children add up to; APFS
        // reports no allocation for the directory record itself.
        logical = 0;
        physical = 0;
        if ctx.opts.detect_packages && ctx.kinds.is_package(ent.name) {
            flags |= flags::PACKAGE;
        }
        let crossing = ent.dev != ctx.root_dev && ent.dev != 0;
        if crossing && !ctx.opts.cross_mounts {
            flags |= flags::OTHER_FS;
        } else if u16::from(depth) + 1 < u16::from(ctx.opts.max_depth) {
            // `depth` belongs to the directory being read, so this entry sits
            // one level deeper; `max_depth` caps the depth of nodes we descend
            // *into*, not the depth we record.
            subdirs.push((chunk.entries.len(), ent.name[..name_len].into()));
        }
    } else {
        if ent.is_symlink {
            flags |= flags::SYMLINK;
            ctx.progress.symlinks.fetch_add(1, Ordering::Relaxed);
        }
        if ctx.opts.dedupe_hardlinks
            && ent.nlink > 1
            && ent.is_regular
            && !ctx.claim_inode(ent.dev, ent.ino)
        {
            flags |= flags::HARDLINK_DUP;
            logical = 0;
            physical = 0;
            ctx.progress.hardlink_dups.fetch_add(1, Ordering::Relaxed);
        }
        ctx.progress.files.fetch_add(1, Ordering::Relaxed);
        ctx.progress.bytes.fetch_add(physical, Ordering::Relaxed);
    }

    chunk.entries.push(RawEntry {
        name_off,
        name_len: name_len as u16,
        flags,
        _pad: 0,
        // Filled in by the caller once a slot has been reserved.
        chunk: NO_CHUNK,
        logical,
        physical,
    });
}

fn open_dir(parent: Option<&DirHandle>, name: &[u8], path: &Path) -> io::Result<libc::c_int> {
    let base_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    let fd = match parent {
        Some(p) => {
            let cname =
                CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            // `O_NOFOLLOW` closes the window where an entry reported as a
            // directory is swapped for a symlink before we open it.
            // SAFETY: `p.fd` is live and `cname` outlives the call.
            unsafe { libc::openat(p.fd, cname.as_ptr(), base_flags | libc::O_NOFOLLOW) }
        }
        None => {
            let cpath = CString::new(path.as_os_str().as_bytes())
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            // SAFETY: `cpath` outlives the call.
            unsafe { libc::open(cpath.as_ptr(), base_flags) }
        }
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Chunks to arena: a breadth-first queue walk, which is just a forward scan
/// over the node vector because children are appended in order.
fn flatten(chunks: Vec<Option<DirChunk>>, kinds: &KindTable) -> (Vec<Node>, Vec<u8>) {
    let total: usize = chunks.iter().flatten().map(|c| c.entries.len()).sum();
    let name_bytes: usize = chunks.iter().flatten().map(|c| c.names.len()).sum();

    let mut chunks = chunks;
    let mut nodes: Vec<Node> = Vec::with_capacity(total + 2);
    let mut names: Vec<u8> = Vec::with_capacity(name_bytes + 64);
    // Which chunk holds each node's children, parallel to `nodes`.
    let mut pending: Vec<u32> = Vec::with_capacity(total + 2);

    nodes.push(make_node(ROOT, 0, 0, flags::DIR, 0, 0, 0));
    pending.push(0);

    let mut i = 0usize;
    while i < nodes.len() {
        let slot = pending[i];
        if slot == NO_CHUNK {
            i += 1;
            continue;
        }
        let Some(chunk) = chunks[slot as usize].take() else {
            i += 1;
            continue;
        };
        if chunk.unreadable {
            nodes[i].flags |= flags::UNREADABLE;
        }
        let depth = nodes[i].depth.saturating_add(1);
        let base = names.len() as u32;
        names.extend_from_slice(&chunk.names);

        nodes[i].child_start = nodes.len() as NodeId;
        nodes[i].child_len = chunk.entries.len() as u32;
        for e in &chunk.entries {
            nodes.push(make_node(
                i as NodeId,
                base + e.name_off,
                e.name_len,
                e.flags,
                depth,
                e.logical,
                e.physical,
            ));
            pending.push(e.chunk);
        }
        i += 1;
    }

    // Sizes roll up in one backwards pass: in breadth-first order a child's
    // index always exceeds its parent's, so every child is already final by
    // the time we reach it.
    for i in (1..nodes.len()).rev() {
        let n = nodes[i];
        let p = n.parent as usize;
        nodes[p].logical += n.logical;
        nodes[p].physical += n.physical;
    }

    classify(&mut nodes, &names, kinds);
    (nodes, names)
}

fn classify(nodes: &mut [Node], names: &[u8], kinds: &KindTable) {
    // 400 k+ hash lookups is worth spreading over the pool; the name buffer is
    // read-only here so the split is trivially safe.
    nodes.par_iter_mut().for_each(|n| {
        n.kind = if n.flags & (flags::DIR | flags::FREE_SPACE) != 0 {
            KIND_FOLDER
        } else {
            let off = n.name_off as usize;
            kinds.classify(&names[off..off + n.name_len as usize])
        };
    });
}

fn single_file_tree(
    path: PathBuf,
    meta: &std::fs::Metadata,
    kinds: &KindTable,
    started: Instant,
) -> Tree {
    use std::os::unix::fs::MetadataExt;
    let name = path
        .file_name()
        .map(|n| n.as_bytes().to_vec())
        .unwrap_or_else(|| path.as_os_str().as_bytes().to_vec());
    let mut nodes = vec![make_node(
        ROOT,
        0,
        name.len() as u16,
        if meta.is_symlink() { flags::SYMLINK } else { 0 },
        0,
        meta.len(),
        meta.blocks() * 512,
    )];
    nodes[0].kind = kinds.classify(&name);
    let stats = ScanStats {
        files: 1,
        elapsed: started.elapsed(),
        ..Default::default()
    };
    Tree::new(path, nodes, name, stats)
}

/// `(available, total)` bytes for the volume holding `path`.
fn volume_space(path: &Path) -> Option<(u64, u64)> {
    let cpath = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: zeroed `statfs` is a valid out-param and `cpath` outlives the call.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(cpath.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let block = st.f_bsize as u64;
    Some((st.f_bavail * block, st.f_blocks * block))
}

/// True when `path` is itself a mount point, which is the only case where
/// adding free space to the tree makes sense.
fn is_volume_root(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(here) = std::fs::metadata(path) else {
        return false;
    };
    match path.parent() {
        None => true,
        Some(parent) => match std::fs::metadata(parent) {
            Ok(up) => up.dev() != here.dev(),
            Err(_) => false,
        },
    }
}
