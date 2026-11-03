/**
 * Temporary groups: items picked by hand, in the order they were selected, listed in the
 * sidebar under the saved searches. Kept on this device per library (WebView storage)
 * until deleted; every window of the app sees the same list (`storage` events).
 */
export type TempGroup = { id: string; name: string; ids: number[] };

const KEY = (bundle: string) => `vp.groups:${bundle}`;

class Groups {
  list = $state<TempGroup[]>([]);
  #bundle = "";

  /** Reads the groups of the library at `bundle` (its path). */
  open(bundle: string) {
    this.#bundle = bundle;
    this.#read();
  }

  get(id: string | null): TempGroup | undefined {
    return id == null ? undefined : this.list.find((g) => g.id === id);
  }

  /** A new group of `ids` (in this order), named "Group N" by `name(n)`. */
  add(ids: number[], name: (n: number) => string): TempGroup {
    const used = new Set(this.list.map((g) => g.name));
    let n = this.list.length + 1;
    while (used.has(name(n))) n++;
    const g = { id: `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`, name: name(n), ids: [...new Set(ids)] };
    this.list = [...this.list, g];
    this.#write();
    return g;
  }

  remove(id: string) {
    this.list = this.list.filter((g) => g.id !== id);
    this.#write();
  }

  #read() {
    try {
      const v = JSON.parse(localStorage.getItem(KEY(this.#bundle)) ?? "[]");
      this.list = Array.isArray(v) ? v.filter((g) => g && typeof g.id === "string" && Array.isArray(g.ids)) : [];
    } catch {
      this.list = [];
    }
  }

  #write() {
    try {
      localStorage.setItem(KEY(this.#bundle), JSON.stringify(this.list));
    } catch {
      // Storage unavailable: the groups last as long as the window.
    }
  }

  constructor() {
    if (typeof window !== "undefined")
      window.addEventListener("storage", (e) => {
        if (this.#bundle && e.key === KEY(this.#bundle)) this.#read();
      });
  }
}

export const groups = new Groups();
