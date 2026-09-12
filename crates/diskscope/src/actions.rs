//! Things the app does to files, and the volumes it can be pointed at.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Shows the item in the desktop's file manager.
///
/// macOS `open -R` selects the item itself. Linux has no portable equivalent —
/// the freedesktop way is a D-Bus call carrying a percent-encoded URI, which is
/// more machinery than this earns — so there we open the containing folder and
/// leave nothing selected.
pub fn reveal_in_file_manager(path: &Path) -> io::Result<()> {
    if cfg!(target_os = "macos") {
        run("open", &["-R".as_ref(), path.as_os_str()])
    } else {
        let folder = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        run("xdg-open", &[folder.as_os_str()])
    }
}

/// Opens the item with whatever app owns it.
pub fn open_path(path: &Path) -> io::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    run(opener, &[path.as_os_str()])
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
    let root_label = if cfg!(target_os = "macos") {
        "Macintosh HD"
    } else {
        "Filesystem"
    };
    out.push((root_label.into(), PathBuf::from("/")));

    // Each root holds external and network mounts. Symlinks are skipped
    // (`file_type` does not follow them), which on macOS drops the link
    // `/Volumes` keeps back to the boot volume — already the entry above.
    for dir in mount_roots() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
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

/// Directories the desktop mounts volumes into. macOS gathers them all under
/// `/Volumes`; Linux spreads them across a few conventional roots, of which
/// `/run/media/$USER` is what udisks2 uses.
fn mount_roots() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        return vec![PathBuf::from("/Volumes")];
    }
    let mut roots = vec![PathBuf::from("/media"), PathBuf::from("/mnt")];
    if let Some(user) = std::env::var_os("USER") {
        roots.push(Path::new("/run/media").join(user));
    }
    roots
}

/// Opens a folder picker. `None` when the user cancels.
pub fn pick_folder(start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("Choose a folder to scan");
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder()
}
