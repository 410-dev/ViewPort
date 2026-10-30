/**
 * Geometry of rotate-then-crop edits, mirroring `vp_core::edit`: the picture turns about
 * its centre onto the bounding box of the turned frame, and the crop is in fractions of
 * that box.
 */

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export const FULL: Rect = { x: 0, y: 0, w: 1, h: 1 };

/** Bounding box of a `w`×`h` frame turned by `deg`. */
export function rotatedSize(w: number, h: number, deg: number): [number, number] {
  const r = (deg * Math.PI) / 180;
  const s = Math.abs(Math.sin(r));
  const c = Math.abs(Math.cos(r));
  return [w * c + h * s, w * s + h * c];
}

/** Whether `crop` lies inside the picture turned by `deg`. */
export function cropInside(w: number, h: number, deg: number, crop: Rect): boolean {
  const [bw, bh] = rotatedSize(w, h, deg);
  const r = (deg * Math.PI) / 180;
  const s = Math.sin(r);
  const c = Math.cos(r);
  const eps = 0.5 + 1e-6 * (w + h);
  const corners: [number, number][] = [
    [crop.x, crop.y],
    [crop.x + crop.w, crop.y],
    [crop.x, crop.y + crop.h],
    [crop.x + crop.w, crop.y + crop.h],
  ];
  return corners.every(([fx, fy]) => {
    const px = fx * bw - bw / 2;
    const py = fy * bh - bh / 2;
    // Back into the picture's own frame.
    const ox = px * c + py * s;
    const oy = -px * s + py * c;
    return Math.abs(ox) <= w / 2 + eps && Math.abs(oy) <= h / 2 + eps;
  });
}

/** Whether `crop` is acceptable: inside the box, and inside the picture unless `expand`. */
export function cropOk(w: number, h: number, deg: number, expand: boolean, crop: Rect): boolean {
  const e = 1e-9;
  if (crop.w <= 0 || crop.h <= 0 || crop.x < -e || crop.y < -e || crop.x + crop.w > 1 + e || crop.y + crop.h > 1 + e) return false;
  return expand || cropInside(w, h, deg, crop);
}

/** Largest crop centred in the box, with pixel aspect `aspect` (w/h) or the picture's own. */
export function largestCrop(w: number, h: number, deg: number, expand: boolean, aspect: number | null): Rect {
  const [bw, bh] = rotatedSize(w, h, deg);
  const a = aspect ?? w / h;
  // Size in box fractions at scale 1: as wide as the box, or as tall.
  let cw = 1;
  let ch = bw / a / bh;
  if (ch > 1) {
    ch = 1;
    cw = (bh * a) / bw;
  }
  const at = (s: number): Rect => ({ x: (1 - cw * s) / 2, y: (1 - ch * s) / 2, w: cw * s, h: ch * s });
  if (cropOk(w, h, deg, expand, at(1))) return at(1);
  let lo = 0;
  let hi = 1;
  for (let i = 0; i < 30; i++) {
    const mid = (lo + hi) / 2;
    if (cropOk(w, h, deg, expand, at(mid))) lo = mid;
    else hi = mid;
  }
  return at(lo);
}

/** `crop` shrunk about its centre (or, if that cannot work, the box's) until acceptable. */
export function fitCrop(w: number, h: number, deg: number, expand: boolean, crop: Rect): Rect {
  if (cropOk(w, h, deg, expand, crop)) return crop;
  const about = (cx: number, cy: number, s: number): Rect => ({ x: cx - (crop.w * s) / 2, y: cy - (crop.h * s) / 2, w: crop.w * s, h: crop.h * s });
  for (const [cx, cy] of [
    [crop.x + crop.w / 2, crop.y + crop.h / 2],
    [0.5, 0.5],
  ]) {
    if (!cropOk(w, h, deg, expand, about(cx, cy, 0.01))) continue;
    let lo = 0.01;
    let hi = 1;
    for (let i = 0; i < 30; i++) {
      const mid = (lo + hi) / 2;
      if (cropOk(w, h, deg, expand, about(cx, cy, mid))) lo = mid;
      else hi = mid;
    }
    return about(cx, cy, lo);
  }
  return largestCrop(w, h, deg, expand, null);
}

/** The furthest point from `from` toward `to` (both acceptable or not) that is acceptable. */
export function towards(w: number, h: number, deg: number, expand: boolean, from: Rect, to: Rect): Rect {
  if (cropOk(w, h, deg, expand, to)) return to;
  const mix = (t: number): Rect => ({
    x: from.x + (to.x - from.x) * t,
    y: from.y + (to.y - from.y) * t,
    w: from.w + (to.w - from.w) * t,
    h: from.h + (to.h - from.h) * t,
  });
  let lo = 0;
  let hi = 1;
  for (let i = 0; i < 20; i++) {
    const mid = (lo + hi) / 2;
    if (cropOk(w, h, deg, expand, mix(mid))) lo = mid;
    else hi = mid;
  }
  return mix(lo);
}

/** The crop after a quarter turn (`dir` 1 clockwise, -1 counter-clockwise) of the whole box. */
export function turnCrop(c: Rect, dir: 1 | -1): Rect {
  return dir === 1 ? { x: 1 - c.y - c.h, y: c.x, w: c.h, h: c.w } : { x: c.y, y: 1 - c.x - c.w, w: c.h, h: c.w };
}

export function isFull(c: Rect): boolean {
  const e = 1e-6;
  return c.x <= e && c.y <= e && c.w >= 1 - e && c.h >= 1 - e;
}
