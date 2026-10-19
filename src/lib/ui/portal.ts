/**
 * Moves an element to `<body>` so floating UI (dropdowns) is not clipped by scrolling or
 * `overflow: hidden` ancestors, nor re-anchored by transformed / backdrop-filtered ones.
 */
export function portal(node: HTMLElement) {
  document.body.appendChild(node);
  return {
    destroy() {
      node.remove();
    },
  };
}
