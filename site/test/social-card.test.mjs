// Checks the built landing page's link-preview metadata. Run after `npm run build`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const dist = new URL("../dist/", import.meta.url);
const html = readFileSync(new URL("index.html", dist), "utf8");

function meta(attr, key) {
  const tag = html.match(new RegExp(`<meta ${attr}="${key}" content="([^"]*)"`));
  assert.ok(tag, `missing <meta ${attr}="${key}">`);
  return tag[1];
}

function pngSize(bytes) {
  assert.equal(bytes.toString("latin1", 1, 4), "PNG");
  return [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
}

test("X selects a large-image card", () => {
  assert.equal(meta("name", "twitter:card"), "summary_large_image");
});

test("Open Graph names the page", () => {
  assert.equal(meta("property", "og:type"), "website");
  assert.ok(meta("property", "og:title"));
  assert.ok(meta("property", "og:description"));
});

test("og:image is an absolute URL to a built image of the declared size", () => {
  const image = new URL(meta("property", "og:image"));
  const site = new URL(meta("property", "og:url"));
  assert.equal(image.protocol, "https:");
  assert.equal(image.origin, site.origin);
  const [width, height] = pngSize(readFileSync(new URL(image.pathname.slice(1), dist)));
  assert.equal(Number(meta("property", "og:image:width")), width);
  assert.equal(Number(meta("property", "og:image:height")), height);
  // X's minimum for summary_large_image.
  assert.ok(width >= 300 && height >= 157, `${width}x${height} is below 300x157`);
  assert.ok(meta("property", "og:image:alt"));
});
