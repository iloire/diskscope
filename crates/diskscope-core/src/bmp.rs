//! Minimal BMP writer, so a render can be saved without pulling in an image
//! encoder. `sips -s format png` turns the result into something a README can
//! show.

use std::io::Write;
use std::path::Path;

/// Writes 24-bit uncompressed BMP from straight RGBA input.
///
/// BMP stores rows bottom-up as BGR, each padded to a 4-byte boundary — hence
/// all the shuffling for what is otherwise a header and a memcpy.
pub fn write_rgba(path: &Path, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    assert_eq!(
        rgba.len(),
        width as usize * height as usize * 4,
        "rgba buffer does not match {width}x{height}"
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_payload_are_the_right_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.bmp");
        // 3 px wide: 9 bytes of pixel data padded to 12 per row.
        write_rgba(&path, 3, 2, &[0u8; 3 * 2 * 4]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..2], b"BM");
        assert_eq!(bytes.len(), 54 + 12 * 2);
        assert_eq!(
            u32::from_le_bytes(bytes[2..6].try_into().unwrap()) as usize,
            bytes.len()
        );
    }

    #[test]
    fn pixels_are_written_bottom_up_as_bgr() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.bmp");
        // One red pixel on the top row, one blue on the bottom.
        let rgba = [255, 0, 0, 255, 0, 0, 255, 255];
        write_rgba(&path, 1, 2, &rgba).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        // First row in the file is the image's last row: blue, as BGR.
        assert_eq!(&bytes[54..57], &[255, 0, 0]);
        assert_eq!(&bytes[58..61], &[0, 0, 255]);
    }
}
