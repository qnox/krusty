// The Bazel registry generator: which releases become registry versions, and the exact files.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { archivedFile, compareVersions, integrity, registryFiles } from "../scripts/bazel-registry.mjs";

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

async function fetchAsset(url) {
  if (url.endsWith(".sha256")) return Buffer.from(`${HEX}  ${url.split("/").pop().replace(/\.sha256$/, "")}\n`);
  if (url.endsWith(".MODULE.bazel")) {
    return Buffer.from(`module(name = "krusty", version = "${url.split("/").at(-2).slice(1)}")\n`);
  }
  throw new Error(`unexpected download ${url}`);
}

/** A module archive packed the way scripts/package-bazel-module.sh packs one. */
function moduleArchive(files) {
  const directory = mkdtempSync(join(tmpdir(), "krusty-bazel-registry-"));
  try {
    for (const [name, contents] of Object.entries(files)) writeFileSync(join(directory, name), contents);
    const tar = execFileSync("tar", ["--owner=0", "--group=0", "--numeric-owner", "--mtime=@0", "-C", directory, "-cf", "-", ...Object.keys(files)]);
    return execFileSync("gzip", ["-n", "-9"], { input: tar });
  } finally {
    rmSync(directory, { recursive: true });
  }
}

/** A release from before the registry: the archive and its checksum, without the module asset. */
function archivedRelease(version, archive, sum = createHash("sha256").update(archive).digest("hex")) {
  const assets = release(version, [`krusty-bazel-${version}.tar.gz`, `krusty-bazel-${version}.tar.gz.sha256`]);
  const fetch = async (url) => {
    if (url.endsWith(".tar.gz")) return archive;
    if (url.endsWith(".sha256")) return Buffer.from(`${sum}  krusty-bazel-${version}.tar.gz\n`);
    throw new Error(`unexpected download ${url}`);
  };
  return { release: assets, fetch };
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
    fetchAsset,
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

test("a release from before the registry serves the MODULE.bazel inside its verified archive", async () => {
  const module = 'module(name = "krusty", version = "2.4.21-build.3")\n';
  const archive = moduleArchive({ "BUILD.bazel": "# build\n", "MODULE.bazel": module, "defs.bzl": "# defs\n" });
  const { release: published, fetch } = archivedRelease("2.4.21-build.3", archive);
  const files = await registryFiles([published], fetch);
  assert.equal(files.get("modules/krusty/2.4.21-build.3/MODULE.bazel"), module);
  assert.equal(
    files.get("modules/krusty/2.4.21-build.3/source.json"),
    `{\n  "url": "${URL_BASE}/v2.4.21-build.3/krusty-bazel-2.4.21-build.3.tar.gz",\n` +
      `  "integrity": "sha256-${createHash("sha256").update(archive).digest("base64")}"\n}\n`,
  );
});

test("an archive that does not match its checksum is an error", async () => {
  const archive = moduleArchive({ "MODULE.bazel": 'module(name = "krusty")\n' });
  const { release: published, fetch } = archivedRelease("1.0.0", archive, HEX);
  await assert.rejects(registryFiles([published], fetch), {
    message: "release v1.0.0: krusty-bazel-1.0.0.tar.gz does not match its .sha256",
  });
});

test("archivedFile reads one root file of the archive, and nothing else", () => {
  const archive = moduleArchive({ "BUILD.bazel": "x".repeat(1000), "MODULE.bazel": "module()\n" });
  assert.equal(archivedFile(archive, "MODULE.bazel"), "module()\n");
  assert.equal(archivedFile(archive, "BUILD.bazel"), "x".repeat(1000));
  assert.throws(() => archivedFile(archive, "README.md"), { message: "the archive has no README.md" });
});

test("a release with only part of the module is an error, not a silent gap", async () => {
  const partial = [["tar.gz"], ["tar.gz.sha256"], ["MODULE.bazel"], ["tar.gz", "MODULE.bazel"], ["tar.gz.sha256", "MODULE.bazel"]];
  for (const present of partial) {
    await assert.rejects(
      registryFiles([release("1.0.0", present.map((suffix) => `krusty-bazel-1.0.0.${suffix}`))], fetchAsset),
      { message: "release v1.0.0 has only part of the Bazel module" },
      `assets ${present.join(", ")}`,
    );
  }
});
