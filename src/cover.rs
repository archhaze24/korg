//! Step 1: normalize album covers to a clean `cover.jpg`.
//!
//! Big covers (side over 2000 px or file over 1.5 MiB) are resized and re-encoded as a
//! progressive JPEG at quality 90. Small ones only lose their metadata segments; their
//! pixel data is never recompressed.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use image::imageops::{self, FilterType};
use image::{ImageDecoder, ImageFormat, ImageReader};

use crate::fsutil::atomic_write;

/// File names (compared case-insensitively) treated as an album cover.
pub const COVER_NAMES: [&str; 4] = ["cover.jpg", "cover.jpeg", "front.jpg", "folder.jpg"];
pub const TARGET_NAME: &str = "cover.jpg";

const MAX_SIDE: u32 = 2000;
const MAX_BYTES: usize = 3 * 1024 * 1024 / 2;
const QUALITY: u8 = 90;

pub fn is_cover_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    COVER_NAMES.contains(&lower.as_str())
}

/// Cover candidates inside `folder` (not recursive).
pub fn candidates(folder: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && is_cover_name(&entry.file_name().to_string_lossy()) {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

#[derive(Debug, PartialEq, Eq)]
pub struct CoverOutcome {
    pub target: PathBuf,
    pub before: usize,
    pub after: usize,
    /// True when the file on disk was (or, in a dry run, would be) rewritten or renamed.
    pub changed: bool,
}

/// Cleans one cover and stores it as `cover.jpg` next to the original.
pub fn process_cover(path: &Path, dry_run: bool) -> Result<CoverOutcome> {
    let target = path.with_file_name(TARGET_NAME);
    let raw = fs::read(path)?;
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    if reader.format() != Some(ImageFormat::Jpeg) {
        bail!("Expected JPEG: {}", path.display());
    }
    let mut decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    let cleaned = if width.max(height) > MAX_SIDE || raw.len() > MAX_BYTES {
        let orientation = decoder.orientation()?;
        let mut img = image::DynamicImage::from_decoder(decoder)?;
        img.apply_orientation(orientation);
        recompress(img.to_rgb8())?
    } else {
        strip_jpeg_metadata(&raw)?
    };
    let renamed = path.file_name().is_none_or(|n| n != TARGET_NAME);
    let changed = cleaned != raw || renamed;
    if changed && !dry_run {
        atomic_write(&target, &cleaned)?;
        if renamed {
            if same_file::is_same_file(path, &target).unwrap_or(false) {
                // Case-insensitive filesystem: `Cover.JPG` and `cover.jpg` are one file, but
                // the entry may keep its old spelling. Go through a temporary name to fix it.
                if !entry_exists(&target) {
                    let staging = path.with_file_name(format!(".korg-case-{}", uuid::Uuid::new_v4().simple()));
                    fs::rename(path, &staging)?;
                    fs::rename(&staging, &target)?;
                }
            } else if path.exists() {
                // A separate file (another name, or a case-sensitive filesystem).
                fs::remove_file(path)?;
            }
        }
    }
    Ok(CoverOutcome { target, before: raw.len(), after: cleaned.len(), changed })
}

/// True when the directory listing contains exactly this file name (case-sensitive check).
fn entry_exists(path: &Path) -> bool {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return false;
    };
    fs::read_dir(parent).is_ok_and(|mut dir| dir.any(|e| e.is_ok_and(|e| e.file_name() == name)))
}

fn recompress(rgb: image::RgbImage) -> Result<Vec<u8>> {
    let (width, height) = rgb.dimensions();
    let longest = width.max(height);
    let rgb = if longest > MAX_SIDE {
        let scale = f64::from(MAX_SIDE) / f64::from(longest);
        // Truncate like the original pipeline, but never go below one pixel.
        let new_w = ((f64::from(width) * scale) as u32).max(1);
        let new_h = ((f64::from(height) * scale) as u32).max(1);
        imageops::resize(&rgb, new_w, new_h, FilterType::Lanczos3)
    } else {
        rgb
    };
    let (width, height) = rgb.dimensions();
    let mut out = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut out, QUALITY);
    encoder.set_progressive(true);
    encoder.encode(rgb.as_raw(), u16::try_from(width)?, u16::try_from(height)?, jpeg_encoder::ColorType::Rgb)?;
    Ok(out)
}

