//! Reading detector outputs: boxes (and landmarks) in model input pixels, then NMS.

use std::collections::HashMap;

use crate::prep::Placement;

/// A found face: box in source image pixels, and five landmarks when the detector gives
/// them (eyes, nose, mouth corners: the order alignment expects).
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    pub landmarks: Option<[[f32; 2]; 5]>,
}

impl Detection {
    fn area(&self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    fn iou(&self, o: &Detection) -> f32 {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
        let union = self.area() + o.area() - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }

    /// Moves model input coordinates to the source image.
    fn place(mut self, p: &Placement) -> Self {
        let (x0, y0) = p.to_source(self.x, self.y);
        let (x1, y1) = p.to_source(self.x + self.w, self.y + self.h);
        (self.x, self.y, self.w, self.h) = (x0, y0, x1 - x0, y1 - y0);
        if let Some(l) = &mut self.landmarks {
            for pt in l.iter_mut() {
                let (x, y) = p.to_source(pt[0], pt[1]);
                *pt = [x, y];
            }
        }
        self
    }

    /// Clips the box to an image of `w` × `h`.
    fn clip(mut self, w: f32, h: f32) -> Self {
        let x0 = self.x.clamp(0.0, w);
        let y0 = self.y.clamp(0.0, h);
        let x1 = (self.x + self.w).clamp(0.0, w);
        let y1 = (self.y + self.h).clamp(0.0, h);
        (self.x, self.y, self.w, self.h) = (x0, y0, x1 - x0, y1 - y0);
        self
    }
}

/// Keeps the best of overlapping detections.
pub fn nms(mut dets: Vec<Detection>, iou: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Detection> = Vec::new();
    for d in dets {
        if keep.iter().all(|k| k.iou(&d) <= iou) {
            keep.push(d);
        }
    }
    keep
}

/// Output tensors by name: shape and values.
pub type Outputs = HashMap<String, (Vec<i64>, Vec<f32>)>;

/// OpenCV YuNet (2023): per stride `s` in 8, 16, 32 a grid of `(H/s) × (W/s)` cells with
/// `cls_s`, `obj_s` (score = √(cls·obj)), `bbox_s` (centre offset and log size in cells)
/// and `kps_s` (five landmarks as cell offsets).
pub fn decode_yunet(out: &Outputs, in_w: u32, in_h: u32, min_score: f32) -> Vec<Detection> {
    let mut dets = Vec::new();
    for stride in [8u32, 16, 32] {
        let get = |n: &str| out.get(&format!("{n}_{stride}")).map(|(_, v)| v.as_slice());
        let (Some(cls), Some(obj), Some(bbox), Some(kps)) = (get("cls"), get("obj"), get("bbox"), get("kps")) else { continue };
        let cols = in_w.div_ceil(stride) as usize;
        let rows = in_h.div_ceil(stride) as usize;
        let s = stride as f32;
        for r in 0..rows {
            for c in 0..cols {
                let i = r * cols + c;
                if i >= cls.len() || i >= obj.len() || (i + 1) * 4 > bbox.len() || (i + 1) * 10 > kps.len() {
                    continue;
                }
                let score = (cls[i].clamp(0.0, 1.0) * obj[i].clamp(0.0, 1.0)).sqrt();
                if score < min_score {
                    continue;
                }
                let b = &bbox[i * 4..i * 4 + 4];
                let cx = (c as f32 + b[0]) * s;
                let cy = (r as f32 + b[1]) * s;
                let w = b[2].exp() * s;
                let h = b[3].exp() * s;
                let k = &kps[i * 10..i * 10 + 10];
                let mut lm = [[0f32; 2]; 5];
                for (n, pt) in lm.iter_mut().enumerate() {
                    *pt = [(k[2 * n] + c as f32) * s, (k[2 * n + 1] + r as f32) * s];
                }
                dets.push(Detection { x: cx - w / 2.0, y: cy - h / 2.0, w, h, score, landmarks: Some(lm) });
            }
        }
    }
    dets
}

/// YOLO v8-style output: `[1, 4 + classes, N]` (or transposed `[1, N, 4 + classes]`), boxes
/// as centre and size in input pixels, score = the best class.
pub fn decode_yolo(shape: &[i64], data: &[f32], min_score: f32) -> Vec<Detection> {
    let dims: Vec<usize> = shape.iter().map(|&d| d.max(0) as usize).collect();
    let (a, b) = match dims.as_slice() {
        [1, a, b] | [a, b] => (*a, *b),
        _ => return Vec::new(),
    };
    // Fewer attributes than candidates: attributes come first.
    let attr_major = a < b;
    let (attrs, n) = if attr_major { (a, b) } else { (b, a) };
    let at = |k: usize, i: usize| if attr_major { data[k * b + i] } else { data[i * b + k] };
    if attrs < 5 || data.len() < attrs * n {
        return Vec::new();
    }
    let mut dets = Vec::new();
    for i in 0..n {
        let score = (4..attrs).map(|k| at(k, i)).fold(f32::MIN, f32::max);
        if score < min_score {
            continue;
        }
        let (cx, cy, w, h) = (at(0, i), at(1, i), at(2, i), at(3, i));
        dets.push(Detection { x: cx - w / 2.0, y: cy - h / 2.0, w, h, score, landmarks: None });
    }
    dets
}

/// Maps detections to the source image (`src_w` × `src_h`), merges overlaps and drops
/// faces whose shorter edge is under `min_px`.
pub fn finish(dets: Vec<Detection>, p: &Placement, src_w: u32, src_h: u32, iou: f32, min_px: f32) -> Vec<Detection> {
    let placed = dets.into_iter().map(|d| d.place(p).clip(src_w as f32, src_h as f32)).filter(|d| d.w.min(d.h) >= min_px).collect();
    nms(placed, iou)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(x: f32, score: f32) -> Detection {
        Detection { x, y: 0.0, w: 10.0, h: 10.0, score, landmarks: None }
    }

    #[test]
    fn nms_keeps_the_best_of_overlaps() {
        let kept = nms(vec![det(0.0, 0.5), det(1.0, 0.9), det(50.0, 0.7)], 0.4);
        assert_eq!(kept.iter().map(|d| d.score).collect::<Vec<_>>(), [0.9, 0.7]);
    }

    #[test]
    fn yunet_cells_become_boxes_and_landmarks() {
        // 32 × 32 input: stride 8 → 4 × 4 cells; one strong cell at row 1, col 2.
        let mut out = Outputs::new();
        let cells = 16;
        let mut cls = vec![0.0; cells];
        let mut obj = vec![0.0; cells];
        let mut bbox = vec![0.0; cells * 4];
        let mut kps = vec![0.0; cells * 10];
        let i = 4 + 2;
        cls[i] = 0.81;
        obj[i] = 1.0;
        bbox[i * 4..i * 4 + 4].copy_from_slice(&[0.5, 0.5, 0.0, 0.0]); // centre of the cell, 1 cell wide
        kps[i * 10] = 0.25;
        for (n, v) in [("cls", cls), ("obj", obj), ("bbox", bbox), ("kps", kps)] {
            out.insert(format!("{n}_8"), (vec![1, cells as i64, 1], v));
        }
        let d = decode_yunet(&out, 32, 32, 0.5);
        assert_eq!(d.len(), 1);
        assert!((d[0].score - 0.9).abs() < 1e-6);
        assert_eq!((d[0].x, d[0].y, d[0].w, d[0].h), (16.0, 8.0, 8.0, 8.0));
        assert_eq!(d[0].landmarks.unwrap()[0], [18.0, 8.0]);
        assert!(decode_yunet(&out, 32, 32, 0.95).is_empty());
    }

    #[test]
    fn yolo_reads_either_layout() {
        // Eight candidates (real models have thousands), one class: [1, 5, 8]; only the
        // first scores.
        let n = 8;
        let mut data = vec![0.0f32; 5 * n];
        for (k, v) in [50.0, 50.0, 20.0, 20.0, 0.9].into_iter().enumerate() {
            data[k * n] = v;
        }
        let d = decode_yolo(&[1, 5, n as i64], &data, 0.5);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].x, d[0].y, d[0].w, d[0].h), (40.0, 40.0, 20.0, 20.0));
        // The same candidates as [1, 8, 5].
        let mut t = vec![0.0f32; 5 * n];
        t[..5].copy_from_slice(&[50.0, 50.0, 20.0, 20.0, 0.9]);
        assert_eq!(decode_yolo(&[1, n as i64, 5], &t, 0.5), d);
    }

    #[test]
    fn finish_maps_back_and_filters_small() {
        let p = Placement { scale_x: 0.5, scale_y: 0.5, offset_x: 0.0, offset_y: 0.0 };
        let d = finish(vec![det(0.0, 0.9), Detection { w: 4.0, h: 4.0, ..det(30.0, 0.8) }], &p, 1000, 1000, 0.4, 16.0);
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].w, d[0].h), (20.0, 20.0));
    }
}
