//! Face alignment: the five landmarks are moved onto the standard 112 × 112 template
//! (ArcFace / SFace) with a similarity transform, so the embedder sees upright faces.

use image::RgbImage;

use crate::prep::bilinear;

/// Landmark template for a 112 × 112 crop: eyes, nose tip, mouth corners (the order YuNet
/// and similar detectors use).
pub const TEMPLATE_112: [[f32; 2]; 5] =
    [[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]];

/// `q = [[a, -b], [b, a]] · p + (tx, ty)`: rotation and uniform scale plus a shift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity {
    pub a: f32,
    pub b: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Similarity {
    /// Least-squares fit taking `from` onto `to` (Umeyama, 2-D).
    pub fn fit(from: &[[f32; 2]], to: &[[f32; 2]]) -> Option<Self> {
        let n = from.len().min(to.len());
        if n < 2 {
            return None;
        }
        let mean = |pts: &[[f32; 2]]| {
            let (sx, sy) = pts[..n].iter().fold((0.0, 0.0), |(x, y), p| (x + p[0], y + p[1]));
            [sx / n as f32, sy / n as f32]
        };
        let (mp, mq) = (mean(from), mean(to));
        let (mut sxx, mut sxy, mut var) = (0.0f32, 0.0f32, 0.0f32);
        for i in 0..n {
            let (ax, ay) = (from[i][0] - mp[0], from[i][1] - mp[1]);
            let (bx, by) = (to[i][0] - mq[0], to[i][1] - mq[1]);
            sxx += ax * bx + ay * by;
            sxy += ax * by - ay * bx;
            var += ax * ax + ay * ay;
        }
        if var <= f32::EPSILON {
            return None;
        }
        let (a, b) = (sxx / var, sxy / var);
        Some(Self { a, b, tx: mq[0] - (a * mp[0] - b * mp[1]), ty: mq[1] - (b * mp[0] + a * mp[1]) })
    }

    pub fn apply(&self, p: [f32; 2]) -> [f32; 2] {
        [self.a * p[0] - self.b * p[1] + self.tx, self.b * p[0] + self.a * p[1] + self.ty]
    }

    pub fn invert(&self, q: [f32; 2]) -> [f32; 2] {
        let (x, y) = (q[0] - self.tx, q[1] - self.ty);
        let d = self.a * self.a + self.b * self.b;
        [(self.a * x + self.b * y) / d, (-self.b * x + self.a * y) / d]
    }
}

/// The face with `landmarks` warped onto the template, `size` × `size` (template scaled).
pub fn align(img: &RgbImage, landmarks: &[[f32; 2]; 5], size: u32) -> Option<RgbImage> {
    let k = size as f32 / 112.0;
    let target: Vec<[f32; 2]> = TEMPLATE_112.iter().map(|p| [p[0] * k, p[1] * k]).collect();
    let t = Similarity::fit(landmarks, &target)?;
    let mut out = RgbImage::new(size, size);
    for v in 0..size {
        for u in 0..size {
            let [sx, sy] = t.invert([u as f32 + 0.5, v as f32 + 0.5]);
            if let Some(p) = bilinear(img, sx - 0.5, sy - 0.5) {
                out.put_pixel(u, v, p);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_recovers_a_rotation_and_scale() {
        let truth = Similarity { a: 0.0, b: 2.0, tx: 5.0, ty: -3.0 }; // 90° turn, twice as large
        let from: Vec<[f32; 2]> = TEMPLATE_112.to_vec();
        let to: Vec<[f32; 2]> = from.iter().map(|p| truth.apply(*p)).collect();
        let t = Similarity::fit(&from, &to).unwrap();
        for (f, q) in from.iter().zip(&to) {
            let p = t.apply(*f);
            assert!((p[0] - q[0]).abs() < 1e-3 && (p[1] - q[1]).abs() < 1e-3);
            let back = t.invert(*q);
            assert!((back[0] - f[0]).abs() < 1e-3 && (back[1] - f[1]).abs() < 1e-3);
        }
        assert!(Similarity::fit(&[[1.0, 1.0], [1.0, 1.0]], &[[0.0, 0.0], [1.0, 1.0]]).is_none());
    }

    #[test]
    fn aligned_template_is_the_identity_crop() {
        // An image whose landmarks already sit on the template comes out unchanged.
        let img = RgbImage::from_fn(112, 112, |x, y| image::Rgb([x as u8, y as u8, 7]));
        let out = align(&img, &TEMPLATE_112, 112).unwrap();
        assert_eq!(out.get_pixel(30, 60).0, [30, 60, 7]);
        assert_eq!(align(&img, &TEMPLATE_112, 56).unwrap().dimensions(), (56, 56));
    }
}
