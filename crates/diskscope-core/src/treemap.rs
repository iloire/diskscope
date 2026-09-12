//! Squarified treemap layout with cushion surfaces.
//!
//! Two ideas, both from the SequoiaView lineage that Disk Inventory X drew on:
//!
//! * **Squarified** layout (Bruls, Huizing & van Wijk, 2000) picks row breaks
//!   that keep every block as close to square as it can, so a 4 KB file in a
//!   corner is still a clickable blob instead of a hairline.
//! * **Cushions** (van Wijk & van de Wetering, 1999) shade each block as a lit
//!   parabolic bump, with the bumps nesting by directory. That shading is what
//!   makes directory structure legible without drawing a single border, and it
//!   is computed here as four coefficients per block and applied per pixel in
//!   [`crate::raster`].
//!
//! # Why this stays fast on a 2 M file tree
//!
//! Layout never visits the whole tree. A subtree whose rectangle would be
//! smaller than `min_area` pixels is emitted as one aggregate block and not
//! descended into, so the work is bounded by the number of blocks the screen
//! can actually show — a few tens of thousands — no matter how big the scan
//! was. Blocks tile their parent exactly (positions accumulate rather than
//! being recomputed), which is what lets the rasteriser treat them as disjoint.

use std::collections::HashMap;

use crate::tree::{flags, NodeId, Tree};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    #[inline]
    pub fn area(&self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    #[inline]
    pub fn inset(&self, by: f32) -> Rect {
        Rect {
            x: self.x + by,
            y: self.y + by,
            w: self.w - 2.0 * by,
            h: self.h - 2.0 * by,
        }
    }

    #[inline]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// One drawable block.
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub node: NodeId,
    pub rect: Rect,
    /// Cushion coefficients `[sx1, sx2, sy1, sy2]`, in image pixel space.
    pub cushion: [f32; 4],
    pub depth: u16,
}

#[derive(Clone, Debug)]
pub struct LayoutOptions {
    /// Subtrees whose rectangle is smaller than this many pixels are drawn as
    /// a single aggregate block. This is the knob that decouples layout cost
    /// from tree size.
    pub min_area: f32,
    /// Gap left around a directory's children. Directory structure reads as
    /// gutters of background rather than drawn borders, and it is also what
    /// guarantees sibling blocks never touch.
    pub padding: f32,
    /// Draw `.app` and friends as one block.
    pub collapse_packages: bool,
    /// Size by blocks on disk rather than apparent size.
    pub physical: bool,
    /// Cushion amplitude at the root.
    pub cushion_height: f32,
    /// Per-level amplitude decay; below 1.0 deeper nesting shades more gently.
    pub cushion_falloff: f32,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            min_area: 8.0,
            padding: 1.0,
            collapse_packages: true,
            physical: true,
            // Tuned by eye against real trees: any stronger and the
            // highlight washes a large block's own colour out to pale grey,
            // which costs more legibility than the extra relief buys.
            cushion_height: 0.42,
            cushion_falloff: 0.82,
        }
    }
}

pub struct Layout {
    pub root: NodeId,
    pub view: Rect,
    /// Blocks to draw, in no particular order. Pairwise disjoint.
    pub leaves: Vec<Cell>,
    /// Directories that were subdivided, with the rectangle their children
    /// were placed into. Used for hover outlines and drill-down targets.
    pub dirs: Vec<Cell>,
    dir_index: HashMap<NodeId, u32>,
    pub physical: bool,
}

impl Layout {
    /// The rectangle a subdivided directory's children occupy.
    pub fn dir_rect(&self, node: NodeId) -> Option<Rect> {
        self.dir_index
            .get(&node)
            .map(|&i| self.dirs[i as usize].rect)
    }

    /// Rectangle of any laid-out node, whether it was subdivided or drawn as
    /// a single block.
    pub fn rect_of(&self, node: NodeId) -> Option<Rect> {
        if let Some(r) = self.dir_rect(node) {
            return Some(r);
        }
        self.leaves.iter().find(|c| c.node == node).map(|c| c.rect)
    }

    pub fn block_count(&self) -> usize {
        self.leaves.len()
    }
}

/// Lays `root`'s subtree out into `view`.
pub fn layout(tree: &Tree, root: NodeId, view: Rect, opts: &LayoutOptions) -> Layout {
    let mut out = Layout {
        root,
        view,
        // A full screen at min_area 8 tops out around 250 k blocks; reserving
        // for a typical window avoids a dozen reallocations mid-layout.
        leaves: Vec::with_capacity(8192),
        dirs: Vec::with_capacity(1024),
        dir_index: HashMap::new(),
        physical: opts.physical,
    };
    if view.w > 0.0 && view.h > 0.0 && !tree.is_empty() {
        place(tree, root, view, [0.0; 4], 0, opts, &mut out);
    }
    for (i, cell) in out.dirs.iter().enumerate() {
        out.dir_index.insert(cell.node, i as u32);
    }
    out
}

