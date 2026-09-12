//! The treemap canvas.
//!
//! The map is rendered on the CPU into a texture rather than submitted as
//! geometry, and re-rendered only when something it depends on changes — the
//! folder in view, the window size, the size metric, the highlighted kind.
//! Panning and zooming do not exist by design (drilling into a folder is the
//! only navigation, as in Disk Inventory X), so a steady window means a steady
//! texture and the frame cost drops to one blit.
//!
//! Hit-testing reads the id buffer the rasteriser filled, so pointing at a
//! 2×3 pixel block is exact and costs one array index.

use diskscope_core::raster::{rasterize, Raster, RasterOptions};
use diskscope_core::treemap::{layout, Layout, LayoutOptions, Rect as TRect};
use diskscope_core::{KindTable, NodeId, Tree};
use egui::{Color32, Pos2, Rect, Stroke, StrokeKind, TextureHandle, TextureOptions};
use rayon::prelude::*;

use crate::theme::color;

/// Everything the rendered map depends on. When this is unchanged, so is the
/// texture.
#[derive(Clone, PartialEq)]
struct CacheKey {
    root: NodeId,
    width: u32,
    height: u32,
    physical: bool,
    collapse_packages: bool,
    highlight: Option<u16>,
    /// Bumped when a new tree is loaded, since node ids mean something else.
    generation: u64,
}

pub struct Canvas {
    texture: Option<TextureHandle>,
    raster: Option<Raster>,
    layout: Option<Layout>,
    key: Option<CacheKey>,
    /// Device pixels per point at the time of the last render.
    scale: f32,
    /// Wall-clock cost of the last layout + rasterise, shown in the status bar.
    pub last_render_ms: f32,
}

impl Default for Canvas {
    fn default() -> Self {
        Self {
            texture: None,
            raster: None,
            layout: None,
            key: None,
            // Never zero, so the point/pixel conversion in `draw_marks` is
            // safe even if it somehow runs before the first render.
            scale: 1.0,
            last_render_ms: 0.0,
        }
    }
}

pub struct CanvasInput<'a> {
    pub tree: &'a Tree,
    pub kinds: &'a KindTable,
    pub root: NodeId,
    pub physical: bool,
    pub collapse_packages: bool,
    pub highlight: Option<u16>,
    pub generation: u64,
    pub selected: Option<NodeId>,
    pub min_area: f32,
}

/// What the pointer did over the map this frame.
#[derive(Default)]
pub struct CanvasOutput {
    pub hovered: Option<NodeId>,
    pub clicked: Option<NodeId>,
    pub double_clicked: Option<NodeId>,
    pub context_target: Option<NodeId>,
}

impl Canvas {
    /// Drops cached state so the next frame re-renders from scratch.
    pub fn invalidate(&mut self) {
        self.key = None;
    }

    pub fn block_count(&self) -> usize {
        self.layout.as_ref().map_or(0, |l| l.block_count())
    }

    pub fn show(&mut self, ui: &mut egui::Ui, input: &CanvasInput<'_>) -> CanvasOutput {
        let size = ui.available_size();
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        ui.painter().rect_filled(rect, 0.0, color::CHART);

        let scale = ui.ctx().pixels_per_point();
        let width = (rect.width() * scale).round().max(1.0) as u32;
        let height = (rect.height() * scale).round().max(1.0) as u32;

        let key = CacheKey {
            root: input.root,
            width,
            height,
            physical: input.physical,
            collapse_packages: input.collapse_packages,
            highlight: input.highlight,
            generation: input.generation,
        };
        if self.key.as_ref() != Some(&key) {
            self.render(ui, input, &key, scale);
            self.key = Some(key);
        }

        let Some(texture) = &self.texture else {
            return CanvasOutput::default();
        };
        ui.painter().image(
            texture.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );

        let mut out = CanvasOutput::default();
        if let (Some(pos), Some(raster)) = (response.hover_pos(), &self.raster) {
            let local = pos - rect.min;
            out.hovered = raster.node_at((local.x * scale) as i32, (local.y * scale) as i32);
        }
        if response.clicked() {
            out.clicked = out.hovered;
        }
        if response.double_clicked() {
            out.double_clicked = out.hovered;
        }
        if response.secondary_clicked() {
            out.context_target = out.hovered;
        }

        self.draw_marks(ui, rect, input, out.hovered);
        out
    }

