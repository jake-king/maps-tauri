// Every Mermaid diagram in the book parses, with the same Mermaid the book's pages render with:
// tools/docs.sh serves node_modules/mermaid's build (pinned in package.json) in place of the older
// copy mdbook-mermaid bundles. The browser draws the diagrams, so without this a syntax error would
// build cleanly and show as an error box.
// `{{#include FILE}}` lines are expanded as mdBook does, relative to the chapter.
//   node tools/mermaid_check.mjs [DIR]      exit 0 all parse · 1 any does not · 2 none found
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { JSDOM } from "jsdom";

// Mermaid sanitises HTML labels (<br/>) with DOMPurify, which needs a DOM; Node has none
const { window } = new JSDOM("");
globalThis.window = window;
globalThis.document = window.document;
const { default: mermaid } = await import("mermaid");

const dir = process.argv[2] ?? "book/src";
let count = 0;
let bad = 0;
for (const name of readdirSync(dir).filter((f) => f.endsWith(".md")).sort()) {
  const path = join(dir, name);
  const lines = readFileSync(path, "utf8").split("\n");
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].trim() !== "```mermaid") continue;
    const start = i + 1;
    const body = [];
    for (i++; i < lines.length && lines[i].trim() !== "```"; i++) {
      const inc = lines[i].match(/^\s*\{\{#include (.+)\}\}\s*$/);
      body.push(inc ? readFileSync(join(dirname(path), inc[1]), "utf8") : lines[i]);
    }
    count++;
    try {
      await mermaid.parse(body.join("\n"));
    } catch (e) {
      bad++;
      console.log(`mermaid: ${path}:${start}: ${String(e.message).split("\n").slice(0, 3).join(" ")}`);
    }
  }
}
console.log(`mermaid: ${count} diagrams, ${bad} that do not parse`);
process.exit(count === 0 ? 2 : bad ? 1 : 0);
