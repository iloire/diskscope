//! Timing harness: `cargo run --release --example bench -- <path> [runs]`
//!
//! Reports the bulk walker against the POSIX walker on the same tree, which is
//! the honest comparison — it isolates the syscall strategy from everything
//! else (same parsing, same arena, same thread pool). Off macOS there is only
//! the one walker, so it is timed alone and no ratio is printed.

use std::path::Path;
use std::time::Instant;

use diskscope_core::{fmt, kinds::KindTable, scan, ScanOptions};

/// The walkers worth timing here. `getattrlistbulk` is a Darwin syscall, so
/// everywhere else there is nothing to compare `posix` against — timing it
/// twice would just print "1.00x faster".
const WALKERS: &[(&str, bool)] = if cfg!(target_os = "macos") {
    &[("getattrlistbulk", true), ("readdir + fstatat", false)]
} else {
    &[("readdir + fstatat", false)]
};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| ".".into());
    let runs: usize = args.next().and_then(|r| r.parse().ok()).unwrap_or(3);
    let kinds = KindTable::builtin();

    println!("{path}  ({runs} runs each, best of)\n");
    let mut best = Vec::new();
    for &(label, prefer_bulk) in WALKERS {
        let mut fastest = f64::INFINITY;
        let mut summary = String::new();
        for _ in 0..runs {
            let progress = scan::Progress::default();
            let opts = ScanOptions {
                prefer_bulk,
                include_free_space: false,
                ..Default::default()
            };
            let started = Instant::now();
            let tree = scan::scan(Path::new(&path), &opts, &kinds, &progress).expect("scan");
            let secs = started.elapsed().as_secs_f64();
            fastest = fastest.min(secs);
            summary = format!(
                "{} entries, {} ({} files, {} dirs, {} errors{})",
                fmt::count(tree.len() as u64),
                fmt::bytes(tree.node(diskscope_core::ROOT).physical),
                fmt::count(tree.stats.files),
                fmt::count(tree.stats.dirs),
                fmt::count(tree.stats.errors),
                if tree.stats.used_fallback {
                    ", fell back"
                } else {
                    ""
                },
            );
            let per_sec = tree.len() as f64 / secs;
            println!(
                "  {label:<18} {secs:>7.3}s  {:>12} entries/s",
                fmt::count(per_sec as u64)
            );
        }
        println!("  {summary}\n");
        best.push((label, fastest));
    }

    if let [(_, bulk), (_, posix)] = best[..] {
        println!("bulk is {:.2}x faster than readdir + fstatat", posix / bulk);
    }
}
