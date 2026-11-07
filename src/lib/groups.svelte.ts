/**
 * Temporary groups: items picked by hand, in the order they were selected, listed in the
 * sidebar under the saved searches. Kept in the library (`temp_group` table) until deleted,
 * so every device that opens the library sees them; other windows hear of changes through
 * `sync` ("groups").
 */
import { api, type TempGroup } from "./api";
import { broadcast, onSync } from "./sync";

export type { TempGroup };

/** Where groups lived before they moved into the library (this device's WebView storage). */
const LEGACY_KEY = (bundle: string) => `vp.groups:${bundle}`;

class Groups {
  list = $state<TempGroup[]>([]);
  #bundle = "";
  #listening = false;
  /** Serializes saves so an older list never lands after a newer one. */
  #saving: Promise<unknown> = Promise.resolve();

  /** Reads the groups of the library at `bundle` (its path). */
  open(bundle: string) {
    this.#bundle = bundle;
    this.list = [];
    if (!this.#listening && typeof window !== "undefined") {
      this.#listening = true;
      onSync("groups", () => this.#read()).catch(() => {});
    }
    this.#read().then(() => this.#importLegacy(bundle));
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

  /** Appends `ids` (in this order) to group `id`, leaving out those already in it. */
  addItems(id: string, ids: number[]) {
    this.#update(id, (list) => [...list, ...ids.filter((x) => !list.includes(x))]);
  }

  /** Takes `ids` out of group `id` (the media stays in the library). */
  removeItems(id: string, ids: number[]) {
    const gone = new Set(ids);
    this.#update(id, (list) => list.filter((x) => !gone.has(x)));
  }

  /** Group `id` in a new order: `shown` first, then the rest (items no longer found) as they were. */
  reorder(id: string, shown: number[]) {
    this.#update(id, (list) => {
      const set = new Set(shown);
      return [...shown, ...list.filter((x) => !set.has(x))];
    });
  }

  #update(id: string, f: (ids: number[]) => number[]) {
    this.list = this.list.map((g) => (g.id === id ? { ...g, ids: f(g.ids) } : g));
    this.#write();
  }

  remove(id: string) {
    this.list = this.list.filter((g) => g.id !== id);
    this.#write();
  }

  async #read() {
    const bundle = this.#bundle;
    try {
      const list = await api.listTempGroups();
      if (bundle === this.#bundle) this.list = list;
    } catch {
      // No library open (yet): keep what we have.
    }
  }

  #write() {
    // A window that never loaded the list (a detached viewer) must not overwrite it.
    if (!this.#bundle) return;
    const snapshot = $state.snapshot(this.list) as TempGroup[];
    this.#saving = this.#saving
      .then(() => api.saveTempGroups(snapshot))
      .then(() => broadcast("groups"))
      .catch(() => {});
  }

  /** Moves groups made before they were kept in the library into it, once. */
  #importLegacy(bundle: string) {
    if (bundle !== this.#bundle) return;
    try {
      const raw = localStorage.getItem(LEGACY_KEY(bundle));
      if (raw == null) return;
      const v = JSON.parse(raw);
      const old: TempGroup[] = Array.isArray(v)
        ? v.filter((g) => g && typeof g.id === "string" && Array.isArray(g.ids)).map((g) => ({ id: g.id, name: String(g.name ?? ""), ids: g.ids }))
        : [];
      const have = new Set(this.list.map((g) => g.id));
      const add = old.filter((g) => !have.has(g.id));
      if (add.length) {
        this.list = [...this.list, ...add];
        this.#write();
      }
      // Forget the old copy only once the library has it.
      this.#saving = this.#saving.then(() => {
        if (add.every((g) => this.list.some((x) => x.id === g.id))) localStorage.removeItem(LEGACY_KEY(bundle));
      });
    } catch {
      // Storage unavailable: nothing to import.
    }
  }
}

export const groups = new Groups();
