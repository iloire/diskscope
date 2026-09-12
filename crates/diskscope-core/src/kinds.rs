//! File-kind classification and the colour palette, both data-driven.
//!
//! The table is loaded from JSON — `data/kinds.json` is baked into the binary
//! as the default and `~/.config/diskscope/kinds.json` overrides it wholesale,
//! so adding a kind or recolouring one never needs a rebuild. See
//! `data/README.md` for the schema.
//!
//! Lookup is on the hot path (once per file, millions of times), so an
//! extension is packed into a `u128` of lowercased bytes rather than being
//! allocated as a string: no `String`, no UTF-8 validation, one FNV hash.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Longest extension we classify. Anything longer falls through to `Other`,
/// which is fine — 16 bytes covers every real extension.
const MAX_EXT: usize = 16;

pub const KIND_FOLDER: u16 = 0;
pub const KIND_OTHER: u16 = 1;
pub const KIND_FREE: u16 = 2;
/// First index available to kinds defined in the JSON.
const FIRST_USER_KIND: u16 = 3;

#[derive(Clone, Debug)]
pub struct Kind {
    pub name: String,
    pub color: [u8; 3],
}

pub struct KindTable {
    kinds: Vec<Kind>,
    by_ext: HashMap<u128, u16, BuildFnv>,
    package_exts: Vec<u128>,
}

impl KindTable {
    /// The table compiled into the binary.
    pub fn builtin() -> Self {
        const DEFAULT: &str = include_str!("../data/kinds.json");
        Self::from_json(DEFAULT).expect("the bundled kinds.json is valid")
    }

    /// `~/.config/diskscope/kinds.json` if it parses, otherwise the builtin.
    /// A broken override must not stop the app from starting, so the error is
    /// returned alongside a working table rather than propagated.
    pub fn load_or_builtin() -> (Self, Option<String>) {
        let Some(path) = Self::user_path() else {
            return (Self::builtin(), None);
        };
        match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::builtin(), None),
            Err(e) => (Self::builtin(), Some(format!("{}: {e}", path.display()))),
            Ok(text) => match Self::from_json(&text) {
                Ok(t) => (t, None),
                Err(e) => (Self::builtin(), Some(format!("{}: {e}", path.display()))),
            },
        }
    }

    pub fn user_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(Path::new(&home).join(".config/diskscope/kinds.json"))
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        let file: KindsFile = serde_json::from_str(text).map_err(|e| e.to_string())?;

        let mut kinds = vec![
            Kind {
                name: file.folder.name.clone(),
                color: parse_color(&file.folder.color)?,
            },
            Kind {
                name: file.other.name.clone(),
                color: parse_color(&file.other.color)?,
            },
            Kind {
                name: file.free_space.name.clone(),
                color: parse_color(&file.free_space.color)?,
            },
        ];
        debug_assert_eq!(kinds.len(), FIRST_USER_KIND as usize);

        let mut by_ext: HashMap<u128, u16, BuildFnv> = HashMap::default();
        for entry in &file.kinds {
            let id = kinds.len() as u16;
            if id == u16::MAX {
                return Err("too many kinds (max 65534)".into());
            }
            kinds.push(Kind {
                name: entry.name.clone(),
                color: parse_color(&entry.color)?,
            });
            for ext in &entry.extensions {
                let Some(key) = ext_key(ext.as_bytes()) else {
                    return Err(format!(
                        "extension {ext:?} in {:?} is longer than {MAX_EXT} bytes",
                        entry.name
                    ));
                };
                // First definition wins, so the file reads top-to-bottom in
                // priority order and a later kind cannot silently steal an
                // extension from an earlier one.
                by_ext.entry(key).or_insert(id);
            }
        }

        let package_exts = file
            .package_extensions
            .iter()
            .filter_map(|e| ext_key(e.as_bytes()))
            .collect();

        Ok(Self {
            kinds,
            by_ext,
            package_exts,
        })
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    #[inline]
    pub fn kind(&self, id: u16) -> &Kind {
        &self.kinds[id as usize]
    }

    pub fn all(&self) -> &[Kind] {
        &self.kinds
    }

    /// Classifies a filename by extension.
    #[inline]
    pub fn classify(&self, name: &[u8]) -> u16 {
        match ext_key(extension(name)) {
            Some(key) => self.by_ext.get(&key).copied().unwrap_or(KIND_OTHER),
            None => KIND_OTHER,
        }
    }

    /// True for `.app`, `.framework` and friends — directories that should be
    /// presented as a single object.
    #[inline]
    pub fn is_package(&self, name: &[u8]) -> bool {
        match ext_key(extension(name)) {
            // A handful of entries, so a linear scan beats hashing.
            Some(key) => self.package_exts.contains(&key),
            None => false,
        }
    }
}

