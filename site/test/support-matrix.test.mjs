// The README and the landing page both list the supported build systems and compiler plugins.
// The README is the written source; this checks the built page names every row the README does,
// so the two cannot drift apart silently. Run after `npm run build`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const html = readFileSync(new URL("../dist/index.html", import.meta.url), "utf8");
const readme = readFileSync(new URL("../../README.md", import.meta.url), "utf8");

function section(markdown, heading) {
  const start = markdown.indexOf(`\n### ${heading}\n`);
  assert.notEqual(start, -1, `README has no "### ${heading}" section`);
  const rest = markdown.slice(start + heading.length + 6);
  const end = rest.search(/\n#{2,3} /);
  return end === -1 ? rest : rest.slice(0, end);
}

function rows(markdown) {
  return markdown
    .split("\n")
    .filter((line) => line.startsWith("| ") && !line.startsWith("| ---") && !/^\| (Build system|Plugin) \|/.test(line))
    .map((line) => line.slice(2, line.indexOf(" |", 2)).replace(/`([^`]*)`/g, "$1"));
}

const text = html.replace(/<[^>]+>/g, "").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"');

for (const heading of ["Build systems", "Compiler plugins"]) {
  test(`the page lists every ${heading.toLowerCase()} row of the README`, () => {
    const names = rows(section(readme, heading));
    assert.ok(names.length >= 5, `README "${heading}" table has only ${names.length} rows`);
    for (const name of names) {
      assert.ok(text.includes(name), `page is missing the "${name}" row`);
    }
  });
}
