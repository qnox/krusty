// Writes the krusty Bazel registry (https://bazel.build/external/registry) into the built site,
// from the GitHub releases that published a Bazel module. Each such release carries
// krusty-bazel-<version>.tar.gz and its .sha256, and releases since the registry also carry the
// stamped krusty-bazel-<version>.MODULE.bazel (scripts/package-bazel-module.sh). For an earlier
// release the registry's MODULE.bazel is the one inside its archive, read after the archive's
// digest is checked against the .sha256. The registry is rebuilt from the releases on every
// deploy, so it holds exactly the published versions.
//
//   node scripts/bazel-registry.mjs OUTPUT_DIRECTORY
//
// GITHUB_TOKEN authenticates the release listing; GITHUB_REPOSITORY (owner/name) defaults to
// qnox/krusty.
import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { gunzipSync } from "node:zlib";

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

/** The hex digest of a `sha256sum` line. */
function digest(sha256sumLine) {
  const hex = sha256sumLine.trim().split(/\s+/)[0];
  if (!/^[0-9a-f]{64}$/.test(hex)) throw new Error(`not a sha256sum line: ${sha256sumLine}`);
  return hex;
}

/** `sha256-<base64>` from a `sha256sum` line. */
export function integrity(sha256sumLine) {
  return `sha256-${Buffer.from(digest(sha256sumLine), "hex").toString("base64")}`;
}

/** The contents of the regular file `name` at the root of a gzipped tar archive. */
export function archivedFile(archive, name) {
  const tar = gunzipSync(archive);
  for (let offset = 0; offset + 512 <= tar.length; ) {
    const header = tar.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) break;
    const field = (start, length) => header.subarray(start, start + length).toString("latin1").replace(/\0.*$/s, "");
    const size = Number.parseInt(field(124, 12).trim(), 8);
    const type = field(156, 1);
    if (field(0, 100) === name && (type === "0" || type === "")) {
      return tar.subarray(offset + 512, offset + 512 + size).toString("utf8");
    }
    offset += 512 + Math.ceil(size / 512) * 512;
  }
  throw new Error(`the archive has no ${name}`);
}

/**
 * The registry's files, as a map from path to contents, for `releases` (GitHub release objects).
 * `fetchAsset(url)` downloads one release asset as a Buffer.
 */
export async function registryFiles(releases, fetchAsset) {
  const fetchText = async (url) => (await fetchAsset(url)).toString("utf8");
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
    if (!archive || !checksum) {
      throw new Error(`release ${release.tag_name} has only part of the Bazel module`);
    }
    const sum = await fetchText(checksum.browser_download_url);
    const base = `modules/${MODULE}/${version}`;
    if (module) {
      files.set(`${base}/MODULE.bazel`, await fetchText(module.browser_download_url));
    } else {
      // Released before the module was published on its own: read it from the verified archive.
      const bytes = await fetchAsset(archive.browser_download_url);
      if (createHash("sha256").update(bytes).digest("hex") !== digest(sum)) {
        throw new Error(`release ${release.tag_name}: krusty-bazel-${version}.tar.gz does not match its .sha256`);
      }
      files.set(`${base}/MODULE.bazel`, archivedFile(bytes, "MODULE.bazel"));
    }
    files.set(
      `${base}/source.json`,
      `${JSON.stringify({ url: archive.browser_download_url, integrity: integrity(sum) }, null, 2)}\n`,
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
  const files = await registryFiles(releases, async (url) =>
    Buffer.from(await (await get(url, "application/octet-stream")).arrayBuffer()),
  );
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
