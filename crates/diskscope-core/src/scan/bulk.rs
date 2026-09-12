//! Directory enumeration via `getattrlistbulk(2)`.
//!
//! This is where the speed comes from. The classic way to measure a tree is
//! `readdir` for the names and then one `lstat` per name — two syscalls per
//! file, each re-resolving a path. `getattrlistbulk` instead returns a whole
//! batch of entries *with their metadata already attached*: with a 256 KiB
//! buffer one syscall typically covers a couple of thousand entries, so a
//! directory of 5,000 files costs ~3 syscalls instead of ~5,001.
//!
//! Records are **variable length** and have to be parsed against
//! `ATTR_CMN_RETURNED_ATTRS`, which the kernel puts first in every record.
//! Two details are easy to get wrong and were both found by dumping real
//! replies (see `examples/dump_attrs.rs`):
//!
//! * `ATTR_CMN_ERROR` is returned **immediately after** the returned-attribute
//!   set, not in bit order with the rest of `commonattr`.
//! * the `fileattr` group is simply **absent for directories**, so a fixed
//!   offset for the size fields reads the filename as a size.
//!
//! Not every filesystem implements it — network mounts and some FUSE volumes
//! return `EINVAL` or `ENOTSUP` — so callers fall back to [`super::posix`] per
//! directory. See `tests/scan.rs`, which asserts both walkers agree.

#![allow(non_camel_case_types)]

use std::io;

use libc::{c_int, c_void, size_t};

use super::Ent;

// sys/attr.h. Declared here rather than pulled from libc so the requested set
// and the parsed layout below are visible in one place.
const ATTR_BIT_MAP_COUNT: u16 = 5;

const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_DEVID: u32 = 0x0000_0002;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_FILEID: u32 = 0x0200_0000;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;

const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
const ATTR_FILE_TOTALSIZE: u32 = 0x0000_0002;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;

/// `vnode.h` `enum vtype`.
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

/// 256 KiB holds ~3,000 records, which keeps the syscall count at one or two
/// for all but the most absurd directories while staying comfortably inside a
/// thread's stack-adjacent working set.
const BUF_BYTES: usize = 256 * 1024;

