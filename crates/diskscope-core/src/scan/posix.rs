//! Portable `readdir` + `fstatat` directory enumeration.
//!
//! On macOS this is the slow path, kept for two reasons: filesystems that
//! reject `getattrlistbulk` (network shares, some FUSE volumes), and as the
//! reference implementation the bulk reader is tested against —
//! `tests/scan.rs` asserts the two produce identical trees there.
//!
//! Everywhere else it is the only walker: `getattrlistbulk` is Darwin-only.

use std::ffi::CStr;
use std::io;

use libc::c_int;

use super::Ent;

/// This thread's errno slot. glibc and macOS libc spell the accessor
/// differently; there is no portable name for it.
#[cfg(target_os = "macos")]
#[inline]
fn errno_slot() -> *mut c_int {
    // SAFETY: returns this thread's errno slot, valid for the thread's life.
    unsafe { libc::__error() }
}

#[cfg(not(target_os = "macos"))]
#[inline]
fn errno_slot() -> *mut c_int {
    // SAFETY: returns this thread's errno slot, valid for the thread's life.
    unsafe { libc::__errno_location() }
}

/// Enumerates `fd` with one `fstatat` per entry. `.` and `..` are skipped.
///
/// `fd` stays owned by the caller: it is duplicated before `fdopendir`, which
/// otherwise takes ownership and would close it on `closedir`.
pub fn read_dir(fd: c_int, mut push: impl FnMut(Ent<'_>)) -> io::Result<()> {
    // SAFETY: `fd` is a live directory descriptor owned by the caller.
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `dup` is a fresh descriptor; `closedir` below consumes it, and
    // the early return closes it by hand.
    let dir = unsafe { libc::fdopendir(dup) };
    if dir.is_null() {
        let e = io::Error::last_os_error();
        unsafe { libc::close(dup) };
        return Err(e);
    }

    let mut result = Ok(());
    loop {
        // `readdir` returns NULL for both end-of-directory and failure, told
        // apart by errno — and it does not clear errno itself, so it has to be
        // zeroed first. Skipping this made every directory inherit the errno
        // of whatever failed last (usually an `fstatat` on a protected file)
        // and report tens of thousands of errors that had not happened.
        unsafe { *errno_slot() = 0 };
        // SAFETY: `dir` is a live DIR*. `readdir` returns a pointer into
        // storage owned by `dir`, valid until the next `readdir`/`closedir`.
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            let e = io::Error::last_os_error();
            if e.raw_os_error().is_some_and(|code| code != 0) {
                result = Err(e);
            }
            break;
        }
        // SAFETY: non-null, and `dirent` is the layout `readdir` fills in.
        let d = unsafe { &*entry };
        // Measured with `strlen` rather than read from a length field: macOS
        // has `d_namlen` but Linux does not, and this is the slow path anyway.
        // SAFETY: `d_name` is NUL-terminated within the `dirent`.
        let name = unsafe { CStr::from_ptr(d.d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }

        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `d_name` is NUL-terminated, `fd` is the directory it came
        // from, and `st` is a valid out-param.
        let rc =
            unsafe { libc::fstatat(fd, d.d_name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
        if rc != 0 {
            let code = io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO);
            push(Ent {
                name,
                is_dir: false,
                is_symlink: false,
                is_regular: false,
                dev: 0,
                ino: d.d_ino,
                nlink: 1,
                logical: 0,
                physical: 0,
                error: code,
            });
            continue;
        }

        let mode = st.st_mode & libc::S_IFMT;
        push(Ent {
            name,
            is_dir: mode == libc::S_IFDIR,
            is_symlink: mode == libc::S_IFLNK,
            is_regular: mode == libc::S_IFREG,
            dev: st.st_dev as i64,
            ino: st.st_ino,
            nlink: st.st_nlink as u32,
            logical: st.st_size.max(0) as u64,
            // `st_blocks` is always in 512-byte units regardless of the
            // volume's block size. This misses resource forks, which
            // `ATTR_FILE_ALLOCSIZE` would include — the one respect in which
            // the fallback is less accurate than the bulk path.
            physical: (st.st_blocks.max(0) as u64) * 512,
            error: 0,
        });
    }

    // SAFETY: `dir` is live and not used again; this also closes `dup`.
    unsafe { libc::closedir(dir) };
    result
}
