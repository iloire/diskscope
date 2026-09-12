//! Software rasteriser for cushion treemaps.
//!
//! The treemap is drawn into an RGBA image on the CPU rather than handed to the
//! GPU as geometry, for three reasons: cushion shading is per-pixel anyway, the
//! image only changes when the view changes (so it is uploaded once and blitted
//! every frame), and it lets us fill a parallel **id buffer** — one `NodeId`
//! per pixel — which turns hit-testing into an array index instead of a
//! spatial query.
//!
//! Parallelism is by horizontal band, each holding a `&mut` slice of the
//! image, with block indices pre-bucketed per band. No `unsafe`, no atomics,
//! and deterministic output regardless of thread count.

use rayon::prelude::*;

use crate::kinds::KindTable;
use crate::tree::{flags, NodeId, Tree};
use crate::treemap::{Layout, Rect};

/// Value in the id buffer meaning "background".
pub const NO_NODE: NodeId = NodeId::MAX;

/// Light direction from van Wijk & van de Wetering: slightly up and to the
/// left, which is the convention that makes bumps read as bumps.
const LIGHT: [f32; 3] = [0.09759, 0.19518, 0.9759];

#[derive(Clone, Debug)]
pub struct RasterOptions {
    /// Flat light. Set high enough that a block in shadow still shows its own
    /// colour rather than going to near-black.
    pub ambient: f32,
    /// Directional contribution on top of the ambient term.
    pub specular: f32,
    /// When set, blocks of other kinds are greyed back.
    pub highlight_kind: Option<u16>,
    /// Brightness left to non-matching blocks while a kind is highlighted.
    pub dim: f32,
    pub background: [u8; 3],
}

impl Default for RasterOptions {
    fn default() -> Self {
        Self {
            ambient: 0.56,
            specular: 0.48,
            highlight_kind: None,
            dim: 0.32,
            background: [14, 16, 20],
        }
    }
}

pub struct Raster {
    pub width: u32,
    pub height: u32,
    /// Straight (non-premultiplied) RGBA, ready for texture upload.
    pub rgba: Vec<u8>,
    /// One node per pixel; `NO_NODE` for background.
    pub ids: Vec<NodeId>,
}

impl Raster {
    /// Which node covers a pixel. `None` for background or out of bounds.
    pub fn node_at(&self, x: i32, y: i32) -> Option<NodeId> {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return None;
        }
        let id = self.ids[y as usize * self.width as usize + x as usize];
        (id != NO_NODE).then_some(id)
    }
}

/// Renders `layout` into a fresh image the size of its view rectangle.
pub fn rasterize(layout: &Layout, tree: &Tree, kinds: &KindTable, opts: &RasterOptions) -> Raster {
    let width = layout.view.w.ceil().max(1.0) as usize;
    let height = layout.view.h.ceil().max(1.0) as usize;

    let mut rgba = vec![0u8; width * height * 4];
    for px in rgba.chunks_exact_mut(4) {
        px[0] = opts.background[0];
        px[1] = opts.background[1];
        px[2] = opts.background[2];
        px[3] = 255;
    }
    let mut ids = vec![NO_NODE; width * height];

    // Bands of ~24 rows keep every core busy on a small window without making
    // the per-band bookkeeping dominate on a large one.
    let rows_per_band = 24.min(height).max(1);
    let bands = height.div_ceil(rows_per_band);

    let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); bands];
    let mut bounds: Vec<[u32; 4]> = Vec::with_capacity(layout.leaves.len());
    for (i, cell) in layout.leaves.iter().enumerate() {
        let b = pixel_bounds(cell.rect, width, height);
        bounds.push(b);
        let [x0, y0, x1, y1] = b;
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let first = y0 as usize / rows_per_band;
        let last = (y1 as usize - 1) / rows_per_band;
        for band in &mut buckets[first..=last] {
            band.push(i as u32);
        }
    }

    let colors = block_colors(layout, tree, kinds, opts);

    rgba.par_chunks_mut(rows_per_band * width * 4)
        .zip(ids.par_chunks_mut(rows_per_band * width))
        .zip(buckets.par_iter())
        .enumerate()
        .for_each(|(band, ((rgba_band, ids_band), bucket))| {
            let band_y0 = band * rows_per_band;
            let band_rows = rgba_band.len() / (width * 4);
            for &i in bucket {
                let cell = &layout.leaves[i as usize];
                shade_block(
                    rgba_band,
                    ids_band,
                    width,
                    band_y0,
                    band_rows,
                    bounds[i as usize],
                    cell.cushion,
                    colors[i as usize],
                    cell.node,
                    opts,
                );
            }
        });

    Raster {
        width: width as u32,
        height: height as u32,
        rgba,
        ids,
    }
}

