//! Picture edits (`vp_core::edit::EditSpec`): rotation by any angle and cropping, written
//! back in the original's format. The EXIF orientation is applied first (as the viewer shows
//! the picture) and cleared in the result; the rest of the EXIF data and the colour profile
//! are kept where the format can hold them.

use std::io::Cursor;

use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Rgba, RgbaImage, metadata::Orientation};
use vp_core::edit::EditSpec;

/// Formats edited pictures are written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
    Bmp,
    Tiff,
}

impl Format {
    /// The format for a file extension, when it can be edited.
    pub fn from_ext(ext: &str) -> Option<Self> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" | "jpe" | "jfif" => Format::Jpeg,
            "png" => Format::Png,
            "webp" => Format::Webp,
            "bmp" => Format::Bmp,
            "tif" | "tiff" => Format::Tiff,
            _ => return None,
        })
    }

    fn image_format(self) -> ImageFormat {
        match self {
            Format::Jpeg => ImageFormat::Jpeg,
            Format::Png => ImageFormat::Png,
            Format::Webp => ImageFormat::WebP,
            Format::Bmp => ImageFormat::Bmp,
            Format::Tiff => ImageFormat::Tiff,
        }
    }

    fn has_alpha(self) -> bool {
        matches!(self, Format::Png | Format::Webp | Format::Tiff)
    }
}

/// An edited picture, encoded.
pub struct Edited {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Size of the original as shown (EXIF orientation applied).
    pub source_width: u32,
    pub source_height: u32,
}

/// Applies `spec` to the picture `data` (in `format`) and encodes the result in the same
/// format. `quality` is the JPEG quality.
pub fn edit_image(data: &[u8], format: Format, spec: &EditSpec, quality: u8) -> Result<Edited, String> {
    let err = |e: image::ImageError| e.to_string();
    let mut decoder = ImageReader::with_format(Cursor::new(data), format.image_format()).into_decoder().map_err(err)?;
    let mut exif = decoder.exif_metadata().ok().flatten();
    let icc = decoder.icc_profile().ok().flatten();
    let orientation = match exif.as_deref_mut() {
        Some(chunk) => Orientation::remove_from_exif_chunk(chunk).unwrap_or(Orientation::NoTransforms),
        None => decoder.orientation().unwrap_or(Orientation::NoTransforms),
    };
    let mut img = DynamicImage::from_decoder(decoder).map_err(err)?;
    img.apply_orientation(orientation);
    let (source_width, source_height) = (img.width(), img.height());

    let out = transform(&img, spec, format.has_alpha());
    let (width, height) = (out.width(), out.height());
    let mut buf = Vec::new();
    let w = Cursor::new(&mut buf);
    match format {
        Format::Jpeg => {
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(w, quality.clamp(1, 100));
            attach(&mut enc, exif, icc);
            DynamicImage::ImageRgb8(out.to_rgb8()).write_with_encoder(enc).map_err(err)?;
        }
        Format::Png => {
            let mut enc = image::codecs::png::PngEncoder::new(w);
            attach(&mut enc, exif, icc);
            out.write_with_encoder(enc).map_err(err)?;
        }
        Format::Webp => {
            let mut enc = image::codecs::webp::WebPEncoder::new_lossless(w);
            attach(&mut enc, exif, icc);
            DynamicImage::ImageRgba8(out.to_rgba8()).write_with_encoder(enc).map_err(err)?;
        }
        Format::Bmp | Format::Tiff => {
            let mut w = w;
            out.write_to(&mut w, format.image_format()).map_err(err)?;
        }
    }
    Ok(Edited { data: buf, width, height, source_width, source_height })
}

fn attach(enc: &mut impl ImageEncoder, exif: Option<Vec<u8>>, icc: Option<Vec<u8>>) {
    if let Some(exif) = exif {
        let _ = enc.set_exif_metadata(exif);
    }
    if let Some(icc) = icc {
        let _ = enc.set_icc_profile(icc);
    }
}