#[repr(C)]
struct Attrlist {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

/// `attribute_set_t`: which attributes a record actually carries.
#[derive(Clone, Copy, Default)]
struct AttributeSet {
    commonattr: u32,
    fileattr: u32,
}

/// Bytes an `attribute_set_t` occupies in the reply (five bitmaps).
const RETURNED_ATTRS_BYTES: usize = 4 * ATTR_BIT_MAP_COUNT as usize;

/// Smallest record we will accept: length word plus the returned set.
const MIN_RECORD: usize = 4 + RETURNED_ATTRS_BYTES;

extern "C" {
    fn getattrlistbulk(
        dirfd: c_int,
        attr_list: *mut c_void,
        attr_buf: *mut c_void,
        attr_buf_size: size_t,
        options: u64,
    ) -> c_int;
}

/// A reusable reply buffer. One per worker thread; allocating 256 KiB per
/// directory would cost more than the syscalls it saves.
pub struct Buffer(Vec<u8>);

impl Buffer {
    pub fn new() -> Self {
        Self(vec![0; BUF_BYTES])
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Enumerates `fd`, calling `push` once per entry. `.` and `..` are not
/// reported by the kernel, so there is nothing to filter.
///
/// The `Ent::name` slice borrows the buffer and is only valid for the duration
/// of the call, which is what keeps the walk allocation-free per file.
pub fn read_dir(fd: c_int, buf: &mut Buffer, mut push: impl FnMut(Ent<'_>)) -> io::Result<()> {
    let mut attrs = Attrlist {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_NAME
            | ATTR_CMN_DEVID
            | ATTR_CMN_OBJTYPE
            | ATTR_CMN_FILEID
            | ATTR_CMN_ERROR,
        volattr: 0,
        dirattr: 0,
        fileattr: ATTR_FILE_LINKCOUNT | ATTR_FILE_TOTALSIZE | ATTR_FILE_ALLOCSIZE,
        forkattr: 0,
    };

    loop {
        let buf_len = buf.0.len();
        // SAFETY: `attrs` and the buffer are both live and correctly sized for
        // the duration of the call; the kernel writes at most `buf_len` bytes.
        let count = unsafe {
            getattrlistbulk(
                fd,
                &mut attrs as *mut Attrlist as *mut c_void,
                buf.0.as_mut_ptr() as *mut c_void,
                buf_len as size_t,
                0,
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count == 0 {
            return Ok(());
        }
        parse_batch(&buf.0, count as usize, &mut push)?;
    }
}

fn parse_batch(buf: &[u8], count: usize, push: &mut impl FnMut(Ent<'_>)) -> io::Result<()> {
    let mut at = 0usize;

    for _ in 0..count {
        if at + MIN_RECORD > buf.len() {
            return Err(malformed("record extends past the reply buffer"));
        }
        let length = read_u32(buf, at)? as usize;
        if length < MIN_RECORD || at + length > buf.len() {
            return Err(malformed("record length outside the reply buffer"));
        }
        // Everything below is read relative to the record, so a bad offset can
        // only ever run off the end of this one record.
        let rec = &buf[at..at + length];
        push(parse_record(rec)?);
        at += length;
    }
    Ok(())
}

/// Walks one record's attributes in the order the kernel emits them.
///
/// Presence is driven entirely by the returned-attribute bitmaps: an attribute
/// whose bit is clear occupies no bytes. Order is bit order within each group,
/// `commonattr` before `fileattr`, with `ATTR_CMN_ERROR` hoisted to the front.
fn parse_record(rec: &[u8]) -> io::Result<Ent<'_>> {
    let returned = AttributeSet {
        commonattr: read_u32(rec, 4)?,
        // volattr at 8 and dirattr at 12 are never requested; forkattr at 20
        // likewise. Only these two groups carry fields we asked for.
        fileattr: read_u32(rec, 16)?,
    };
    let mut at = 4 + RETURNED_ATTRS_BYTES;

    let error = if returned.commonattr & ATTR_CMN_ERROR != 0 {
        take_u32(rec, &mut at)? as i32
    } else {
        0
    };

    if returned.commonattr & ATTR_CMN_NAME == 0 {
        return Err(malformed("reply is missing ATTR_CMN_NAME"));
    }
    let name = take_name(rec, &mut at)?;

    // An entry the filesystem could not describe carries nothing but its name
    // and the error, so there is no point reading further.
    if error != 0 {
        return Ok(Ent {
            name,
            is_dir: false,
            is_symlink: false,
            is_regular: false,
            dev: 0,
            ino: 0,
            nlink: 1,
            logical: 0,
            physical: 0,
            error,
        });
    }

    let dev = if returned.commonattr & ATTR_CMN_DEVID != 0 {
        take_i32(rec, &mut at)? as i64
    } else {
        0
    };
    let objtype = if returned.commonattr & ATTR_CMN_OBJTYPE != 0 {
        take_u32(rec, &mut at)?
    } else {
        0
    };
    let ino = if returned.commonattr & ATTR_CMN_FILEID != 0 {
        take_u64(rec, &mut at)?
    } else {
        0
    };
    let nlink = if returned.fileattr & ATTR_FILE_LINKCOUNT != 0 {
        take_u32(rec, &mut at)?
    } else {
        1
    };
    // `TOTALSIZE` counts every fork and reports the *uncompressed* length for
    // HFS-compressed files; `ALLOCSIZE` is what the volume actually gave up.
    let logical = if returned.fileattr & ATTR_FILE_TOTALSIZE != 0 {
        take_i64(rec, &mut at)?.max(0) as u64
    } else {
        0
    };
    let physical = if returned.fileattr & ATTR_FILE_ALLOCSIZE != 0 {
        take_i64(rec, &mut at)?.max(0) as u64
    } else {
        // A filesystem that reports a length but no allocation is better
        // approximated by the length than by zero.
        logical
    };

    Ok(Ent {
        name,
        is_dir: objtype == VDIR,
        is_symlink: objtype == VLNK,
        is_regular: objtype == VREG,
        dev,
        ino,
        nlink,
        logical,
        physical,
        error: 0,
    })
}

/// Reads an `attrreference_t` and returns the bytes it points at, minus the
/// trailing NUL. The offset is relative to the reference's own position.
fn take_name<'a>(rec: &'a [u8], at: &mut usize) -> io::Result<&'a [u8]> {
    let base = *at as i64;
    let offset = take_i32(rec, at)? as i64;
    let length = take_u32(rec, at)? as usize;
    let start = base + offset;
    let len = length.saturating_sub(1);
    if start < 0 || start as usize + len > rec.len() {
        return Err(malformed("name reference outside the record"));
    }
    Ok(&rec[start as usize..start as usize + len])
}

fn read_u32(rec: &[u8], at: usize) -> io::Result<u32> {
    let bytes = rec
        .get(at..at + 4)
        .ok_or_else(|| malformed("attribute runs past the end of its record"))?;
    Ok(u32::from_ne_bytes(bytes.try_into().unwrap()))
}

fn take_u32(rec: &[u8], at: &mut usize) -> io::Result<u32> {
    let v = read_u32(rec, *at)?;
    *at += 4;
    Ok(v)
}

fn take_i32(rec: &[u8], at: &mut usize) -> io::Result<i32> {
    Ok(take_u32(rec, at)? as i32)
}

/// Attributes are packed on 4-byte boundaries, so 8-byte values are read as
/// two words rather than assuming alignment.
fn take_u64(rec: &[u8], at: &mut usize) -> io::Result<u64> {
    let bytes = rec
        .get(*at..*at + 8)
        .ok_or_else(|| malformed("attribute runs past the end of its record"))?;
    let v = u64::from_ne_bytes(bytes.try_into().unwrap());
    *at += 8;
    Ok(v)
}

fn take_i64(rec: &[u8], at: &mut usize) -> io::Result<i64> {
    Ok(take_u64(rec, at)? as i64)
}

fn malformed(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("getattrlistbulk: {what}"),
    )
}

/// True when the error means "this filesystem cannot do bulk attributes",
/// as opposed to a real failure worth reporting.
pub fn is_unsupported(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EINVAL) | Some(libc::ENOTSUP) | Some(libc::EOPNOTSUPP) | Some(libc::ENOSYS)
    ) || e.kind() == io::ErrorKind::InvalidData
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a record byte-for-byte the way the kernel does, so the parser is
    /// pinned to the real layout rather than to whatever it happens to do.
    fn record(is_dir: bool, name: &str, error: u32) -> Vec<u8> {
        let mut rec = vec![0u8; 4]; // length, filled in at the end
        let common = ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_NAME
            | ATTR_CMN_DEVID
            | ATTR_CMN_OBJTYPE
            | ATTR_CMN_FILEID
            | ATTR_CMN_ERROR;
        let fileattr = if is_dir || error != 0 {
            0
        } else {
            ATTR_FILE_LINKCOUNT | ATTR_FILE_TOTALSIZE | ATTR_FILE_ALLOCSIZE
        };
        rec.extend_from_slice(&common.to_ne_bytes()); // commonattr
        rec.extend_from_slice(&0u32.to_ne_bytes()); // volattr
        rec.extend_from_slice(&0u32.to_ne_bytes()); // dirattr
        rec.extend_from_slice(&fileattr.to_ne_bytes());
        rec.extend_from_slice(&0u32.to_ne_bytes()); // forkattr
        rec.extend_from_slice(&error.to_ne_bytes()); // ERROR, hoisted to front

        let attrref_at = rec.len();
        rec.extend_from_slice(&0i32.to_ne_bytes()); // dataoffset, patched below
        rec.extend_from_slice(&(name.len() as u32 + 1).to_ne_bytes());
        if error == 0 {
            rec.extend_from_slice(&0x0100_000eu32.to_ne_bytes()); // devid
            rec.extend_from_slice(&(if is_dir { VDIR } else { VREG }).to_ne_bytes());
            rec.extend_from_slice(&4242u64.to_ne_bytes()); // fileid
            if !is_dir {
                rec.extend_from_slice(&2u32.to_ne_bytes()); // linkcount
                rec.extend_from_slice(&51_200i64.to_ne_bytes()); // totalsize
                rec.extend_from_slice(&53_248i64.to_ne_bytes()); // allocsize
            }
        }
        let name_at = rec.len();
        rec.extend_from_slice(name.as_bytes());
        rec.push(0);
        while rec.len() % 4 != 0 {
            rec.push(0);
        }
        let offset = (name_at - attrref_at) as i32;
        rec[attrref_at..attrref_at + 4].copy_from_slice(&offset.to_ne_bytes());
        let length = rec.len() as u32;
        rec[0..4].copy_from_slice(&length.to_ne_bytes());
        rec
    }