/// Base colour per block, resolved once so the pixel loop stays arithmetic.
fn block_colors(
    layout: &Layout,
    tree: &Tree,
    kinds: &KindTable,
    opts: &RasterOptions,
) -> Vec<[f32; 3]> {
    layout
        .leaves
        .iter()
        .map(|cell| {
            let node = tree.node(cell.node);
            let mut c = kinds.kind(node.kind).color.map(|v| v as f32);
            // Unreadable and skipped entries are structural information, not a
            // file kind, so they get their own flat greys.
            if node.has(flags::UNREADABLE) {
                c = [92.0, 64.0, 64.0];
            } else if node.has(flags::OTHER_FS) {
                c = [70.0, 78.0, 92.0];
            }
            match opts.highlight_kind {
                Some(k) if k != node.kind => {
                    let grey = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
                    // Mix most of the way to grey, then darken, so the
                    // highlighted kind is the only saturated thing on screen.
                    [
                        (c[0] * 0.25 + grey * 0.75) * opts.dim,
                        (c[1] * 0.25 + grey * 0.75) * opts.dim,
                        (c[2] * 0.25 + grey * 0.75) * opts.dim,
                    ]
                }
                _ => c,
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn shade_block(
    rgba: &mut [u8],
    ids: &mut [NodeId],
    width: usize,
    band_y0: usize,
    band_rows: usize,
    bounds: [u32; 4],
    cushion: [f32; 4],
    color: [f32; 3],
    node: NodeId,
    opts: &RasterOptions,
) {
    let [x0, y0, x1, y1] = bounds.map(|v| v as usize);
    let y_start = y0.max(band_y0);
    let y_end = y1.min(band_y0 + band_rows);
    if x1 <= x0 || y_end <= y_start {
        return;
    }
    let [sx1, sx2, sy1, sy2] = cushion;

    for y in y_start..y_end {
        let fy = y as f32 + 0.5;
        let ny = -(2.0 * sy2 * fy + sy1);
        let row = (y - band_y0) * width;
        for x in x0..x1 {
            let fx = x as f32 + 0.5;
            let nx = -(2.0 * sx2 * fx + sx1);
            let cos_a =
                (nx * LIGHT[0] + ny * LIGHT[1] + LIGHT[2]) / (nx * nx + ny * ny + 1.0).sqrt();
            let intensity = opts.ambient + opts.specular * cos_a.max(0.0);
            let px = (row + x) * 4;
            rgba[px] = (color[0] * intensity).min(255.0) as u8;
            rgba[px + 1] = (color[1] * intensity).min(255.0) as u8;
            rgba[px + 2] = (color[2] * intensity).min(255.0) as u8;
            rgba[px + 3] = 255;
            ids[row + x] = node;
        }
    }
}

/// Snaps a rectangle to pixels. Both edges round the same way, so a block's
/// far edge lands exactly on its neighbour's near edge and the results tile
/// without gaps or overlap.
fn pixel_bounds(r: Rect, width: usize, height: usize) -> [u32; 4] {
    let clamp_x = |v: f32| v.round().clamp(0.0, width as f32) as u32;
    let clamp_y = |v: f32| v.round().clamp(0.0, height as f32) as u32;
    [
        clamp_x(r.x),
        clamp_y(r.y),
        clamp_x(r.x + r.w),
        clamp_y(r.y + r.h),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_rectangles_tile_without_gap_or_overlap() {
        let a = Rect::new(0.0, 0.0, 10.3, 5.0);
        let b = Rect::new(10.3, 0.0, 7.2, 5.0);
        let [_, _, a_x1, _] = pixel_bounds(a, 100, 100);
        let [b_x0, ..] = pixel_bounds(b, 100, 100);
        assert_eq!(a_x1, b_x0);
    }

    #[test]
    fn bounds_are_clamped_to_the_image() {
        assert_eq!(
            pixel_bounds(Rect::new(-5.0, -5.0, 200.0, 200.0), 40, 30),
            [0, 0, 40, 30]
        );
    }
}
