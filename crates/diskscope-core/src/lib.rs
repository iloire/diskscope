//! Disk usage scanning and cushion-treemap rendering.
//!
//! The engine behind `diskscope`, split out so it can be benchmarked and
//! scripted without a window. Nothing here touches a GUI toolkit.
//!
//! ```no_run
//! use diskscope_core::{kinds::KindTable, scan, treemap, raster};
//! use std::path::Path;
//!
//! let kinds = KindTable::builtin();
//! let progress = scan::Progress::default();
//! let tree = scan::scan(Path::new("/Users"), &Default::default(), &kinds, &progress)?;
//!
//! let view = treemap::Rect::new(0.0, 0.0, 1200.0, 800.0);
//! let layout = treemap::layout(&tree, diskscope_core::tree::ROOT, view, &Default::default());
//! let image = raster::rasterize(&layout, &tree, &kinds, &Default::default());
//! # Ok::<(), std::io::Error>(())
//! ```

pub mod fmt;
pub mod kinds;
pub mod raster;
pub mod scan;
pub mod summary;
pub mod tree;
pub mod treemap;

pub use kinds::KindTable;
pub use scan::{scan, Progress, ScanOptions};
pub use tree::{Node, NodeId, Tree, ROOT};