/// Rotates and crops. The fill of a free rotation is transparent when `alpha` is allowed,
/// else black.
pub fn transform(img: &DynamicImage, spec: &EditSpec, alpha: bool) -> DynamicImage {
    let g = spec.geometry(img.width(), img.height(), false);
    if let Some(q) = spec.quarter_turns() {
        let turned = match q {
            1 => img.rotate90(),
            2 => img.rotate180(),
            3 => img.rotate270(),
            _ => img.clone(),
        };
        if (g.x, g.y, g.w, g.h) == (0, 0, turned.width(), turned.height()) {
            return turned;
        }
        return turned.crop_imm(g.x, g.y, g.w, g.h);
    }
    let src = img.to_rgba8();
    let fill = if alpha { Rgba([0, 0, 0, 0]) } else { Rgba([0, 0, 0, 255]) };
    let (sw, sh) = (src.width() as f64, src.height() as f64);
    let (s, c) = spec.angle().to_radians().sin_cos();
    let (bw, bh) = (g.box_w as f64, g.box_h as f64);
    let mut out = RgbaImage::new(g.w, g.h);
    for (ox, oy, px) in out.enumerate_pixels_mut() {
        // Centre of the output pixel, relative to the box's centre.
        let (bx, by) = (g.x as f64 + ox as f64 + 0.5 - bw / 2.0, g.y as f64 + oy as f64 + 0.5 - bh / 2.0);
        // Undo the clockwise rotation, back into source pixel coordinates.
        let (sx, sy) = (bx * c + by * s + sw / 2.0 - 0.5, -bx * s + by * c + sh / 2.0 - 0.5);
        *px = bilinear(&src, sx, sy, fill);
    }
    let out = DynamicImage::ImageRgba8(out);
    if alpha || img.color().has_alpha() { out } else { DynamicImage::ImageRgb8(out.to_rgb8()) }
}

fn bilinear(src: &RgbaImage, x: f64, y: f64, fill: Rgba<u8>) -> Rgba<u8> {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (w, h) = (src.width() as i64, src.height() as i64);
    let at = |xi: i64, yi: i64| {
        if xi < 0 || yi < 0 || xi >= w || yi >= h { fill } else { *src.get_pixel(xi as u32, yi as u32) }
    };
    let (x0, y0) = (x0 as i64, y0 as i64);
    if x0 < -1 || y0 < -1 || x0 >= w || y0 >= h {
        return fill;
    }
    let (p00, p10, p01, p11) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
    let mut out = [0u8; 4];
    for i in 0..4 {
        let top = p00[i] as f64 * (1.0 - fx) + p10[i] as f64 * fx;
        let bottom = p01[i] as f64 * (1.0 - fx) + p11[i] as f64 * fx;
        out[i] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
    }
    Rgba(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, Rgb, RgbImage};
    use vp_core::edit::Crop;

    fn sample(w: u32, h: u32) -> DynamicImage {
        // Left half red, right half blue.
        DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, _| if x < w / 2 { Rgb([255, 0, 0]) } else { Rgb([0, 0, 255]) }))
    }

    #[test]
    fn quarter_turn_and_crop() {
        let img = sample(40, 20);
        let spec = EditSpec { rotate_deg: 90.0, crop: Some(Crop { x: 0.0, y: 0.0, w: 1.0, h: 0.5 }), ..Default::default() };
        let out = transform(&img, &spec, false);
        assert_eq!(out.dimensions(), (20, 20));
        // Turned clockwise, the red left half is on top.
        assert_eq!(out.to_rgb8().get_pixel(10, 10), &Rgb([255, 0, 0]));
    }

    #[test]
    fn free_rotation_has_no_fill_inside_and_fill_outside() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(100, 60, Rgb([200, 200, 200])));
        let inside = transform(&img, &EditSpec { rotate_deg: 15.0, ..Default::default() }, false).to_rgb8();
        assert!(inside.pixels().all(|p| p[0] > 150), "the default crop stays inside the picture");
        let expanded = transform(&img, &EditSpec { rotate_deg: 15.0, expand: true, ..Default::default() }, true).to_rgba8();
        assert_eq!(expanded.get_pixel(0, 0)[3], 0, "the corner of an expanded canvas is transparent");
        let (w, h) = expanded.dimensions();
        assert_eq!(expanded.get_pixel(w / 2, h / 2)[3], 255);
    }

    #[test]
    fn round_trip_keeps_format_and_size() {
        let img = sample(64, 32);
        let mut png = Vec::new();
        img.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        let spec = EditSpec { crop: Some(Crop { x: 0.5, y: 0.0, w: 0.5, h: 1.0 }), ..Default::default() };
        let out = edit_image(&png, Format::Png, &spec, 90).unwrap();
        assert_eq!((out.width, out.height), (32, 32));
        let back = image::load_from_memory_with_format(&out.data, ImageFormat::Png).unwrap();
        assert_eq!(back.to_rgb8().get_pixel(5, 5), &Rgb([0, 0, 255]));

        let mut jpg = Vec::new();
        img.write_to(&mut Cursor::new(&mut jpg), ImageFormat::Jpeg).unwrap();
        let out = edit_image(&jpg, Format::Jpeg, &EditSpec { rotate_deg: 270.0, ..Default::default() }, 90).unwrap();
        assert_eq!((out.width, out.height), (32, 64));
    }
}
