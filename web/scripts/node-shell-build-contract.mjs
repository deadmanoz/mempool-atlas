import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const webRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function attribute(tag, name) {
  const match = tag.match(new RegExp(`\\b${name}=["']([^"']+)["']`, "i"));
  return match?.[1] ?? null;
}

test("the node build exposes its shell CSS before the document body", () => {
  const html = readFileSync(resolve(webRoot, "dist/index.html"), "utf8");
  const head = html.match(/<head\b[^>]*>([\s\S]*?)<\/head>/i)?.[1];
  assert.ok(head, "dist/index.html must contain a head element");

  const stylesheetHrefs = [...head.matchAll(/<link\b[^>]*>/gi)]
    .filter((match) => attribute(match[0], "rel") === "stylesheet")
    .map((match) => attribute(match[0], "href"))
    .filter((href) => href !== null);

  assert.ok(
    stylesheetHrefs.length > 0,
    "dist/index.html must expose render-blocking node shell CSS in its head",
  );

  const css = stylesheetHrefs
    .map((href) => {
      assert.match(href, /^\/assets\/[^/?#]+\.css$/);
      return readFileSync(resolve(webRoot, "dist", `.${href}`), "utf8");
    })
    .join("\n");

  assert.match(css, /\.shell\{/);
  assert.match(css, /\.node-source-picker\{/);
  assert.match(css, /\.source-summary\{/);
});