/// The bytes after the last `.`, or empty when there is no usable extension.
/// A leading dot does not count (`.gitignore` has no extension), matching how
/// the Finder treats dotfiles.
#[inline]
fn extension(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b'.') {
        Some(0) | None => &[],
        Some(i) => &name[i + 1..],
    }
}

/// Packs up to 16 ASCII-lowercased bytes into a `u128`. `None` when the
/// extension is empty or too long.
#[inline]
fn ext_key(ext: &[u8]) -> Option<u128> {
    if ext.is_empty() || ext.len() > MAX_EXT {
        return None;
    }
    let mut key: u128 = 0;
    for (i, &b) in ext.iter().enumerate() {
        key |= (b.to_ascii_lowercase() as u128) << (i * 8);
    }
    Some(key)
}

fn parse_color(s: &str) -> Result<[u8; 3], String> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 {
        return Err(format!("colour {s:?} must be #rrggbb"));
    }
    let byte = |i: usize| {
        u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| format!("colour {s:?} is not hex"))
    };
    Ok([byte(0)?, byte(2)?, byte(4)?])
}

#[derive(Deserialize)]
struct KindsFile {
    folder: NamedColor,
    other: NamedColor,
    free_space: NamedColor,
    #[serde(default)]
    package_extensions: Vec<String>,
    kinds: Vec<KindEntry>,
}

#[derive(Deserialize)]
struct NamedColor {
    name: String,
    color: String,
}

#[derive(Deserialize)]
struct KindEntry {
    name: String,
    color: String,
    extensions: Vec<String>,
}

// --- FNV-1a -----------------------------------------------------------------
// SipHash costs more than the lookup it guards when the key is already an
// integer. Sixteen lines here beats pulling in a hasher crate.

#[derive(Default, Clone, Copy)]
pub struct BuildFnv;

impl std::hash::BuildHasher for BuildFnv {
    type Hasher = Fnv;
    #[inline]
    fn build_hasher(&self) -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

pub struct Fnv(u64);

impl std::hash::Hasher for Fnv {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_split() {
        assert_eq!(extension(b"movie.mp4"), b"mp4");
        assert_eq!(extension(b"archive.tar.gz"), b"gz");
        assert_eq!(extension(b".gitignore"), b"");
        assert_eq!(extension(b"Makefile"), b"");
        assert_eq!(extension(b"weird."), b"");
    }

    #[test]
    fn builtin_table_classifies() {
        let t = KindTable::builtin();
        assert_eq!(t.kind(t.classify(b"a.MP4")).name, "Video");
        assert_eq!(t.kind(t.classify(b"a.rs")).name, "Source code");
        assert_eq!(t.kind(t.classify(b"a.ts")).name, "Web");
        assert_eq!(t.classify(b"Makefile"), KIND_OTHER);
        assert_eq!(t.classify(b"no.suchextension"), KIND_OTHER);
        assert!(t.is_package(b"Xcode.app"));
        assert!(!t.is_package(b"notes.txt"));
    }

    #[test]
    fn ext_key_is_case_insensitive_and_bounded() {
        assert_eq!(ext_key(b"PNG"), ext_key(b"png"));
        assert!(ext_key(b"").is_none());
        assert!(ext_key(&[b'x'; MAX_EXT]).is_some());
        assert!(ext_key(&[b'x'; MAX_EXT + 1]).is_none());
    }

    #[test]
    fn bad_colour_is_an_error() {
        let json = r##"{"folder":{"name":"F","color":"nope"},"other":{"name":"O","color":"#000000"},
                        "free_space":{"name":"S","color":"#000000"},"kinds":[]}"##;
        assert!(KindTable::from_json(json).is_err());
    }
}