fn place(
    tree: &Tree,
    node: NodeId,
    rect: Rect,
    cushion: [f32; 4],
    depth: u16,
    opts: &LayoutOptions,
    out: &mut Layout,
) {
    let n = tree.node(node);
    let subdividable = n.child_len > 0
        && !(opts.collapse_packages && n.has(flags::PACKAGE))
        && rect.area() >= opts.min_area
        && rect.w > 2.0 * opts.padding + 1.0
        && rect.h > 2.0 * opts.padding + 1.0;

    if !subdividable {
        out.leaves.push(Cell {
            node,
            rect,
            cushion,
            depth,
        });
        return;
    }

    let inner = rect.inset(opts.padding);

    // Zero-sized entries would get zero-area rectangles, so they are dropped
    // rather than laid out; the sidebar still accounts for them.
    let mut items: Vec<(NodeId, u64)> = tree
        .children(node)
        .filter_map(|c| {
            let size = tree.size(c, opts.physical);
            (size > 0).then_some((c, size))
        })
        .collect();
    if items.is_empty() {
        out.leaves.push(Cell {
            node,
            rect,
            cushion,
            depth,
        });
        return;
    }
    items.sort_unstable_by_key(|item| std::cmp::Reverse(item.1));
    let total: u64 = items.iter().map(|(_, s)| *s).sum();

    let mut placed: Vec<(NodeId, Rect)> = Vec::with_capacity(items.len());
    squarify(&items, total, inner, &mut placed);

    out.dirs.push(Cell {
        node,
        rect: inner,
        cushion,
        depth,
    });

    let h = opts.cushion_height * opts.cushion_falloff.powi(depth as i32);
    for (child, crect) in placed {
        let (sx1, sx2) = add_ridge(crect.x, crect.x + crect.w, h, cushion[0], cushion[1]);
        let (sy1, sy2) = add_ridge(crect.y, crect.y + crect.h, h, cushion[2], cushion[3]);
        place(
            tree,
            child,
            crect,
            [sx1, sx2, sy1, sy2],
            depth + 1,
            opts,
            out,
        );
    }
}

/// Superimposes one parabolic ridge on a cushion surface along one axis.
///
/// From van Wijk & van de Wetering: the surface is `z = s1*x + s2*x²` per
/// axis, and nesting a child means adding the parabola that peaks in the
/// middle of the child's span and falls to zero at its edges.
#[inline]
fn add_ridge(a: f32, b: f32, h: f32, s1: f32, s2: f32) -> (f32, f32) {
    let span = b - a;
    if span <= 0.0 {
        return (s1, s2);
    }
    (s1 + 4.0 * h * (b + a) / span, s2 - 4.0 * h / span)
}

