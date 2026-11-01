// Reads the inputs of the public metrics page (/metrics/) from one directory and turns them into the
// page's data. The `pages` workflow fills the directory each night (see .github/workflows/pages.yml):
//
//   ci-run.json                                   the master `ci` run the box reports come from:
//                                                 {"databaseId", "headSha", "createdAt", "url"}
//   box/pct-<kotlin>/pct.txt                      JVM case report      "<pct> <passed> <applicable>"
//   box/jvm-byte-equality-<kotlin>/jvm-byte-equality.txt
//                                                 JVM byte report      "<pct> <matched> <total>"
//   box/native-pct-<kotlin>/native-pct.txt        Native case report   "<pct> <passed> <applicable>"
//   corpus/results.json                           the `corpus-results` branch's scoreboard
//   corpus/history.tsv                            the `corpus-results` branch's run history
//   coverage-baseline.json                        the coverage gate's recorded floor
//
// Every input is optional: one that is absent is reported as not measured, never guessed. One that
// is present but malformed throws, so a broken producer stops the deploy instead of publishing a
// wrong number.
//
//   node scripts/metrics.mjs INPUT_DIRECTORY     prints the collected data as JSON
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

// The box report kinds: artifact prefix, file name, and the names of the two counts
// (scripts/conformance-report.sh).
const BOX_REPORTS = {
  jvm: { prefix: "pct-", file: "pct.txt", count: "passed", of: "applicable" },
  bytes: { prefix: "jvm-byte-equality-", file: "jvm-byte-equality.txt", count: "matched", of: "total" },
  native: { prefix: "native-pct-", file: "native-pct.txt", count: "passed", of: "applicable" },
};

const KOTLIN_VERSION = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

/** One box report line, validated as scripts/conformance-report.sh validates it. */
export function parseBoxReport(text, path) {
  const match = /^(0|[1-9]\d{0,2})\.(\d) (0|[1-9]\d{0,14}) (0|[1-9]\d{0,14})$/.exec(text.trim());
  if (!match) throw new Error(`${path}: not a "<pct> <count> <of>" report: ${JSON.stringify(text)}`);
  const count = Number(match[3]);
  const of = Number(match[4]);
  const tenths = of === 0 ? 0 : Math.floor((count * 1000) / of);
  if (count > of || tenths !== Number(match[1]) * 10 + Number(match[2])) {
    throw new Error(`${path}: percentage does not match ${count}/${of}`);
  }
  return { count, of };
}

/** Exact Kotlin version order: numeric release, then a prerelease before its release. */
export function compareKotlinVersions(a, b) {
  const [ra, pa] = a.split(/-(.*)/s);
  const [rb, pb] = b.split(/-(.*)/s);
  const xa = ra.split(".").map(Number);
  const xb = rb.split(".").map(Number);
  for (let i = 0; i < 3; i++) if (xa[i] !== xb[i]) return xa[i] - xb[i];
  if (pa === pb) return 0;
  if (pa === undefined) return 1;
  if (pb === undefined) return -1;
  return pa < pb ? -1 : 1;
}

function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    throw new Error(`${path}: ${error.message}`);
  }
}

function box(dir) {
  const reports = join(dir, "box");
  if (!existsSync(join(dir, "ci-run.json")) || !existsSync(reports)) return null;
  const run = readJson(join(dir, "ci-run.json"));
  for (const key of ["headSha", "createdAt", "url"]) {
    if (typeof run[key] !== "string") throw new Error(`ci-run.json: missing ${key}`);
  }
  const versions = new Map();
  for (const artifact of readdirSync(reports)) {
    const [kind, spec] = Object.entries(BOX_REPORTS).find(([, s]) => artifact.startsWith(s.prefix)) ?? [];
    const version = spec && artifact.slice(spec.prefix.length);
    if (!spec || !KOTLIN_VERSION.test(version)) throw new Error(`box/${artifact}: not a box report artifact`);
    const path = join(reports, artifact, spec.file);
    const { count, of } = parseBoxReport(readFileSync(path, "utf8"), path);
    if (!versions.has(version)) versions.set(version, { version, jvm: null, bytes: null, native: null });
    versions.get(version)[kind] = { [spec.count]: count, [spec.of]: of };
  }
  if (versions.size === 0) return null;
  return {
    run: { commit: run.headSha, date: run.createdAt, url: run.url },
    versions: [...versions.values()].sort((a, b) => compareKotlinVersions(b.version, a.version)),
  };
}

