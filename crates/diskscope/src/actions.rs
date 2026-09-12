//! Things the app does to files, and the volumes it can be pointed at.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Selects the item in a new Finder window.
pub fn reveal_in_finder(path: &Path) -> io::Result<()> {
    run("open", &["-R".as_ref(), path.as_os_str()])
}

/// Opens the item with whatever app owns it.
pub fn open_path(path: &Path) -> io::Result<()> {
    run("open", &[path.as_os_str()])
}

/// Moves the item to the Trash, recoverably — never an unlink. The caller is
/// responsible for confirming with the user first.
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|e| e.to_string())
}

fn run(program: &str, args: &[&std::ffi::OsStr]) -> io::Result<()> {
    let status = Command::new(program).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{program} exited with {status}")))
    }
}

/// Mounted volumes plus the obvious starting points, in the order a person
/// would look for them.
pub fn scan_targets() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        let label = home
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Home".into());
        out.push((label, home));
    }
    out.push(("Macintosh HD".into(), PathBuf::from("/")));

    // `/Volumes` holds external and network mounts, plus a symlink back to the
    // boot volume that would just duplicate the entry above.
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        let mut mounts: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        mounts.sort_by(|a, b| a.0.cmp(&b.0));
        out.extend(mounts);
    }
    out
}

/// Opens a folder picker. `None` when the user cancels.
pub fn pick_folder(start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("Choose a folder to scan");
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder()
}