    /// Selection and hover marks are egui shapes on top of the texture, so
    /// moving the pointer never costs a re-render.
    fn draw_marks(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        input: &CanvasInput<'_>,
        hovered: Option<NodeId>,
    ) {
        let Some(layout) = &self.layout else { return };
        let painter = ui.painter_at(rect);
        let to_screen = |r: TRect| {
            Rect::from_min_size(
                rect.min + egui::vec2(r.x / self.scale, r.y / self.scale),
                egui::vec2(r.w / self.scale, r.h / self.scale),
            )
        };

        // The folder the pointer is inside gets a quiet outline: it answers
        // "which directory am I looking at" without a click.
        if let Some(node) = hovered {
            let parent = input.tree.node(node).parent;
            if let Some(r) = layout.dir_rect(parent) {
                painter.rect_stroke(
                    to_screen(r).expand(1.0),
                    0.0,
                    Stroke::new(1.0, color::SOUNDING.gamma_multiply(0.7)),
                    StrokeKind::Outside,
                );
            }
            if let Some(r) = find_rect(layout, node) {
                painter.rect_stroke(
                    to_screen(r),
                    0.0,
                    Stroke::new(1.0, color::PAPER),
                    StrokeKind::Inside,
                );
            }
        }

        // Selection is a double stroke — white inside, magenta outside — so it
        // stays visible whatever colour the block underneath happens to be.
        if let Some(node) = input.selected {
            if let Some(r) = find_rect(layout, node) {
                let screen = to_screen(r);
                painter.rect_stroke(
                    screen,
                    0.0,
                    Stroke::new(1.0, color::PAPER),
                    StrokeKind::Inside,
                );
                painter.rect_stroke(
                    screen.expand(1.5),
                    0.0,
                    Stroke::new(2.0, color::MAGENTA),
                    StrokeKind::Outside,
                );
            }
        }
    }

    fn render(&mut self, ui: &egui::Ui, input: &CanvasInput<'_>, key: &CacheKey, scale: f32) {
        let started = std::time::Instant::now();
        let view = TRect::new(0.0, 0.0, key.width as f32, key.height as f32);
        let layout_opts = LayoutOptions {
            // Thresholds are in device pixels, so a Retina display gets the
            // same physical detail rather than four times as many blocks.
            min_area: input.min_area * scale * scale,
            padding: scale.round().max(1.0),
            collapse_packages: input.collapse_packages,
            physical: input.physical,
            ..Default::default()
        };
        let laid_out = layout(input.tree, input.root, view, &layout_opts);

        let raster_opts = RasterOptions {
            highlight_kind: input.highlight,
            background: [color::CHART.r(), color::CHART.g(), color::CHART.b()],
            ..Default::default()
        };
        let image = rasterize(&laid_out, input.tree, input.kinds, &raster_opts);

        // NEAREST because the texture is drawn at exactly 1:1; filtering would
        // only soften the one-pixel gutters that carry the directory structure.
        let pixels: Vec<Color32> = image
            .rgba
            .par_chunks_exact(4)
            .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
            .collect();
        let color_image =
            egui::ColorImage::new([image.width as usize, image.height as usize], pixels);
        match &mut self.texture {
            Some(handle) => handle.set(color_image, TextureOptions::NEAREST),
            None => {
                self.texture = Some(ui.ctx().load_texture(
                    "treemap",
                    color_image,
                    TextureOptions::NEAREST,
                ))
            }
        }

        self.raster = Some(image);
        self.layout = Some(laid_out);
        self.scale = scale;
        self.last_render_ms = started.elapsed().as_secs_f32() * 1000.0;
    }
}

/// Rectangle of a node whether it was subdivided or drawn as one block.
///
/// The leaf lookup is a linear scan, which is fine because it only runs when
/// the hovered or selected node actually changes, not per frame.
fn find_rect(layout: &Layout, node: NodeId) -> Option<TRect> {
    layout
        .leaves
        .iter()
        .find(|c| c.node == node)
        .map(|c| c.rect)
        .or_else(|| layout.dir_rect(node))
}
