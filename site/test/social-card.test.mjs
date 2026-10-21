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

test("X selects a large-image card and finds its own tags without falling back to Open Graph", () => {
  assert.equal(meta("name", "twitter:card"), "summary_large_image");
  assert.equal(meta("name", "twitter:title"), meta("property", "og:title"));
  assert.equal(meta("name", "twitter:description"), meta("property", "og:description"));
  assert.equal(meta("name", "twitter:image"), meta("property", "og:image"));
  assert.equal(meta("name", "twitter:image:alt"), meta("property", "og:image:alt"));
  assert.equal(meta("name", "twitter:site"), "@qnox");
  assert.equal(meta("name", "twitter:creator"), "@qnox");
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
  // The 1.91:1 card size X and Open Graph render uncropped; X's minimum is 300x157.
  assert.equal(`${width}x${height}`, "1200x630");
  assert.equal(meta("property", "og:image:type"), "image/png");
  assert.ok(meta("property", "og:image:alt"));
});
