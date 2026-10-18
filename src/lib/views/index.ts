import type { Component } from "svelte";
import type { Key } from "../i18n.svelte";
import type { IconName } from "../ui/Icon.svelte";
import GridView from "./GridView.svelte";
import ListView from "./ListView.svelte";
import TreeView from "./TreeView.svelte";
import type { ViewProps } from "./types";

/** Available main views. A new view only needs a component taking `ViewProps` and an entry here. */
export const VIEWS: { key: "grid" | "list" | "tree"; icon: IconName; label: Key; component: Component<ViewProps> }[] = [
  { key: "grid", icon: "grid", label: "view.grid", component: GridView },
  { key: "list", icon: "list", label: "view.list", component: ListView },
  { key: "tree", icon: "tree", label: "view.tree", component: TreeView },
];
