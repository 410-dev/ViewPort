//! Basic edits of a picture or video (rotate, crop, cut, speed), shared by the image editor,
//! the FFmpeg and Android video paths and the UI (`src/lib/edit/geometry.ts` mirrors the
//! geometry here).
//!
//! Order: the original is rotated about its centre onto the bounding box of the rotated
//! frame, then cropped. The crop is given in fractions of that bounding box, so the same
//! spec applies at any resolution.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditSpec {
    /// Clockwise rotation in degrees, any angle.
    pub rotate_deg: f64,
    /// The rotated frame keeps all of the picture on a larger canvas, filled black
    /// (transparent where the format allows). Off, the editor keeps the crop inside the
    /// picture, so no fill shows.
    pub expand: bool,
    /// Kept part, in fractions of the rotated frame's bounding box. None keeps it all
    /// (or, rotated without `expand`, the largest part of the original's shape inside).
    pub crop: Option<Crop>,
    /// Kept span of a video, in milliseconds of the original.
    pub trim: Option<Trim>,
    /// Playback speed of a video (2 = twice as fast).
    pub speed: f64,
}

impl Default for EditSpec {
    fn default() -> Self {
        Self { rotate_deg: 0.0, expand: false, crop: None, trim: None, speed: 1.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Trim {
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Pixel geometry of an edit for a `w`×`h` original.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EditGeometry {
    /// Size of the rotated frame's bounding box.
    pub box_w: u32,
    pub box_h: u32,
    /// Kept rectangle within that box.
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Smallest and largest speed accepted.
pub const SPEED_RANGE: (f64, f64) = (0.1, 16.0);

impl EditSpec {
    /// Rotation in [0, 360).
    pub fn angle(&self) -> f64 {
        self.rotate_deg.rem_euclid(360.0)
    }

    /// The rotation in quarter turns (0..=3) when it is a multiple of 90°.
    pub fn quarter_turns(&self) -> Option<u8> {
        let q = self.angle() / 90.0;
        ((q - q.round()).abs() < 1e-6).then(|| (q.round() as u8) % 4)
    }

    pub fn is_identity(&self) -> bool {
        self.quarter_turns() == Some(0)
            && self.crop.is_none_or(|c| c.x <= 0.0 && c.y <= 0.0 && c.w >= 1.0 && c.h >= 1.0)
            && self.trim.is_none()
            && (self.speed - 1.0).abs() < 1e-9
    }

    /// Checks the values; `duration_ms` is the video's length (None for pictures).
    pub fn validate(&self, duration_ms: Option<i64>) -> Result<(), String> {
        if !self.rotate_deg.is_finite() {
            return Err("rotation".into());
        }
        if let Some(c) = self.crop {
            let ok = [c.x, c.y, c.w, c.h].iter().all(|v| v.is_finite())
                && c.x >= -1e-6
                && c.y >= -1e-6
                && c.w > 0.0
                && c.h > 0.0
                && c.x + c.w <= 1.0 + 1e-6
                && c.y + c.h <= 1.0 + 1e-6;
            if !ok {
                return Err("crop".into());
            }
        }
        if duration_ms.is_none() && (self.trim.is_some() || (self.speed - 1.0).abs() > 1e-9) {
            return Err("cut and speed are for videos".into());
        }
        if !(self.speed.is_finite() && self.speed >= SPEED_RANGE.0 && self.speed <= SPEED_RANGE.1) {
            return Err("speed".into());
        }
        if let Some(t) = self.trim {
            let end = duration_ms.unwrap_or(i64::MAX);
            if t.start_ms < 0 || t.end_ms <= t.start_ms || (end > 0 && t.start_ms >= end) {
                return Err("cut".into());
            }
        }
        Ok(())
    }

    /// The crop actually applied: the given one, or (rotated without `expand`) the
    /// largest part of the original's shape inside the picture.
    pub fn effective_crop(&self, w: f64, h: f64) -> Crop {
        if let Some(c) = self.crop {
            return c;
        }
        if self.expand || self.quarter_turns().is_some() {
            return Crop { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
        }
        let (bw, bh) = rotated_size(w, h, self.rotate_deg);
        let s = inner_scale(w, h, self.rotate_deg);
        let (cw, ch) = (w * s / bw, h * s / bh);
        Crop { x: (1.0 - cw) / 2.0, y: (1.0 - ch) / 2.0, w: cw, h: ch }
    }

    /// Pixel geometry for a `w`×`h` original. `even` rounds the output to even sizes
    /// (video encoders need them).
    pub fn geometry(&self, w: u32, h: u32, even: bool) -> EditGeometry {
        let (bw, bh) = match self.quarter_turns() {
            Some(1 | 3) => (h, w),
            Some(_) => (w, h),
            None => {
                let (bw, bh) = rotated_size(w as f64, h as f64, self.rotate_deg);
                (bw.round().max(1.0) as u32, bh.round().max(1.0) as u32)
            }
        };
        let c = self.effective_crop(w as f64, h as f64);
        let fit = |pos: f64, len: f64, total: u32| {
            let total_f = total as f64;
            let mut a = (pos * total_f).round().clamp(0.0, total_f - 1.0) as u32;
            let mut n = (len * total_f).round().max(1.0) as u32;
            n = n.min(total - a);
            if even && n > 1 {
                n -= n % 2;
                if n == 0 {
                    n = 2.min(total);
                    a = a.min(total - n);
                }
            }
            (a, n)
        };
        let (x, cw) = fit(c.x, c.w, bw);
        let (y, ch) = fit(c.y, c.h, bh);
        EditGeometry { box_w: bw, box_h: bh, x, y, w: cw, h: ch }
    }

    /// Where a box (`x, y, w, h` in fractions of a `w`×`h` original) lands in the result,
    /// in fractions of it; None when the box's centre is cut away.
    pub fn map_box(&self, w: u32, h: u32, b: (f64, f64, f64, f64)) -> Option<(f64, f64, f64, f64)> {
        let g = self.geometry(w, h, false);
        let (wf, hf) = (w as f64, h as f64);
        let (s, c) = match self.quarter_turns() {
            Some(q) => [(0.0, 1.0), (1.0, 0.0), (0.0, -1.0), (-1.0, 0.0)][q as usize],
            None => self.angle().to_radians().sin_cos(),
        };
        let (cx, cy) = ((b.0 + b.2 / 2.0) * wf - wf / 2.0, (b.1 + b.3 / 2.0) * hf - hf / 2.0);
        // Clockwise about the centre, into the rotated box, then into the crop.
        let ox = cx * c - cy * s + g.box_w as f64 / 2.0 - g.x as f64;
        let oy = cx * s + cy * c + g.box_h as f64 / 2.0 - g.y as f64;
        let (gw, gh) = (g.w as f64, g.h as f64);
        if !(0.0..=gw).contains(&ox) || !(0.0..=gh).contains(&oy) {
            return None;
        }
        let (bw, bh) = (b.2 * wf, b.3 * hf);
        let (nw, nh) = (bw * c.abs() + bh * s.abs(), bw * s.abs() + bh * c.abs());
        Some(((ox - nw / 2.0) / gw, (oy - nh / 2.0) / gh, nw / gw, nh / gh))
    }

    /// Length of the result in milliseconds for a video of `duration_ms`.
    pub fn output_duration_ms(&self, duration_ms: i64) -> i64 {
        let (a, b) = self.trim.map_or((0, duration_ms), |t| (t.start_ms, t.end_ms.min(duration_ms)));
        ((b - a).max(0) as f64 / self.speed).round() as i64
    }
}

/// Bounding box of a `w`×`h` frame rotated by `deg`.
pub fn rotated_size(w: f64, h: f64, deg: f64) -> (f64, f64) {
    let (s, c) = deg.to_radians().sin_cos();
    (w * c.abs() + h * s.abs(), w * s.abs() + h * c.abs())
}

/// Largest scale of the original's shape, centred, that fits inside it rotated by `deg`.
pub fn inner_scale(w: f64, h: f64, deg: f64) -> f64 {
    let (s, c) = deg.to_radians().sin_cos();
    let (s, c) = (s.abs(), c.abs());
    (w / (w * c + h * s)).min(h / (w * s + h * c)).min(1.0)
}

/// Whether `crop` (fractions of the rotated bounding box) lies inside the rotated picture.
pub fn crop_inside(w: f64, h: f64, deg: f64, crop: Crop) -> bool {
    let (bw, bh) = rotated_size(w, h, deg);
    let (s, c) = deg.to_radians().sin_cos();
    let eps = 0.5 + 1e-6 * (w + h);
    [(crop.x, crop.y), (crop.x + crop.w, crop.y), (crop.x, crop.y + crop.h), (crop.x + crop.w, crop.y + crop.h)].iter().all(|&(fx, fy)| {
        let (px, py) = (fx * bw - bw / 2.0, fy * bh - bh / 2.0);
        // Back into the original's frame (undo the clockwise rotation).
        let (ox, oy) = (px * c + py * s, -px * s + py * c);
        ox.abs() <= w / 2.0 + eps && oy.abs() <= h / 2.0 + eps
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_turns_swap_the_box() {
        let spec = EditSpec { rotate_deg: -90.0, ..Default::default() };
        assert_eq!(spec.quarter_turns(), Some(3));
        let g = spec.geometry(400, 300, false);
        assert_eq!((g.box_w, g.box_h, g.x, g.y, g.w, g.h), (300, 400, 0, 0, 300, 400));
    }

    #[test]
    fn free_rotation_keeps_the_inside_by_default() {
        let spec = EditSpec { rotate_deg: 10.0, ..Default::default() };
        let c = spec.effective_crop(400.0, 300.0);
        assert!(crop_inside(400.0, 300.0, 10.0, c));
        // The original's shape, and as large as fits: a little larger no longer does.
        assert!((c.w * rotated_size(400.0, 300.0, 10.0).0 / (c.h * rotated_size(400.0, 300.0, 10.0).1) - 4.0 / 3.0).abs() < 1e-9);
        let grown = Crop { x: c.x - 0.01, y: c.y - 0.01, w: c.w + 0.02, h: c.h + 0.02 };
        assert!(!crop_inside(400.0, 300.0, 10.0, grown));
        // Expanded, the whole box is kept.
        let spec = EditSpec { rotate_deg: 10.0, expand: true, ..Default::default() };
        let g = spec.geometry(400, 300, false);
        assert_eq!((g.x, g.y, g.w, g.h), (0, 0, g.box_w, g.box_h));
    }

    #[test]
    fn crop_maps_to_pixels_and_video_sizes_are_even() {
        let spec = EditSpec { crop: Some(Crop { x: 0.25, y: 0.1, w: 0.5, h: 0.333 }), ..Default::default() };
        let g = spec.geometry(1001, 600, true);
        assert_eq!((g.x, g.y), (250, 60));
        assert_eq!((g.w % 2, g.h % 2), (0, 0));
        assert!(g.x + g.w <= 1001 && g.y + g.h <= 600);
    }

    #[test]
    fn boxes_follow_the_picture() {
        // A box at the top left of a 400×300 picture turned clockwise ends at the top right.
        let spec = EditSpec { rotate_deg: 90.0, ..Default::default() };
        let (x, y, w, h) = spec.map_box(400, 300, (0.0, 0.0, 0.25, 0.2)).unwrap();
        assert!((x - 0.8).abs() < 1e-9 && y.abs() < 1e-9 && (w - 0.2).abs() < 1e-9 && (h - 0.25).abs() < 1e-9, "{x} {y} {w} {h}");
        // Cropped to the right half, a box on the left is gone and one on the right moves.
        let spec = EditSpec { crop: Some(Crop { x: 0.5, y: 0.0, w: 0.5, h: 1.0 }), ..Default::default() };
        assert!(spec.map_box(400, 300, (0.1, 0.1, 0.1, 0.1)).is_none());
        let (x, ..) = spec.map_box(400, 300, (0.6, 0.1, 0.1, 0.1)).unwrap();
        assert!((x - 0.2).abs() < 1e-9);
    }

    #[test]
    fn validation_and_duration() {
        let mut spec = EditSpec { trim: Some(Trim { start_ms: 1000, end_ms: 5000 }), speed: 2.0, ..Default::default() };
        assert!(spec.validate(Some(10_000)).is_ok());
        assert!(spec.validate(None).is_err());
        assert_eq!(spec.output_duration_ms(10_000), 2000);
        spec.trim = Some(Trim { start_ms: 5000, end_ms: 1000 });
        assert!(spec.validate(Some(10_000)).is_err());
        assert!(EditSpec::default().is_identity());
        assert!(EditSpec { rotate_deg: 360.0, ..Default::default() }.is_identity());
    }
}
