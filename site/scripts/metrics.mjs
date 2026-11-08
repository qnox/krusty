// Reads the inputs of the public metrics page (/metrics/) from one directory and turns them into the
// page's data. The `pages` workflow fills the directory each night (see .github/workflows/pages.yml):
//
//   ci-run.json                                   the master `ci` run the box reports come from:
//                                                 {"databaseId", "headSha", "createdAt", "url"}
//   box/pct-<kotlin>/pct.txt                      JVM case report      "<pct> <passed> <applicable>"
//   box/jvm-byte-equality-<kotlin>/jvm-byte-equality.txt
//                                                 JVM byte report      "<pct> <matched> <total>"
//   box/native-pct-<kotlin>/native-pct.txt        Native case report   "<pct> <passed> <applicable>"
//   corpus/runs/<run id>/run.json                 one nightly `oss corpus` run on master:
//                                                 {"databaseId", "headSha", "createdAt", "url", "conclusion"}
//   corpus/runs/<run id>/summary.json             that run's `summary` artifact (summarize.py --json);
//                                                 a run without it (an older artifact) is left out
//   corpus/runs/<run id>/projects.tsv             scripts/oss-corpus/projects.tsv at the run's commit
//   corpus/runs/<run id>/patches.txt              the names in scripts/oss-corpus/patches/ at that commit
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

/**
 * Why the compilations krusty did not build were not built, from the per-compilation rows:
 * left to kotlinc (the Gradle plugin found a plugin or argument krusty does not implement and let
 * kotlinc compile it), failed (krusty reported an error), or not run (a task it depends on failed).
 * Null for a result without rows.
 */
function notBuilt(p) {
  if (!Array.isArray(p.rows)) return null;
  const out = { leftToKotlinc: 0, failed: 0, notRun: 0 };
  for (const row of p.rows) {
    if (row.built) continue;
    if (row.fallback) out.leftToKotlinc += 1;
    else if (typeof row.error === "string" && row.error.startsWith("not run:")) out.notRun += 1;
    else out.failed += 1;
  }
  return out;
}

const PROJECTS_TSV = "scripts/oss-corpus/projects.tsv";
const PATCHES = "scripts/oss-corpus/patches";

/**
 * What the corpus builds, from projects.tsv at the measured commit: per project, the upstream
 * repository and pinned commit, the Kotlin version pin, and the krusty-side patch applied after
 * checkout (a path in this repository at the measured commit), or null without one.
 */
export function corpusSources(tsv, patches, path) {
  const out = new Map();
  for (const line of tsv.split("\n")) {
    if (line.trim() === "" || line.startsWith("#")) continue;
    const [name, url, commit, , , pin] = line.split("\t");
    if (!name || !url || !/^[0-9a-f]{40}$/.test(commit ?? "") || !pin) throw new Error(`${path}: not a project line: ${JSON.stringify(line)}`);
    out.set(name, {
      url: url.replace(/\.git$/, ""),
      commit,
      kotlinPin: pin === "-" ? null : pin,
      patch: patches.has(`${name}.patch`) ? `${PATCHES}/${name}.patch` : null,
    });
  }
  return out;
}

function corpusProject(p, path, measured, source) {
  const cost = (side) => {
    const c = p.cost?.[side];
    if (!c) return null;
    return {
      // The whole build's exit status; null for results recorded before the corpus kept it.
      exit: Number.isInteger(c.exit) ? c.exit : null,
      builtCompileSeconds: c.built_compile_seconds,
      compileSeconds: c.compile_seconds,
      wallSeconds: c.wall_seconds,
      cpuSeconds: c.cpu_seconds,
      // The compiler's own peak: the Kotlin daemon in the reference build, krusty in the other.
      compilerPeakRssMb: side === "kotlinc" ? c.peak_rss_mb.kotlin_daemon : c.peak_rss_mb.krusty,
      buildPeakRssMb: c.peak_rss_mb.total,
    };
  };
  for (const key of ["reference_exit", "compilations", "built", "classes", "identical"]) {
    if (!Number.isInteger(p[key])) throw new Error(`${path}: ${p.name}: missing ${key}`);
  }
  const project = {
    name: p.name,
    stars: p.stars ?? null,
    kotlinLoc: p.loc?.kotlin ?? null,
    // The reference build's own exit status. A failed one is reported as failed, not as a
    // project krusty matched or missed: the classes it left are all the comparison has.
    referenceExit: p.reference_exit,
    referenceBuild: p.reference_exit === 0 ? "ok" : `failed (exit ${p.reference_exit})`,
    compilations: p.compilations,
    built: p.built,
    classes: p.classes,
    identical: p.identical,
    measured,
    // What the run built; null when the run's project list does not name it.
    source,
    notBuilt: notBuilt(p),
    cost: { kotlinc: cost("kotlinc"), krusty: cost("krusty") },
  };
  project.wholeBuildsComparable = wholeBuildsComparable(project, p.reference_exit);
  return project;
}

