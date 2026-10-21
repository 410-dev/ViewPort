//! Images to input tensors, and the geometry to map model coordinates back.

use image::{RgbImage, imageops::FilterType};

use crate::pack::{Channels, Fit, InputSpec, Layout};

/// Maps a point of the model input back to the source image: `src = (p - offset) / scale`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub scale_x: f32,
    pub scale_y: f32,
    pub offset_x: f32,
    pub offset_y: f32,
}

impl Placement {
    pub fn to_source(&self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.offset_x) / self.scale_x, (y - self.offset_y) / self.scale_y)
    }
}

/// An input tensor: shape and values.
#[derive(Debug, Clone, PartialEq)]
pub struct Tensor {
    pub shape: Vec<i64>,
    pub data: Vec<f32>,
}

/// Fits `img` into the input size of `spec` and returns the tensor and where the image went.
pub fn to_tensor(img: &RgbImage, spec: &InputSpec) -> (Tensor, Placement) {
    let (w, h) = (spec.width.max(1), spec.height.max(1));
    let (iw, ih) = (img.width().max(1) as f32, img.height().max(1) as f32);
    let pad = image::Rgb(spec.pad);
    let (canvas, placement) = match spec.fit {
        Fit::Stretch => {
            let c = image::imageops::resize(img, w, h, FilterType::Triangle);
            (c, Placement { scale_x: w as f32 / iw, scale_y: h as f32 / ih, offset_x: 0.0, offset_y: 0.0 })
        }
        Fit::Letterbox | Fit::PadSquare => {
            let s = (w as f32 / iw).min(h as f32 / ih);
            let (nw, nh) = (((iw * s).round() as u32).clamp(1, w), ((ih * s).round() as u32).clamp(1, h));
            let scaled = image::imageops::resize(img, nw, nh, FilterType::Triangle);
            let (ox, oy) = if spec.fit == Fit::PadSquare { ((w - nw) / 2, (h - nh) / 2) } else { (0, 0) };
            let mut c = RgbImage::from_pixel(w, h, pad);
            image::imageops::overlay(&mut c, &scaled, ox as i64, oy as i64);
            (c, Placement { scale_x: s, scale_y: s, offset_x: ox as f32, offset_y: oy as f32 })
        }
    };
    (pixels(&canvas, spec), placement)
}

/// The tensor of an image already at the input size.
pub fn pixels(img: &RgbImage, spec: &InputSpec) -> Tensor {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let order: [usize; 3] = if spec.channels == Channels::Bgr { [2, 1, 0] } else { [0, 1, 2] };
    let value = |p: &image::Rgb<u8>, c: usize| (p.0[order[c]] as f32 * spec.scale - spec.mean[c]) / spec.std[c];
    let mut data = vec![0f32; 3 * w * h];
    match spec.layout {
        Layout::Nchw => {
            for (x, y, p) in img.enumerate_pixels() {
                let i = y as usize * w + x as usize;
                for c in 0..3 {
                    data[c * w * h + i] = value(p, c);
                }
            }
            Tensor { shape: vec![1, 3, h as i64, w as i64], data }
        }
        Layout::Nhwc => {
            for (x, y, p) in img.enumerate_pixels() {
                let i = (y as usize * w + x as usize) * 3;
                for c in 0..3 {
                    data[i + c] = value(p, c);
                }
            }
            Tensor { shape: vec![1, h as i64, w as i64, 3], data }
        }
    }
}

/// A square crop of `img` centred on a box grown by `grow`, padded where it leaves the
/// image, scaled to `size`.
pub fn square_crop(img: &RgbImage, b: [f32; 4], grow: f32, size: u32, pad: [u8; 3]) -> RgbImage {
    let [x, y, w, h] = b;
    let side = (w.max(h) * grow.max(1.0)).max(1.0);
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let (x0, y0) = (cx - side / 2.0, cy - side / 2.0);
    let out = size.max(1);
    let step = side / out as f32;
    let mut c = RgbImage::from_pixel(out, out, image::Rgb(pad));
    for v in 0..out {
        for u in 0..out {
            let sx = x0 + (u as f32 + 0.5) * step;
            let sy = y0 + (v as f32 + 0.5) * step;
            if let Some(p) = bilinear(img, sx - 0.5, sy - 0.5) {
                c.put_pixel(u, v, p);
            }
        }
    }
    c
}

/// Bilinear sample at a (pixel-centre based) position, `None` outside the image.
pub fn bilinear(img: &RgbImage, x: f32, y: f32) -> Option<image::Rgb<u8>> {
    let (w, h) = (img.width() as i64, img.height() as i64);
    if x < -0.5 || y < -0.5 || x > w as f32 - 0.5 || y > h as f32 - 0.5 {
        return None;
    }
    let (x0, y0) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |xx: i64, yy: i64| img.get_pixel(xx.clamp(0, w - 1) as u32, yy.clamp(0, h - 1) as u32).0;
    let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
    let mut out = [0u8; 3];
    for i in 0..3 {
        let top = a[i] as f32 * (1.0 - fx) + b[i] as f32 * fx;
        let bottom = c[i] as f32 * (1.0 - fx) + d[i] as f32 * fx;
        out[i] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
    }
    Some(image::Rgb(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(w: u32, h: u32, fit: Fit) -> InputSpec {
        InputSpec { width: w, height: h, layout: Layout::Nchw, channels: Channels::Rgb, scale: 1.0, mean: [0.0; 3], std: [1.0; 3], fit, pad: [0; 3] }
    }

    #[test]
    fn letterbox_maps_points_back() {
        let img = RgbImage::from_pixel(200, 100, image::Rgb([255, 0, 0]));
        let (t, p) = to_tensor(&img, &spec(64, 64, Fit::Letterbox));
        assert_eq!(t.shape, [1, 3, 64, 64]);
        assert_eq!(p.to_source(32.0, 16.0), (100.0, 50.0));
        // The red image fills the top half; the bottom is padding.
        assert_eq!(t.data[0], 255.0);
        assert_eq!(t.data[63 * 64], 0.0);
        // Square padding centres it.
        let (_, p) = to_tensor(&img, &spec(64, 64, Fit::PadSquare));
        assert_eq!(p.offset_y, 16.0);
    }

    #[test]
    fn channel_order_layout_and_normalisation() {
        let img = RgbImage::from_pixel(2, 1, image::Rgb([10, 20, 30]));
        let s = InputSpec { layout: Layout::Nhwc, channels: Channels::Bgr, scale: 0.5, mean: [1.0, 2.0, 3.0], std: [2.0; 3], ..spec(2, 1, Fit::Stretch) };
        let t = pixels(&img, &s);
        assert_eq!(t.shape, [1, 1, 2, 3]);
        assert_eq!(&t.data[..3], &[(15.0 - 1.0) / 2.0, (10.0 - 2.0) / 2.0, (5.0 - 3.0) / 2.0]);
    }

    #[test]
    fn square_crop_pads_outside() {
        let img = RgbImage::from_pixel(10, 10, image::Rgb([200, 200, 200]));
        let c = square_crop(&img, [0.0, 0.0, 4.0, 4.0], 2.0, 8, [0, 0, 0]);
        assert_eq!(c.dimensions(), (8, 8));
        assert_eq!(c.get_pixel(0, 0).0, [0, 0, 0], "above-left of the image is padding");
        assert_eq!(c.get_pixel(6, 6).0, [200, 200, 200]);
    }
}