    /// name, is_dir, logical, physical, nlink, errno
    type Parsed = (String, bool, u64, u64, u32, i32);

    fn parse_all(buf: &[u8], count: usize) -> io::Result<Vec<Parsed>> {
        let mut out = Vec::new();
        parse_batch(buf, count, &mut |e: Ent<'_>| {
            out.push((
                String::from_utf8_lossy(e.name).into_owned(),
                e.is_dir,
                e.logical,
                e.physical,
                e.nlink,
                e.error,
            ));
        })?;
        Ok(out)
    }

    #[test]
    fn parses_a_regular_file_record() {
        let got = parse_all(&record(false, "big.bin", 0), 1).unwrap();
        assert_eq!(got, vec![("big.bin".into(), false, 51_200, 53_248, 2, 0)]);
    }

    #[test]
    fn a_directory_record_has_no_size_fields() {
        // The bug this pins: reading sizes at a fixed offset here would have
        // picked up the filename bytes.
        let got = parse_all(&record(true, "subdir", 0), 1).unwrap();
        assert_eq!(got, vec![("subdir".into(), true, 0, 0, 1, 0)]);
    }

    #[test]
    fn an_errored_entry_keeps_only_its_name() {
        let got = parse_all(&record(false, "denied", libc::EACCES as u32), 1).unwrap();
        assert_eq!(got, vec![("denied".into(), false, 0, 0, 1, libc::EACCES)]);
    }

