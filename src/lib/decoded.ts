/**
 * Neighbouring pictures the viewer panes keep decoded in the webview, shared between
 * the panes of a window: two panes on the same list hold one image each, not two.
 * Each pane (`owner`) names what it wants kept; an image goes once no pane wants it.
 */
const images = new Map<string, { img: HTMLImageElement; owners: Set<object> }>();

/** Keeps `urls` decoded for `owner` (and lets go of the rest it held). */
export function keepDecoded(owner: object, urls: Iterable<string>) {
  const want = new Set(urls);
  for (const url of want) {
    let e = images.get(url);
    if (!e) {
      const img = new Image();
      img.src = url;
      img.decode().catch(() => {});
      e = { img, owners: new Set() };
      images.set(url, e);
    }
    e.owners.add(owner);
  }
  for (const [url, e] of images) {
    if (want.has(url) || !e.owners.delete(owner)) continue;
    if (e.owners.size === 0) images.delete(url);
  }
}
