//! `diskscope` — where the disk went.
//!
//! With no arguments, or with a path, this opens the window. `--text` prints
//! the same breakdown to the terminal instead, which is what the benchmarks
//! and any scripting go through.

mod actions;
mod app;
mod canvas;
mod job;
mod sidebar;
mod theme;

use std::path::PathBuf;
use std::process::ExitCode;

use diskscope_core::summary::{kind_totals, largest_files};
use diskscope_core::{fmt, scan, KindTable, Progress, ScanOptions, ROOT};

/// Stamped by `build.rs`. Absent when built without a git checkout, in which
/// case nothing is shown rather than a placeholder.
pub const COMMIT: Option<&str> = option_env!("DISKSCOPE_COMMIT");

fn version() -> String {
    match COMMIT {
        Some(sha) => format!("diskscope {} ({sha})", env!("CARGO_PKG_VERSION")),
        None => format!("diskscope {}", env!("CARGO_PKG_VERSION")),
    }
}

const USAGE: &str = "\
diskscope — where the disk went

USAGE
    diskscope [PATH]              open the window, optionally scanning PATH
    diskscope --text PATH         print a breakdown instead of opening a window

OPTIONS
    --text                  print to the terminal and exit
    --apparent              size by apparent length rather than blocks on disk
    --all-volumes           descend into other volumes mounted inside the tree
    --keep-hard-links       count every name of a hard-linked file
    --no-bulk               use the portable readdir walker, not getattrlistbulk
    --depth N               stop descending N levels below the root
    --top N                 how many largest files --text lists (default 15)
    --screenshot FILE.bmp   scan, photograph the window into FILE, then quit
    -V, --version           print the version and the commit it was built from
    -h, --help              this
";

fn main() -> ExitCode {
    let mut path: Option<PathBuf> = None;
    let mut text_mode = false;
    let mut physical = true;
    let mut top = 15usize;
    let mut opts = ScanOptions::default();
    let mut shot: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("{}", version());
                return ExitCode::SUCCESS;
            }
            "--text" => text_mode = true,
            "--apparent" => physical = false,
            "--all-volumes" => opts.cross_mounts = true,
            "--keep-hard-links" => opts.dedupe_hardlinks = false,
            "--no-bulk" => opts.prefer_bulk = false,
            "--depth" => match args.next().and_then(|v| v.parse::<u8>().ok()) {
                Some(n) => opts.max_depth = n,
                None => return fail("--depth needs a number from 0 to 255"),
            },
            "--top" => match args.next().and_then(|v| v.parse::<usize>().ok()) {
                Some(n) => top = n,
                None => return fail("--top needs a number"),
            },
            "--screenshot" => match args.next() {
                Some(file) => shot = Some(PathBuf::from(file)),
                None => return fail("--screenshot needs a file to write"),
            },
            other if other.starts_with('-') => {
                return fail(&format!("unknown option {other}. Try --help."))
            }
            other => path = Some(PathBuf::from(other)),
        }
    }

    if text_mode {
        let Some(path) = path else {
            return fail("--text needs a path to scan");
        };
        return match print_report(&path, &opts, physical, top) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&format!("{}: {e}", path.display())),
        };
    }

    if shot.is_some() && path.is_none() {
        return fail("--screenshot needs a path to scan");
    }

    match run_window(path, shot) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e.to_string()),
    }
}

fn run_window(start: Option<PathBuf>, shot: Option<PathBuf>) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("Diskscope"),
        ..Default::default()
    };
    eframe::run_native(
        "Diskscope",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(&cc.egui_ctx, start, shot)))),
    )
}

fn print_report(
    path: &std::path::Path,
    opts: &ScanOptions,
    physical: bool,
    top: usize,
) -> std::io::Result<()> {
    let kinds = KindTable::builtin();
    let progress = Progress::default();
    let tree = scan(path, opts, &kinds, &progress)?;
    let stats = &tree.stats;
    let total = tree.size(ROOT, physical);

    println!("{}", tree.root_path.display());
    println!(
        "  {}  ·  {} entries ({} files, {} folders)  ·  {:.2}s",
        fmt::bytes(total),
        fmt::count(tree.len() as u64),
        fmt::count(stats.files),
        fmt::count(stats.dirs),
        stats.elapsed.as_secs_f64(),
    );
    if let (Some(free), Some(volume)) = (stats.free_space, stats.volume_total) {
        println!(
            "  volume: {} of {} free",
            fmt::bytes(free),
            fmt::bytes(volume)
        );
    }
    if stats.hardlink_dups > 0 {
        println!(
            "  {} extra hard links not counted",
            fmt::count(stats.hardlink_dups)
        );
    }
    if stats.used_fallback {
        println!("  fell back to the portable walker on at least one directory");
    }

    println!("\nBY KIND");
    for t in kind_totals(&tree, ROOT, kinds.len(), physical) {
        let size = if physical { t.physical } else { t.logical };
        println!(
            "  {:<16} {:>10} {:>5}  {:>14}",
            kinds.kind(t.kind).name,
            fmt::bytes(size),
            fmt::percent(size, total),
            fmt::files(t.files),
        );
    }

    if top > 0 {
        println!("\nLARGEST FILES");
        for id in largest_files(&tree, ROOT, top, physical) {
            println!(
                "  {:>10}  {}",
                fmt::bytes(tree.size(id, physical)),
                tree.display_path(id)
            );
        }
    }

    if stats.errors > 0 {
        println!("\n{} items could not be read:", fmt::count(stats.errors));
        for (path, message) in stats.error_samples.iter().take(5) {
            println!("  {}: {message}", path.display());
        }
        if stats.errors as usize > 5 {
            println!("  …");
        }
        println!("Grant Full Disk Access in System Settings > Privacy & Security to include them.");
    }
    Ok(())
}

fn fail(message: &str) -> ExitCode {
    eprintln!("diskscope: {message}");
    ExitCode::FAILURE
}