/**
 * Whether the two whole builds of a project did the same work, so their wall time and memory can be
 * compared: both Gradle builds succeeded, and krusty built at least one compilation and failed or
 * skipped none (a compilation left to kotlinc is compiled in both builds). False when they did not;
 * null when a result recorded before the corpus kept the build exit statuses cannot tell.
 */
export function wholeBuildsComparable(project, referenceExit) {
  const { kotlinc, krusty } = project.cost;
  if (!kotlinc || !krusty || referenceExit !== 0 || project.built === 0) return false;
  if (project.notBuilt === null) return null;
  if (project.notBuilt.failed > 0 || project.notBuilt.notRun > 0) return false;
  if (kotlinc.exit === null || krusty.exit === null) return null;
  return kotlinc.exit === 0 && krusty.exit === 0;
}

const sum = (projects, key) => projects.reduce((total, p) => total + p[key], 0);

/** One corpus run: its identity, and every project record its summary artifact holds. */
function corpusRun(dir, id) {
  const at = (file) => join(dir, id, file);
  const run = readJson(at("run.json"));
  for (const key of ["headSha", "createdAt", "url", "conclusion"]) {
    if (typeof run[key] !== "string") throw new Error(`${at("run.json")}: missing ${key}`);
  }
  if (String(run.databaseId) !== id) throw new Error(`${at("run.json")}: not run ${id}`);
  const measured = { run: run.url, commit: run.headSha, date: run.createdAt, conclusion: run.conclusion };
  const summary = readJson(at("summary.json"));
  if (!Array.isArray(summary.projects)) throw new Error(`${at("summary.json")}: no projects`);
  const listed = existsSync(at("projects.tsv"))
    ? corpusSources(
        readFileSync(at("projects.tsv"), "utf8"),
        new Set(existsSync(at("patches.txt")) ? readFileSync(at("patches.txt"), "utf8").split("\n").filter(Boolean) : []),
        at("projects.tsv"),
      )
    : null;
  const projects = summary.projects.map((p) => corpusProject(p, at("summary.json"), measured, listed?.get(p.name) ?? null));
  return {
    measured,
    projects,
    // Listed projects the run has no result for: their job failed before it compared anything.
    // Null when the run's project list was not read.
    noResult: listed ? [...listed.keys()].filter((name) => !projects.some((p) => p.name === name)) : null,
    listed: listed ? listed.size : null,
  };
}

/** One line per corpus run, in the order given: the sums over the projects the run measured. */
export function corpusHistory(runs) {
  return runs
    .map(({ measured, projects }) => ({
      date: measured.date.slice(0, 10),
      commit: measured.commit,
      run: measured.run,
      projects: projects.length,
      built: sum(projects, "built"),
      compilations: sum(projects, "compilations"),
      identical: sum(projects, "identical"),
      classes: sum(projects, "classes"),
    }));
}

function corpus(dir) {
  const runsDir = join(dir, "corpus", "runs");
  if (!existsSync(runsDir)) return null;
  // A run whose summary artifact predates summary.json (it held only summary.md) has no
  // per-project records: it is left out like a run whose artifact expired.
  const runs = readdirSync(runsDir)
    .filter((id) => existsSync(join(runsDir, id, "summary.json")))
    .map((id) => corpusRun(runsDir, id));
  if (runs.length === 0) return null;
  runs.sort((a, b) => (a.measured.date < b.measured.date ? -1 : a.measured.date > b.measured.date ? 1 : 0));
  const latest = runs[runs.length - 1];
  const { projects } = latest;
  return {
    measured: latest.measured,
    projects,
    noResult: latest.noResult,
    listed: latest.listed,
    total: { compilations: sum(projects, "compilations"), built: sum(projects, "built"), classes: sum(projects, "classes"), identical: sum(projects, "identical") },
    history: corpusHistory(runs),
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
