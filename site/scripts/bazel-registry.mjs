// Writes the krusty Bazel registry (https://bazel.build/external/registry) into the built site,
// from the GitHub releases that published a Bazel module. Each such release carries
// krusty-bazel-<version>.tar.gz, its .sha256, and the stamped krusty-bazel-<version>.MODULE.bazel
// (scripts/package-bazel-module.sh). The registry is rebuilt from the releases on every deploy,
// so it holds exactly the published versions.
//
//   node scripts/bazel-registry.mjs OUTPUT_DIRECTORY
//
// GITHUB_TOKEN authenticates the release listing; GITHUB_REPOSITORY (owner/name) defaults to
// qnox/krusty.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const MODULE = "krusty";

/** Bazel's module version order: dot-separated release identifiers, then the prerelease. */
export function compareVersions(a, b) {
  const split = (version) => {
    const [release, prerelease] = version.split("+")[0].split(/-(.*)/s);
    return { release: release.split("."), prerelease: prerelease === undefined ? null : prerelease.split(".") };
  };
  const identifiers = (x, y) => {
    for (let i = 0; i < Math.max(x.length, y.length); i++) {
      if (x[i] === undefined) return -1;
      if (y[i] === undefined) return 1;
      const xn = /^\d+$/.test(x[i]);
      const yn = /^\d+$/.test(y[i]);
      if (xn && yn) {
        const d = Number(x[i]) - Number(y[i]);
        if (d !== 0) return Math.sign(d);
      } else if (xn !== yn) {
        return xn ? -1 : 1;
      } else if (x[i] !== y[i]) {
        return x[i] < y[i] ? -1 : 1;
      }
    }
    return 0;
  };
  const left = split(a);
  const right = split(b);
  const release = identifiers(left.release, right.release);
  if (release !== 0) return release;
  if (left.prerelease === null || right.prerelease === null) {
    return (left.prerelease === null) - (right.prerelease === null);
  }
  return identifiers(left.prerelease, right.prerelease);
}

/** `sha256-<base64>` from a `sha256sum` line. */
export function integrity(sha256sumLine) {
  const hex = sha256sumLine.trim().split(/\s+/)[0];
  if (!/^[0-9a-f]{64}$/.test(hex)) throw new Error(`not a sha256sum line: ${sha256sumLine}`);
  return `sha256-${Buffer.from(hex, "hex").toString("base64")}`;
}

/**
 * The registry's files, as a map from path to contents, for `releases` (GitHub release objects).
 * `fetchText(url)` downloads one release asset.
 */
export async function registryFiles(releases, fetchText) {
  const files = new Map();
  const versions = [];
  for (const release of releases) {
    if (release.draft) continue;
    const version = release.tag_name.replace(/^v/, "");
    const asset = (name) => release.assets.find((candidate) => candidate.name === name);
    const archive = asset(`krusty-bazel-${version}.tar.gz`);
    const checksum = asset(`krusty-bazel-${version}.tar.gz.sha256`);
    const module = asset(`krusty-bazel-${version}.MODULE.bazel`);
    if (!archive && !checksum && !module) continue;
    if (!archive || !checksum || !module) {
      throw new Error(`release ${release.tag_name} has only part of the Bazel module`);
    }
    const base = `modules/${MODULE}/${version}`;
    files.set(`${base}/MODULE.bazel`, await fetchText(module.browser_download_url));
    files.set(
      `${base}/source.json`,
      `${JSON.stringify(
        { url: archive.browser_download_url, integrity: integrity(await fetchText(checksum.browser_download_url)) },
        null,
        2,
      )}\n`,
    );
    versions.push(version);
  }
  versions.sort(compareVersions);
  files.set(
    `modules/${MODULE}/metadata.json`,
    `${JSON.stringify(
      {
        homepage: "https://krustythecompiler.dev",
        maintainers: [],
        repository: ["github:qnox/krusty"],
        versions,
        yanked_versions: {},
      },
      null,
      2,
    )}\n`,
  );
  files.set("bazel_registry.json", "{}\n");
  return files;
}

async function main(output) {
  const repository = process.env.GITHUB_REPOSITORY ?? "qnox/krusty";
  const headers = { Accept: "application/vnd.github+json" };
  if (process.env.GITHUB_TOKEN) headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  const get = async (url, accept) => {
    const response = await fetch(url, { headers: accept ? { ...headers, Accept: accept } : headers });
    if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
    return response;
  };
  const releases = [];
  for (let page = 1; ; page++) {
    const batch = await (await get(`https://api.github.com/repos/${repository}/releases?per_page=100&page=${page}`)).json();
    releases.push(...batch);
    if (batch.length < 100) break;
  }
  const files = await registryFiles(releases, async (url) => (await get(url, "application/octet-stream")).text());
  for (const [path, contents] of files) {
    mkdirSync(join(output, path, ".."), { recursive: true });
    writeFileSync(join(output, path), contents);
  }
  const count = JSON.parse(files.get(`modules/${MODULE}/metadata.json`)).versions.length;
  console.log(`bazel registry: ${count} version(s) of ${MODULE} in ${output}`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  if (process.argv.length !== 3) {
    console.error("usage: node scripts/bazel-registry.mjs OUTPUT_DIRECTORY");
    process.exit(2);
  }
  await main(process.argv[2]);
}
