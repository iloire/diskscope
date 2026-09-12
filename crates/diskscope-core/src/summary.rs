//! Per-kind and per-file rollups for whatever subtree is on screen.
//!
//! Both are recomputed when the view changes rather than cached per node: a
//! whole-tree pass is a linear scan of a flat vector (a few milliseconds for a
//! million entries) and a drilled-in pass only touches that subtree, so the
//! caching would cost more memory than it saves time.

use rayon::prelude::*;

use crate::tree::{flags, NodeId, Tree, ROOT};

#[derive(Clone, Copy, Debug, Default)]
pub struct KindTotal {
    pub kind: u16,
    pub logical: u64,
    pub physical: u64,
    pub files: u64,
}

/// Totals per file kind within `root`'s subtree, largest first.
///
/// Directories contribute nothing of their own — only the files inside them —
/// so the totals add up to the subtree size without double counting.
pub fn kind_totals(tree: &Tree, root: NodeId, kind_count: usize, physical: bool) -> Vec<KindTotal> {
    let mut totals = vec![KindTotal::default(); kind_count];
    for (i, t) in totals.iter_mut().enumerate() {
        t.kind = i as u16;
    }

    let mut add = |n: &crate::tree::Node| {
        if n.is_dir() {
            return;
        }
        let slot = &mut totals[n.kind as usize];
        slot.logical += n.logical;
        slot.physical += n.physical;
        slot.files += 1;
    };

    if root == ROOT {
        // The whole tree is the whole vector, so this is one linear pass.
        for n in tree.nodes() {
            add(n);
        }
    } else {
        for_each_in_subtree(tree, root, |id| add(tree.node(id)));
    }

    totals.retain(|t| t.files > 0);
    let key = |t: &KindTotal| if physical { t.physical } else { t.logical };
    totals.sort_unstable_by(|a, b| key(b).cmp(&key(a)).then(b.files.cmp(&a.files)));
    totals
}

/// The `n` largest files under `root`, largest first.
///
/// Keeps a bounded insertion-sorted list rather than sorting the subtree: for
/// the `n` we care about (tens) that is a couple of comparisons per file.
pub fn largest_files(tree: &Tree, root: NodeId, n: usize, physical: bool) -> Vec<NodeId> {
    if n == 0 {
        return Vec::new();
    }
    let mut top: Vec<(u64, NodeId)> = Vec::with_capacity(n + 1);
    let mut floor = 0u64;

    let mut consider = |id: NodeId| {
        let node = tree.node(id);
        if node.is_dir() || node.has(flags::FREE_SPACE) {
            return;
        }
        let size = if physical {
            node.physical
        } else {
            node.logical
        };
        if size == 0 || (top.len() == n && size <= floor) {
            return;
        }
        let at = top.partition_point(|(s, _)| *s > size);
        top.insert(at, (size, id));
        top.truncate(n);
        if top.len() == n {
            floor = top[n - 1].0;
        }
    };

    if root == ROOT {
        for id in 0..tree.len() as NodeId {
            consider(id);
        }
    } else {
        for_each_in_subtree(tree, root, &mut consider);
    }
    top.into_iter().map(|(_, id)| id).collect()
}

/// Depth-first walk of `root` and everything under it, `root` included.
pub fn for_each_in_subtree(tree: &Tree, root: NodeId, mut visit: impl FnMut(NodeId)) {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        visit(id);
        let kids = tree.children(id);
        stack.extend(kids);
    }
}

/// Number of entries under `root`, inclusive. Parallel for the whole tree.
pub fn subtree_len(tree: &Tree, root: NodeId) -> usize {
    if root == ROOT {
        return tree.len();
    }
    let mut n = 0;
    for_each_in_subtree(tree, root, |_| n += 1);
    n
}

/// Directories under `root` sorted by size, for the folder pane. Only the
/// immediate children, since the pane expands lazily.
pub fn children_by_size(tree: &Tree, parent: NodeId, physical: bool) -> Vec<NodeId> {
    let mut kids: Vec<NodeId> = tree.children(parent).collect();
    kids.sort_unstable_by_key(|&c| std::cmp::Reverse(tree.size(c, physical)));
    kids
}

/// Total bytes accounted for by each kind across the whole tree, in parallel.
/// Used for the one-shot summary the CLI prints.
pub fn kind_totals_parallel(tree: &Tree, kind_count: usize) -> Vec<KindTotal> {
    tree.nodes()
        .par_iter()
        .fold(
            || vec![KindTotal::default(); kind_count],
            |mut acc, n| {
                if !n.is_dir() {
                    let slot = &mut acc[n.kind as usize];
                    slot.logical += n.logical;
                    slot.physical += n.physical;
                    slot.files += 1;
                }
                acc
            },
        )
        .reduce(
            || vec![KindTotal::default(); kind_count],
            |mut a, b| {
                for (x, y) in a.iter_mut().zip(b) {
                    x.logical += y.logical;
                    x.physical += y.physical;
                    x.files += y.files;
                }
                a
            },
        )
        .into_iter()
        .enumerate()
        .map(|(i, mut t)| {
            t.kind = i as u16;
            t
        })
        .filter(|t| t.files > 0)
        .collect()
}
