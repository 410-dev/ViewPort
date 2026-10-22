//! Thumbnail generation and storage.
//!
//! Thumbnails live in a few large append-only pack files inside the bundle instead of
//! one small file each, which keeps copying and syncing a bundle fast. The location of
//! each thumbnail (pack, offset, length) is recorded in the library database.

mod pack;

pub use pack::PackStore;

use std::io::Cursor;

use image::{DynamicImage, ImageDecoder, ImageReader};
use vp_core::settings::ThumbnailSettings;

#[derive(Debug, thiserror::Error)]
pub enum ThumbError {
    #[error("unsupported image format")]
    Unsupported,
    #[error("could not decode image: {0}")]
    Decode(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Thumb {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub mime: &'static str,
}

fn decode_err(e: image::ImageError) -> ThumbError {
    match e {
        image::ImageError::Unsupported(_) => ThumbError::Unsupported,
        image::ImageError::IoError(e) => ThumbError::Io(e),
        other => ThumbError::Decode(other.to_string()),
    }
}

/// Decodes `bytes`, applies EXIF orientation and scales down to fit `max_edge_px`
/// (smaller images are kept as they are).
pub fn decode_scaled(bytes: &[u8], max_edge_px: u32) -> Result<DynamicImage, ThumbError> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    if reader.format().is_none() {
        return Err(ThumbError::Unsupported);
    }
    let mut decoder = reader.into_decoder().map_err(decode_err)?;
    let orientation = decoder.orientation().map_err(decode_err)?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(decode_err)?;
    img.apply_orientation(orientation);
    Ok(fit(img, max_edge_px))
}

/// Scales `img` down to fit `max_edge_px` (smaller images are kept as they are).
pub fn fit(img: DynamicImage, max_edge_px: u32) -> DynamicImage {
    let max = max_edge_px.max(1);
    if img.width() > max || img.height() > max { img.thumbnail(max, max) } else { img }
}

/// Decodes `bytes`, applies EXIF orientation and scales to fit `max_edge_px`.
/// Opaque images are encoded as JPEG, images with transparency as PNG.
pub fn generate(bytes: &[u8], s: &ThumbnailSettings) -> Result<Thumb, ThumbError> {
    encode(decode_scaled(bytes, s.max_edge_px)?, s)
}

/// Scales an image decoded elsewhere (e.g. by libheif) to fit `max_edge_px` and encodes
/// it like [`generate`].
pub fn from_image(img: DynamicImage, s: &ThumbnailSettings) -> Result<Thumb, ThumbError> {
    encode(fit(img, s.max_edge_px), s)
}

fn encode(img: DynamicImage, s: &ThumbnailSettings) -> Result<Thumb, ThumbError> {
    let (width, height) = (img.width(), img.height());
    let mut data = Vec::new();
    let mime = if img.color().has_alpha() {
        img.to_rgba8().write_to(&mut Cursor::new(&mut data), image::ImageFormat::Png).map_err(decode_err)?;
        "image/png"
    } else {
        let rgb = img.to_rgb8();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, s.quality.clamp(1, 100));
        enc.encode_image(&rgb).map_err(decode_err)?;
        "image/jpeg"
    };
    Ok(Thumb { data, width, height, mime })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

    fn encode(img: DynamicImage, fmt: ImageFormat) -> Vec<u8> {
        let mut v = Vec::new();
        img.write_to(&mut Cursor::new(&mut v), fmt).unwrap();
        v
    }

    #[test]
    fn scales_and_picks_format() {
        let s = ThumbnailSettings { max_edge_px: 100, ..Default::default() };
        let jpg = encode(DynamicImage::ImageRgb8(RgbImage::from_pixel(400, 200, Rgb([200, 10, 10]))), ImageFormat::Jpeg);
        let t = generate(&jpg, &s).unwrap();
        assert_eq!((t.width, t.height, t.mime), (100, 50, "image/jpeg"));
        assert_eq!(image::guess_format(&t.data).unwrap(), ImageFormat::Jpeg);

        let png = encode(DynamicImage::ImageRgba8(RgbaImage::from_pixel(30, 60, Rgba([0, 0, 0, 0]))), ImageFormat::Png);
        let t = generate(&png, &s).unwrap();
        assert_eq!((t.width, t.height, t.mime), (30, 60, "image/png"), "small images are not upscaled");
    }

    #[test]
    fn rejects_garbage() {
        let s = ThumbnailSettings::default();
        assert!(matches!(generate(b"definitely not an image", &s), Err(ThumbError::Unsupported)));
    }
}