/// Places `items` (descending by size, summing to `total`) inside `rect`.
///
/// Each pass picks the longest prefix whose worst aspect ratio still improves,
/// lays it as a strip across the shorter side, and recurses on what is left.
/// Extents accumulate so that a block's far edge is bit-identical to its
/// neighbour's near edge — the property the rasteriser relies on.
fn squarify(items: &[(NodeId, u64)], total: u64, rect: Rect, out: &mut Vec<(NodeId, Rect)>) {
    let mut rect = rect;
    let mut items = items;
    // Sizes still to place, in the tree's own units. Keeping this in size
    // units rather than pixels is what makes the area budget exact: each pass
    // recomputes pixels-per-unit from the rectangle it actually has left.
    let mut remaining = total;

    while !items.is_empty() {
        if rect.w <= 0.0 || rect.h <= 0.0 || remaining == 0 {
            break;
        }
        let side = rect.w.min(rect.h) as f64;
        if side <= 0.0 {
            break;
        }
        let area_per_unit = (rect.w as f64 * rect.h as f64) / remaining as f64;

        // Longest prefix whose worst aspect ratio keeps improving. The largest
        // block in the strip is the first, since `items` is sorted descending.
        let largest = items[0].1 as f64 * area_per_unit;
        let mut strip_size = 0u64;
        let mut count = 0usize;
        let mut best = f64::INFINITY;
        for (i, item) in items.iter().enumerate() {
            let candidate = strip_size + item.1;
            let thickness = candidate as f64 * area_per_unit / side;
            let t2 = (thickness * thickness).max(f64::MIN_POSITIVE);
            let smallest = (item.1 as f64 * area_per_unit).max(f64::MIN_POSITIVE);
            // Either the widest block gets flattened, or the narrowest gets
            // stretched; the worse of the two is what we minimise.
            let worst = (t2 / smallest).max(largest / t2);
            if i == 0 || worst <= best {
                best = worst;
                strip_size = candidate;
                count = i + 1;
            } else {
                break;
            }
        }

        let vertical = rect.w >= rect.h;
        let longer = if vertical { rect.w } else { rect.h };
        // For the final strip this comes out exactly equal to `longer`; the
        // clamp only guards float drift.
        let thickness = ((strip_size as f64 * area_per_unit / side) as f32).min(longer);
        let strip = &items[..count];

        if vertical {
            let mut cursor = rect.y;
            let end = rect.y + rect.h;
            for (k, item) in strip.iter().enumerate() {
                // The last block takes the remainder, so the strip closes
                // exactly on its rectangle instead of drifting.
                let extent = if k + 1 == count {
                    end - cursor
                } else {
                    (item.1 as f64 * area_per_unit / thickness as f64) as f32
                };
                out.push((
                    item.0,
                    Rect {
                        x: rect.x,
                        y: cursor,
                        w: thickness,
                        h: extent,
                    },
                ));
                cursor += extent;
            }
            rect.x += thickness;
            rect.w -= thickness;
        } else {
            let mut cursor = rect.x;
            let end = rect.x + rect.w;
            for (k, item) in strip.iter().enumerate() {
                let extent = if k + 1 == count {
                    end - cursor
                } else {
                    (item.1 as f64 * area_per_unit / thickness as f64) as f32
                };
                out.push((
                    item.0,
                    Rect {
                        x: cursor,
                        y: rect.y,
                        w: extent,
                        h: thickness,
                    },
                ));
                cursor += extent;
            }
            rect.y += thickness;
            rect.h -= thickness;
        }

        remaining -= strip_size;
        items = &items[count..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes(n: &[u64]) -> Vec<(NodeId, u64)> {
        n.iter()
            .enumerate()
            .map(|(i, &s)| (i as NodeId, s))
            .collect()
    }

    #[test]
    fn strip_fills_its_rectangle_exactly() {
        let items = sizes(&[50, 30, 12, 5, 2, 1]);
        let total: u64 = items.iter().map(|i| i.1).sum();
        let view = Rect::new(0.0, 0.0, 400.0, 300.0);
        let mut out = Vec::new();
        squarify(&items, total, view, &mut out);

        assert_eq!(out.len(), items.len());
        let covered: f32 = out.iter().map(|(_, r)| r.area()).sum();
        assert!(
            (covered - view.area()).abs() < 1.0,
            "blocks cover {covered} of {}",
            view.area()
        );
        for (_, r) in &out {
            assert!(
                r.x >= view.x - 0.01 && r.y >= view.y - 0.01,
                "{r:?} escapes left/top"
            );
            assert!(
                r.x + r.w <= view.x + view.w + 0.01 && r.y + r.h <= view.y + view.h + 0.01,
                "{r:?} escapes right/bottom"
            );
        }
    }

    #[test]
    fn blocks_do_not_overlap() {
        // Deterministic pseudo-random sizes; overlapping blocks would break the
        // rasteriser's disjointness assumption.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let raw: Vec<u64> = (0..200)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state % 10_000 + 1
            })
            .collect();
        let mut items = sizes(&raw);
        items.sort_unstable_by_key(|item| std::cmp::Reverse(item.1));
        let total: u64 = items.iter().map(|i| i.1).sum();
        let mut out = Vec::new();
        squarify(&items, total, Rect::new(0.0, 0.0, 1200.0, 800.0), &mut out);

        for (i, (_, a)) in out.iter().enumerate() {
            for (_, b) in &out[i + 1..] {
                let overlap_x = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
                let overlap_y = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
                assert!(
                    overlap_x <= 0.01 || overlap_y <= 0.01,
                    "{a:?} overlaps {b:?} by {overlap_x}x{overlap_y}"
                );
            }
        }
    }

    #[test]
    fn areas_are_proportional_to_size() {
        let items = sizes(&[600, 300, 100]);
        let mut out = Vec::new();
        squarify(&items, 1000, Rect::new(0.0, 0.0, 1000.0, 1000.0), &mut out);
        let by_node: std::collections::HashMap<_, _> =
            out.iter().map(|(n, r)| (*n, r.area())).collect();
        // 1 M px over 1000 units, so 1000 px per unit of size.
        for (node, size) in items {
            let expected = size as f32 * 1000.0;
            let got = by_node[&node];
            assert!(
                (got - expected).abs() / expected < 0.02,
                "node {node}: {got} px for size {size}, wanted ~{expected}"
            );
        }
    }

    #[test]
    fn ridge_is_symmetric_about_the_span() {
        let (s1, s2) = add_ridge(10.0, 30.0, 0.5, 0.0, 0.0);
        // Slope of z = s1*x + s2*x^2 is zero at the midpoint of the span.
        let midpoint = -s1 / (2.0 * s2);
        assert!(
            (midpoint - 20.0).abs() < 1e-3,
            "peak at {midpoint}, wanted 20"
        );
    }

    #[test]
    fn degenerate_ridge_is_ignored() {
        assert_eq!(add_ridge(5.0, 5.0, 0.5, 1.0, -2.0), (1.0, -2.0));
    }
}
