//! The scanned filesystem as a flat arena.
//!
//! A `Tree` is three vectors and nothing else: no per-node allocation, no
//! `Rc`, no `PathBuf` per entry. A node is 40 bytes and its name lives as raw
//! bytes in one shared buffer, so a 2 million file scan costs ~100 MB instead
//! of the ~1 GB an object-per-file design needs.
//!
//! Nodes are stored in **breadth-first order**, which buys two things:
//!
//! * the children of any node are one contiguous index range, so a node only
//!   needs `child_start` + `child_len` and there is no child-pointer array;
//! * a child always has a higher index than its parent, so bottom-up size
//!   aggregation is a single backwards pass over the vector.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Index into [`Tree`]'s node vector. `u32` caps a scan at 4.29 G entries.
pub type NodeId = u32;

/// The scan root is always node 0.
pub const ROOT: NodeId = 0;

pub mod flags {
    /// Entry is a directory.
    pub const DIR: u8 = 1 << 0;
    /// Symlink. Never followed; the link itself is measured.
    pub const SYMLINK: u8 = 1 << 1;
    /// A macOS bundle (`.app`, `.framework`, …). Scanned, but drawn as one block.
    pub const PACKAGE: u8 = 1 << 2;
    /// Second or later hard link to an inode we already counted; size is zeroed.
    pub const HARDLINK_DUP: u8 = 1 << 3;
    /// Directory we could not open, or an entry the filesystem errored on.
    pub const UNREADABLE: u8 = 1 << 4;
    /// A mount point on another device that we did not descend into.
    pub const OTHER_FS: u8 = 1 << 5;
    /// Synthetic "free space" node, not a real filesystem entry.
    pub const FREE_SPACE: u8 = 1 << 6;
}

#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub parent: NodeId,
    /// First child's `NodeId`; meaningless when `child_len == 0`.
    pub child_start: NodeId,
    pub child_len: u32,
    pub(crate) name_off: u32,
    pub(crate) name_len: u16,
    /// Index into [`crate::kinds::KindTable`].
    pub kind: u16,
    pub flags: u8,
    /// Depth below the scan root, saturating at 255.
    pub depth: u8,
    /// Apparent size: sum of all forks, uncompressed. Subtree total for dirs.
    pub logical: u64,
    /// Blocks actually allocated on disk. Subtree total for dirs.
    pub physical: u64,
}

impl Node {
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.flags & flags::DIR != 0
    }

    #[inline]
    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    /// True when the treemap should stop here: a file, an empty or unreadable
    /// directory, or a bundle while `collapse_packages` is on.
    #[inline]
    pub fn is_leaf(&self, collapse_packages: bool) -> bool {
        self.child_len == 0 || (collapse_packages && self.has(flags::PACKAGE))
    }
}

/// What one scan produced, alongside the tree itself.
#[derive(Clone, Debug, Default)]
pub struct ScanStats {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    /// Entries skipped as duplicate hard links.
    pub hardlink_dups: u64,
    pub errors: u64,
    /// First few errors, for the UI. Capped so a permission-denied sweep of
    /// `/System` cannot grow without bound.
    pub error_samples: Vec<(PathBuf, String)>,
    /// `getattrlistbulk` was unavailable and the POSIX walker was used instead.
    pub used_fallback: bool,
    pub elapsed: std::time::Duration,
    /// Free bytes on the volume holding the scan root, if it could be read.
    pub free_space: Option<u64>,
    /// Total bytes on that volume.
    pub volume_total: Option<u64>,
}

pub struct Tree {
    pub root_path: PathBuf,
    pub stats: ScanStats,
    nodes: Vec<Node>,
    names: Vec<u8>,
}

