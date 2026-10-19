/**
 * Zoom and pan for one image. Scale 1 is fit-to-screen; positions are in CSS pixels
 * relative to the stage center. The pan is clamped so the image never leaves an empty
 * gap on a side it overflows.
 */
export class ZoomPan {
  scale = $state(1);
  x = $state(0);
  y = $state(0);

  /** Stage size and the image's natural size, set by the viewer. */
  stage = { w: 0, h: 0 };
  natural = { w: 0, h: 0 };

  constructor(private max: () => number) {}

  get zoomed() {
    return this.scale > 1.001;
  }

  reset() {
    this.scale = 1;
    this.x = 0;
    this.y = 0;
  }

  /** Displayed size at scale 1 (object-fit: contain). */
  private fit() {
    const { w, h } = this.natural;
    if (!w || !h || !this.stage.w || !this.stage.h) return { w: this.stage.w, h: this.stage.h };
    const k = Math.min(this.stage.w / w, this.stage.h / h);
    return { w: w * k, h: h * k };
  }

  private clamp() {
    const f = this.fit();
    const mx = Math.max(0, (f.w * this.scale - this.stage.w) / 2);
    const my = Math.max(0, (f.h * this.scale - this.stage.h) / 2);
    this.x = Math.min(mx, Math.max(-mx, this.x));
    this.y = Math.min(my, Math.max(-my, this.y));
  }

  /** Sets the scale keeping the point (`px`, `py`, relative to the stage center) in place. */
  zoomTo(scale: number, px = 0, py = 0) {
    const next = Math.min(this.max(), Math.max(1, scale));
    const r = next / this.scale;
    this.x = px - (px - this.x) * r;
    this.y = py - (py - this.y) * r;
    this.scale = next;
    if (!this.zoomed) this.reset();
    else this.clamp();
  }

  zoomBy(factor: number, px = 0, py = 0) {
    this.zoomTo(this.scale * factor, px, py);
  }

  panBy(dx: number, dy: number) {
    this.x += dx;
    this.y += dy;
    this.clamp();
  }

  get transform() {
    return `translate(${this.x}px, ${this.y}px) scale(${this.scale})`;
  }
}