function corpusProject(p) {
  const cost = (side) => {
    const c = p.cost?.[side];
    if (!c) return null;
    return {
      builtCompileSeconds: c.built_compile_seconds,
      compileSeconds: c.compile_seconds,
      wallSeconds: c.wall_seconds,
      cpuSeconds: c.cpu_seconds,
      // The compiler's own peak: the Kotlin daemon in the reference build, krusty in the other.
      compilerPeakRssMb: side === "kotlinc" ? c.peak_rss_mb.kotlin_daemon : c.peak_rss_mb.krusty,
      buildPeakRssMb: c.peak_rss_mb.total,
    };
  };
  for (const key of ["compilations", "built", "classes", "identical"]) {
    if (!Number.isInteger(p[key])) throw new Error(`corpus/results.json: ${p.name}: missing ${key}`);
  }
  return {
    name: p.name,
    stars: p.stars ?? null,
    kotlinLoc: p.loc?.kotlin ?? null,
    referenceBuild: p.reference_exit === 0 ? "ok" : `failed (exit ${p.reference_exit})`,
    compilations: p.compilations,
    built: p.built,
    classes: p.classes,
    identical: p.identical,
    measured: p.measured ?? null,
    // What the run built: upstream repository and commit, the krusty-side patch (repository path at
    // the measured commit) and the Kotlin version pin. Absent for results published before the
    // corpus recorded it.
    source: p.source ?? null,
    cost: { kotlinc: cost("kotlinc"), krusty: cost("krusty") },
  };
}

/** One line per corpus run: the sums over the projects the run measured, oldest first. */
export function corpusHistory(tsv) {
  const [header, ...lines] = tsv.split("\n").filter((line) => line.trim() !== "");
  const columns = header.split("\t");
  const at = (name) => {
    const index = columns.indexOf(name);
    if (index === -1) throw new Error(`corpus/history.tsv: no "${name}" column`);
    return index;
  };
  const [date, commit, run, built, compilations, identical, classes] = [
    "date", "krusty commit", "run", "built", "compilations", "identical", "classes",
  ].map(at);
  const runs = new Map();
  for (const line of lines) {
    const cells = line.split("\t");
    const key = cells[run];
    if (!runs.has(key)) {
      runs.set(key, { date: cells[date], commit: cells[commit], run: key, projects: 0, built: 0, compilations: 0, identical: 0, classes: 0 });
    }
    const entry = runs.get(key);
    entry.projects += 1;
    for (const [name, index] of [["built", built], ["compilations", compilations], ["identical", identical], ["classes", classes]]) {
      const value = Number(cells[index]);
      if (!Number.isInteger(value)) throw new Error(`corpus/history.tsv: bad ${name} in: ${line}`);
      entry[name] += value;
    }
  }
  return [...runs.values()];
}

function corpus(dir) {
  const results = join(dir, "corpus", "results.json");
  if (!existsSync(results)) return null;
  const projects = readJson(results).projects.map(corpusProject);
  const sum = (key) => projects.reduce((total, p) => total + p[key], 0);
  const history = join(dir, "corpus", "history.tsv");
  return {
    projects,
    total: { compilations: sum("compilations"), built: sum("built"), classes: sum("classes"), identical: sum("identical") },
    history: existsSync(history) ? corpusHistory(readFileSync(history, "utf8")) : [],
  };
}

function coverage(dir) {
  const path = join(dir, "coverage-baseline.json");
  if (!existsSync(path)) return null;
  const baseline = readJson(path);
  const out = {};
  for (const kind of ["lines", "functions", "regions", "branches"]) {
    const { covered, count } = baseline[kind] ?? {};
    if (!Number.isInteger(covered) || !Number.isInteger(count)) throw new Error(`${path}: missing ${kind}`);
    out[kind] = { covered, count };
  }
  return out;
}

/** The page's data from INPUT_DIRECTORY; a missing directory yields only unmeasured sections. */
export function collectMetrics(dir) {
  const present = dir !== undefined && dir !== "" && existsSync(dir);
  return {
    box: present ? box(dir) : null,
    corpus: present ? corpus(dir) : null,
    coverage: present ? coverage(dir) : null,
  };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  if (process.argv.length !== 3) {
    console.error("usage: node scripts/metrics.mjs INPUT_DIRECTORY");
    process.exit(2);
  }
  console.log(JSON.stringify(collectMetrics(process.argv[2]), null, 1));
}