impl Tree {
    pub(crate) fn new(
        root_path: PathBuf,
        nodes: Vec<Node>,
        names: Vec<u8>,
        stats: ScanStats,
    ) -> Self {
        debug_assert!(!nodes.is_empty(), "a tree always has a root");
        Self {
            root_path,
            stats,
            nodes,
            names,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    #[inline]
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    #[inline]
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    #[inline]
    pub fn children(&self, id: NodeId) -> std::ops::Range<NodeId> {
        let n = self.node(id);
        n.child_start..n.child_start + n.child_len
    }

    /// Raw name bytes. Filesystem names are not guaranteed to be UTF-8, so the
    /// bytes are what we store and lossy conversion happens only for display.
    #[inline]
    pub fn name_bytes(&self, id: NodeId) -> &[u8] {
        let n = self.node(id);
        let off = n.name_off as usize;
        &self.names[off..off + n.name_len as usize]
    }

    pub fn name(&self, id: NodeId) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(self.name_bytes(id))
    }

    /// Rebuilds a node's absolute path by walking to the root. O(depth), no
    /// path strings are stored per node.
    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut parts: Vec<&[u8]> = Vec::with_capacity(self.node(id).depth as usize + 1);
        let mut cur = id;
        while cur != ROOT {
            parts.push(self.name_bytes(cur));
            cur = self.node(cur).parent;
        }
        let mut path = self.root_path.clone();
        for part in parts.iter().rev() {
            path.push(OsStr::from_bytes(part));
        }
        path
    }

    /// Path relative to the scan root, for compact display.
    pub fn display_path(&self, id: NodeId) -> String {
        self.path(id).to_string_lossy().into_owned()
    }

    pub fn size(&self, id: NodeId, physical: bool) -> u64 {
        let n = self.node(id);
        if physical {
            n.physical
        } else {
            n.logical
        }
    }

    /// Ancestors from the root down to `id`, inclusive. Drives the breadcrumb.
    pub fn ancestry(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = vec![id];
        let mut cur = id;
        while cur != ROOT {
            cur = self.node(cur).parent;
            out.push(cur);
        }
        out.reverse();
        out
    }

    /// Appends the synthetic free-space node the UI can show as part of the
    /// volume. Only valid on a tree rooted at a volume mount point.
    pub(crate) fn push_free_space(&mut self, bytes: u64, kind: u16) {
        let name_off = self.names.len() as u32;
        self.names.extend_from_slice(b"(free space)");
        let root = self.nodes[ROOT as usize];
        // The root's children are contiguous, so the new node has to go right
        // after them; that is only true while nothing has been appended since
        // the flatten pass, which is why this is crate-private and one-shot.
        debug_assert_eq!(
            (root.child_start + root.child_len) as usize,
            self.nodes.len(),
            "free space node must extend the root's child range"
        );
        self.nodes.push(Node {
            parent: ROOT,
            child_start: 0,
            child_len: 0,
            name_off,
            name_len: 12,
            kind,
            flags: flags::FREE_SPACE,
            depth: 1,
            logical: bytes,
            physical: bytes,
        });
        let root = &mut self.nodes[ROOT as usize];
        root.child_len += 1;
        root.logical += bytes;
        root.physical += bytes;
    }

    pub(crate) fn set_name(&mut self, id: NodeId, name: &[u8]) {
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        let n = &mut self.nodes[id as usize];
        n.name_off = off;
        n.name_len = name.len().min(u16::MAX as usize) as u16;
    }

    /// Bytes held by the arena itself, for the "how much did this cost" readout.
    pub fn memory_bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<Node>() + self.names.capacity()
    }
}

/// Builder-side constructor, used by the scanner's flatten pass.
pub(crate) fn make_node(
    parent: NodeId,
    name_off: u32,
    name_len: u16,
    flags: u8,
    depth: u8,
    logical: u64,
    physical: u64,
) -> Node {
    Node {
        parent,
        child_start: 0,
        child_len: 0,
        name_off,
        name_len,
        kind: 0,
        flags,
        depth,
        logical,
        physical,
    }
}

/// Where a path sits relative to a tree root, for `--exclude` style matching.
pub fn is_under(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}
