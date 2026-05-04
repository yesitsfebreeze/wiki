"use strict";

/*
 * Graph Tag Colors
 *
 * Colors tag-type nodes in Obsidian's graph view to match `graph.json`
 * `colorGroups` queries of the form `tag:#X`, applied at half brightness so
 * tag nodes are visibly subordinate to their member notes. Tags without a
 * matching color group fall back to the CSS variable `--graph-node-tag`.
 *
 * Why hook the renderer instead of using colorGroups directly: Obsidian's
 * built-in colorGroups system colors *file* nodes whose frontmatter/tags
 * match a query — it does not color the tag-pseudo-nodes that appear when
 * `showTags` is on. Recoloring those requires walking `renderer.nodes` and
 * overriding `node.color` after each render iteration.
 */

const { Plugin } = require("obsidian");

const GRAPH_VIEW_TYPE = "graph";
const TAG_QUERY_RE = /tag:\s*#?([\p{L}\p{N}_/\-]+)/giu;

class GraphTagColorsPlugin extends Plugin {
  async onload() {
    this.tagColors = new Map();
    this.patched = new WeakSet();

    await this.loadTagColors();

    this.registerEvent(
      this.app.workspace.on("layout-change", () => this.patchAllGraphLeaves()),
    );
    this.registerEvent(
      this.app.workspace.on("active-leaf-change", () => this.patchAllGraphLeaves()),
    );

    this.app.workspace.onLayoutReady(() => this.patchAllGraphLeaves());
  }

  onunload() {
    this.app.workspace.getLeavesOfType(GRAPH_VIEW_TYPE).forEach((leaf) => {
      const renderer = leaf?.view?.renderer;
      if (renderer && renderer._gtcOriginalIteration) {
        renderer.onIteration = renderer._gtcOriginalIteration;
        delete renderer._gtcOriginalIteration;
      }
    });
  }

  async loadTagColors() {
    this.tagColors.clear();
    let raw;
    try {
      raw = await this.app.vault.adapter.read(".obsidian/graph.json");
    } catch {
      return;
    }
    let cfg;
    try {
      cfg = JSON.parse(raw);
    } catch {
      return;
    }
    const groups = Array.isArray(cfg?.colorGroups) ? cfg.colorGroups : [];
    for (const group of groups) {
      const query = typeof group?.query === "string" ? group.query : "";
      const rgb = group?.color?.rgb;
      if (typeof rgb !== "number") continue;
      const halved = halveBrightness(rgb);
      for (const tag of extractTagNames(query)) {
        this.tagColors.set(tag.toLowerCase(), halved);
      }
    }
  }

  patchAllGraphLeaves() {
    const leaves = this.app.workspace.getLeavesOfType(GRAPH_VIEW_TYPE);
    for (const leaf of leaves) this.patchLeaf(leaf);
  }

  patchLeaf(leaf) {
    const renderer = leaf?.view?.renderer;
    if (!renderer || this.patched.has(renderer)) return;
    this.patched.add(renderer);

    const original = renderer.onIteration?.bind(renderer);
    renderer._gtcOriginalIteration = renderer.onIteration;

    const apply = () => this.applyColors(renderer);

    renderer.onIteration = function patchedIteration() {
      const result = original ? original.apply(this, arguments) : undefined;
      apply();
      return result;
    };

    apply();
  }

  applyColors(renderer) {
    const nodes = renderer?.nodes;
    if (!Array.isArray(nodes) || this.tagColors.size === 0) return;
    for (const node of nodes) {
      if (!node || node.type !== "tag") continue;
      const name = tagNameOf(node);
      if (!name) continue;
      const color = this.tagColors.get(name.toLowerCase());
      if (color === undefined) continue;
      node.color = { a: 1, rgb: color };
    }
  }
}

function extractTagNames(query) {
  const out = [];
  TAG_QUERY_RE.lastIndex = 0;
  let m;
  while ((m = TAG_QUERY_RE.exec(query)) !== null) out.push(m[1]);
  return out;
}

function halveBrightness(rgb) {
  return (rgb >> 1) & 0x7f7f7f;
}

function tagNameOf(node) {
  const id = typeof node.id === "string" ? node.id : "";
  if (!id) return null;
  if (id.startsWith("tag:#")) return id.slice(5);
  if (id.startsWith("tag:")) return id.slice(4);
  if (id.startsWith("#")) return id.slice(1);
  return null;
}

module.exports = GraphTagColorsPlugin;