    #[test]
    fn a_batch_of_mixed_records_parses_in_order() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&record(true, "a", 0));
        buf.extend_from_slice(&record(false, "a-much-longer-filename-here.dat", 0));
        buf.extend_from_slice(&record(false, "bb.txt", 0));
        let got = parse_all(&buf, 3).unwrap();
        let names: Vec<&str> = got.iter().map(|g| g.0.as_str()).collect();
        assert_eq!(names, ["a", "a-much-longer-filename-here.dat", "bb.txt"]);
        assert_eq!(got[1].2, 51_200);
    }

    #[test]
    fn truncated_reply_is_rejected_not_read_past() {
        let buf = [0u8; 8];
        let err = parse_batch(&buf, 1, &mut |_| panic!("should not yield")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            is_unsupported(&err),
            "a malformed reply should send us to the fallback"
        );
    }

    #[test]
    fn a_name_pointing_outside_its_record_is_rejected() {
        let mut rec = record(false, "ok.txt", 0);
        // Push the name reference far past the end of the record.
        let attrref_at = 4 + RETURNED_ATTRS_BYTES + 4;
        rec[attrref_at..attrref_at + 4].copy_from_slice(&9999i32.to_ne_bytes());
        let err = parse_batch(&rec, 1, &mut |_| panic!("should not yield")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
