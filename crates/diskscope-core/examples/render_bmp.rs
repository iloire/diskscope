//! Renders a treemap straight to a BMP, with no window involved:
//! `cargo run --release --example render_bmp -- <path> <out.bmp> [w] [h]`
//!
//! Used to eyeball the map — shading, proportions, gutters — without a GUI in
//! the way, and to diff renders after a change to the layout or rasteriser.

use std::io::Write;
use std::path::Path;

use diskscope_core::kinds::KindTable;
use diskscope_core::raster::{rasterize, RasterOptions};
use diskscope_core::treemap::{layout, LayoutOptions, Rect};
use diskscope_core::{scan, Progress, ScanOptions, ROOT};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: render_bmp <path> <out.bmp> [w] [h]");
    let out = args.next().unwrap_or_else(|| "treemap.bmp".into());
    let w: u32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(1400);
    let h: u32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(900);

    let kinds = KindTable::builtin();
    let progress = Progress::default();
    let opts = ScanOptions::default();
    let tree = scan(Path::new(&path), &opts, &kinds, &progress).expect("scan");

    let started = std::time::Instant::now();
    let lay = layout(
        &tree,
        ROOT,
        Rect::new(0.0, 0.0, w as f32, h as f32),
        &LayoutOptions {
            min_area: 9.0,
            padding: 1.0,
            cushion_height: env("H", LayoutOptions::default().cushion_height),
            cushion_falloff: env("F", LayoutOptions::default().cushion_falloff),
            ..Default::default()
        },
    );
    let img = rasterize(
        &lay,
        &tree,
        &kinds,
        &RasterOptions {
            background: [0x0d, 0x13, 0x18],
            ambient: env("AMBIENT", RasterOptions::default().ambient),
            specular: env("SPECULAR", RasterOptions::default().specular),
            ..Default::default()
        },
    );
    eprintln!(
        "{} entries -> {} blocks, laid out and shaded in {:.1} ms",
        tree.len(),
        lay.block_count(),
        started.elapsed().as_secs_f64() * 1000.0
    );

    write_bmp(Path::new(&out), img.width, img.height, &img.rgba).expect("write bmp");
    eprintln!("wrote {out}");
}

/// 24-bit uncompressed BMP: bottom-up rows of BGR, each padded to 4 bytes.
/// Verbose but dependency-free, and `sips` converts it to PNG.
fn write_bmp(path: &Path, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    let row_bytes = (width * 3).next_multiple_of(4) as usize;
    let pixel_bytes = row_bytes * height as usize;
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);

    file.write_all(b"BM")?;
    file.write_all(&(54 + pixel_bytes as u32).to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?;
    file.write_all(&54u32.to_le_bytes())?;
    file.write_all(&40u32.to_le_bytes())?;
    file.write_all(&(width as i32).to_le_bytes())?;
    file.write_all(&(height as i32).to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&24u16.to_le_bytes())?;
    for value in [0u32, pixel_bytes as u32, 0, 0, 0, 0] {
        file.write_all(&value.to_le_bytes())?;
    }

    let mut row = vec![0u8; row_bytes];
    for y in (0..height as usize).rev() {
        for x in 0..width as usize {
            let src = (y * width as usize + x) * 4;
            row[x * 3] = rgba[src + 2];
            row[x * 3 + 1] = rgba[src + 1];
            row[x * 3 + 2] = rgba[src];
        }
        file.write_all(&row)?;
    }
    file.flush()
}

/// Lets the shading constants be swept from the shell while comparing renders.
fn env(name: &str, fallback: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(fallback)
}