/// Drops APP1..APP15 and COM segments without touching the coded image data.
///
/// APP0 (JFIF) stays, and so does Adobe APP14, which CMYK/YCCK images need to be
/// decoded correctly. Every scan is walked, so progressive files work too.
pub fn strip_jpeg_metadata(data: &[u8]) -> Result<Vec<u8>> {
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        bail!("Not a JPEG");
    }
    let mut out = data[..2].to_vec();
    let mut pos = 2;
    while pos < data.len() {
        let start = pos;
        if data[pos] != 0xFF {
            bail!("Invalid JPEG marker");
        }
        while pos < data.len() && data[pos] == 0xFF {
            pos += 1;
        }
        let Some(&marker) = data.get(pos) else {
            bail!("Invalid JPEG marker");
        };
        pos += 1;
        if marker == 0xD9 {
            out.extend_from_slice(&data[start..pos]);
            return Ok(out);
        }
        let length = match data.get(pos..pos + 2) {
            Some(bytes) => usize::from(u16::from_be_bytes([bytes[0], bytes[1]])),
            None => 0,
        };
        if length < 2 || pos + length > data.len() {
            bail!("Invalid JPEG segment");
        }
        let end = pos + length;
        let metadata = ((0xE1..=0xEF).contains(&marker) || marker == 0xFE) && marker != 0xEE;
        if !metadata {
            out.extend_from_slice(&data[start..end]);
        }
        pos = end;
        if marker == 0xDA {
            // Entropy-coded data runs until the next real marker (not a stuffed
            // 0xFF00 or a restart marker).
            let scan_start = pos;
            while pos + 1 < data.len() {
                if data[pos] == 0xFF && data[pos + 1] != 0 && !(0xD0..=0xD7).contains(&data[pos + 1]) {
                    break;
                }
                pos += if data[pos] == 0xFF { 2 } else { 1 };
            }
            out.extend_from_slice(&data[scan_start..pos]);
        }
    }
    bail!("Missing JPEG end marker")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([50, 90, 170]));
        JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img).unwrap();
        out
    }

    /// Inserts an APP1 (EXIF-like) and a COM segment right after SOI.
    fn with_metadata(data: &[u8]) -> Vec<u8> {
        let mut out = data[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1, 0x00, 0x08, b'E', b'x', b'i', b'f', 0, 0]);
        out.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x05, b'h', b'i', b'!']);
        out.extend_from_slice(&data[2..]);
        out
    }

    #[test]
    fn strip_removes_metadata_and_is_idempotent() {
        let clean = jpeg(71, 43);
        let dirty = with_metadata(&clean);
        assert_ne!(clean, dirty);
        let stripped = strip_jpeg_metadata(&dirty).unwrap();
        assert_eq!(stripped, strip_jpeg_metadata(&clean).unwrap());
        assert_eq!(stripped, strip_jpeg_metadata(&stripped).unwrap());
        let before = image::load_from_memory(&dirty).unwrap();
        let after = image::load_from_memory(&stripped).unwrap();
        assert_eq!(before.as_bytes(), after.as_bytes());
    }

    #[test]
    fn strip_handles_progressive_scans() {
        let mut out = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut out, 80);
        encoder.set_progressive(true);
        let pixels: Vec<u8> = (0..64 * 48 * 3).map(|i| (i * 7 % 251) as u8).collect();
        encoder.encode(&pixels, 64, 48, jpeg_encoder::ColorType::Rgb).unwrap();
        let dirty = with_metadata(&out);
        let stripped = strip_jpeg_metadata(&dirty).unwrap();
        assert_eq!(stripped, out);
        assert!(image::load_from_memory(&stripped).is_ok());
    }

    #[test]
    fn strip_rejects_garbage() {
        assert!(strip_jpeg_metadata(b"nope").is_err());
        let mut truncated = jpeg(10, 10);
        truncated.truncate(truncated.len() - 2);
        assert!(strip_jpeg_metadata(&truncated).is_err());
    }

    #[test]
    fn large_cover_is_resized_and_second_pass_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cover.jpg");
        fs::write(&path, jpeg(3000, 1500)).unwrap();
        let first = process_cover(&path, false).unwrap();
        assert!(first.changed);
        let size = image::image_dimensions(&path).unwrap();
        assert_eq!(size, (2000, 1000));
        let bytes = fs::read(&path).unwrap();
        let second = process_cover(&path, false).unwrap();
        assert!(!second.changed);
        assert_eq!(bytes, fs::read(&path).unwrap());
    }

    #[test]
    fn small_cover_keeps_pixels_and_loses_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cover.jpg");
        let clean = jpeg(200, 100);
        fs::write(&path, with_metadata(&clean)).unwrap();
        assert!(process_cover(&path, false).unwrap().changed);
        assert_eq!(fs::read(&path).unwrap(), strip_jpeg_metadata(&clean).unwrap());
    }

    #[test]
    fn dry_run_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Front.jpg");
        fs::write(&path, jpeg(100, 100)).unwrap();
        let outcome = process_cover(&path, true).unwrap();
        assert!(outcome.changed);
        assert!(path.exists());
        assert!(!dir.path().join("cover.jpg").exists());
    }

    #[test]
    fn renaming_leaves_no_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cover.JPG");
        fs::write(&path, jpeg(100, 100)).unwrap();
        process_cover(&path, false).unwrap();
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("cover.jpg")]);
        assert_eq!(candidates(dir.path()).unwrap().len(), 1);
    }

    #[test]
    fn non_jpeg_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cover.jpg");
        fs::write(&path, b"\x89PNG\r\n\x1a\n not really a png").unwrap();
        assert!(process_cover(&path, false).is_err());
    }

    #[test]
    fn name_matching_ignores_case() {
        for name in ["cover.jpg", "COVER.JPG", "Cover.jpeg", "Front.jpg", "folder.JPG"] {
            assert!(is_cover_name(name), "{name}");
        }
        assert!(!is_cover_name("back.jpg"));
        assert!(!is_cover_name("cover.png"));
    }
}
