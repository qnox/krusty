// The Bazel registry generator: which releases become registry versions, and the exact files.
import { test } from "node:test";
import assert from "node:assert/strict";
import { compareVersions, integrity, registryFiles } from "../scripts/bazel-registry.mjs";

const HEX = "e4bb5022b320848d25df237f95cce326b1771adb09c2ca0f75fb0c64acc73859";
const URL_BASE = "https://github.com/qnox/krusty/releases/download";

function release(version, names) {
  return {
    tag_name: `v${version}`,
    draft: false,
    assets: names.map((name) => ({ name, browser_download_url: `${URL_BASE}/v${version}/${name}` })),
  };
}

function bazelRelease(version) {
  return release(version, [
    `krusty-${version}-x86_64-unknown-linux-gnu.tar.gz`,
    `krusty-bazel-${version}.tar.gz`,
    `krusty-bazel-${version}.tar.gz.sha256`,
    `krusty-bazel-${version}.MODULE.bazel`,
  ]);
}

async function fetchText(url) {
  if (url.endsWith(".sha256")) return `${HEX}  ${url.split("/").pop().replace(/\.sha256$/, "")}\n`;
  if (url.endsWith(".MODULE.bazel")) return `module(name = "krusty", version = "${url.split("/").at(-2).slice(1)}")\n`;
  throw new Error(`unexpected download ${url}`);
}

test("versions follow Bazel's order, numeric prerelease identifiers included", () => {
  const versions = ["2.4.21-build.10", "2.4.3-build.1", "2.4.21-build.9", "2.4.21", "2.4.21-build.a"];
  assert.deepEqual(versions.sort(compareVersions), [
    "2.4.3-build.1",
    "2.4.21-build.9",
    "2.4.21-build.10",
    "2.4.21-build.a",
    "2.4.21",
  ]);
});

test("integrity is the base64 of the sha256sum digest", () => {
  assert.equal(integrity(`${HEX}  krusty-bazel-1.tar.gz\n`), "sha256-5LtQIrMghI0l3yN/lczjJrF3GtsJwsoPdfsMZKzHOFk=");
  assert.throws(() => integrity("nope  x"), /not a sha256sum line/);
});

test("every release with a Bazel module becomes one registry version", async () => {
  const files = await registryFiles(
    [
      bazelRelease("2.4.21-build.10"),
      release("2.4.21-build.8", ["krusty-2.4.21-build.8-x86_64-unknown-linux-gnu.tar.gz"]),
      { ...bazelRelease("2.4.21-build.11"), draft: true },
      bazelRelease("2.4.21-build.9"),
    ],
    fetchText,
  );
  assert.deepEqual([...files.keys()].sort(), [
    "bazel_registry.json",
    "modules/krusty/2.4.21-build.10/MODULE.bazel",
    "modules/krusty/2.4.21-build.10/source.json",
    "modules/krusty/2.4.21-build.9/MODULE.bazel",
    "modules/krusty/2.4.21-build.9/source.json",
    "modules/krusty/metadata.json",
  ]);
  assert.equal(files.get("bazel_registry.json"), "{}\n");
  assert.equal(
    files.get("modules/krusty/2.4.21-build.9/MODULE.bazel"),
    'module(name = "krusty", version = "2.4.21-build.9")\n',
  );
  assert.equal(
    files.get("modules/krusty/2.4.21-build.9/source.json"),
    `{\n  "url": "${URL_BASE}/v2.4.21-build.9/krusty-bazel-2.4.21-build.9.tar.gz",\n` +
      `  "integrity": "sha256-5LtQIrMghI0l3yN/lczjJrF3GtsJwsoPdfsMZKzHOFk="\n}\n`,
  );
  assert.equal(
    files.get("modules/krusty/metadata.json"),
    `{\n  "homepage": "https://krustythecompiler.dev",\n  "maintainers": [],\n` +
      `  "repository": [\n    "github:qnox/krusty"\n  ],\n` +
      `  "versions": [\n    "2.4.21-build.9",\n    "2.4.21-build.10"\n  ],\n  "yanked_versions": {}\n}\n`,
  );
});

test("a release with only part of the module is an error, not a silent gap", async () => {
  await assert.rejects(
    registryFiles([release("1.0.0", ["krusty-bazel-1.0.0.tar.gz"])], fetchText),
    { message: "release v1.0.0 has only part of the Bazel module" },
  );
});
