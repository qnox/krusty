// The metrics page's data: which inputs it reads, how it validates them, and what the built page
// shows. The page tests run after `npm run build`; with no KRUSTY_METRICS_INPUT the page must say
// that nothing was measured rather than show a number.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { collectMetrics, compareKotlinVersions, corpusHistory, parseBoxReport } from "../scripts/metrics.mjs";

function inputs(files) {
  const dir = mkdtempSync(join(tmpdir(), "metrics-"));
  for (const [path, contents] of Object.entries(files)) {
    mkdirSync(dirname(join(dir, path)), { recursive: true });
    writeFileSync(join(dir, path), typeof contents === "string" ? contents : JSON.stringify(contents));
  }
  return dir;
}

const RUN = { databaseId: 7, headSha: "a".repeat(40), createdAt: "2026-11-01T09:00:00Z", url: "https://example.test/run/7" };

function project(name, extra = {}) {
  return {
    name,
    reference_exit: 0,
    loc: { kotlin: 100, java: 0 },
    compilations: 4,
    built: 2,
    classes: 10,
    identical: 3,
    cost: {
      kotlinc: { wall_seconds: 10, cpu_seconds: 20, compile_seconds: 8, built_compile_seconds: 4, peak_rss_mb: { total: 900, kotlin_daemon: 500, krusty: 0 } },
      krusty: { wall_seconds: 7, cpu_seconds: 9, compile_seconds: 3, built_compile_seconds: 1, peak_rss_mb: { total: 600, kotlin_daemon: 0, krusty: 80 } },
    },
    measured: { date: "2026-11-01", commit: "b".repeat(40), run: "https://example.test/run/8" },
    rows: [
      { built: true, fallback: null, error: null },
      { built: false, fallback: "krusty does not implement everything it requests (plugin)", error: null },
      { built: false, fallback: null, error: "not run: a task it depends on failed" },
      { built: false, fallback: null, error: "A.kt:1:1: error: unresolved reference 'x'." },
    ],
    ...extra,
  };
}

test("a box report must agree with its counts", () => {
  assert.deepEqual(parseBoxReport("41.2 412 1000\n", "r"), { count: 412, of: 1000 });
  assert.deepEqual(parseBoxReport("0.0 0 0", "r"), { count: 0, of: 0 });
  assert.throws(() => parseBoxReport("41.3 412 1000", "r"), /r: percentage does not match 412\/1000/);
  assert.throws(() => parseBoxReport("100.0 2 1", "r"), /r: percentage does not match 2\/1/);
  assert.throws(() => parseBoxReport("41.2 0412 1000", "r"), /r: not a "<pct> <count> <of>" report/);
});

test("Kotlin versions order numerically, a prerelease before its release", () => {
  const sorted = ["2.4.10", "2.4.2", "2.4.20-RC", "2.4.20", "2.4.0"].sort(compareKotlinVersions);
  assert.deepEqual(sorted, ["2.4.0", "2.4.2", "2.4.10", "2.4.20-RC", "2.4.20"]);
});

test("no input directory yields only unmeasured sections", () => {
  assert.deepEqual(collectMetrics(undefined), { box: null, corpus: null, coverage: null });
  assert.deepEqual(collectMetrics(join(tmpdir(), "metrics-does-not-exist")), { box: null, corpus: null, coverage: null });
});

test("box reports group by Kotlin version, newest first, and keep a missing kind as null", () => {
  const dir = inputs({
    "ci-run.json": RUN,
    "box/pct-2.4.10/pct.txt": "50.0 1 2\n",
    "box/jvm-byte-equality-2.4.10/jvm-byte-equality.txt": "25.0 1 4\n",
    "box/native-pct-2.4.10/native-pct.txt": "100.0 3 3\n",
    "box/pct-2.4.20/pct.txt": "66.6 2 3\n",
  });
  try {
    assert.deepEqual(collectMetrics(dir).box, {
      run: { commit: RUN.headSha, date: RUN.createdAt, url: RUN.url },
      versions: [
        { version: "2.4.20", jvm: { passed: 2, applicable: 3 }, bytes: null, native: null },
        { version: "2.4.10", jvm: { passed: 1, applicable: 2 }, bytes: { matched: 1, total: 4 }, native: { passed: 3, applicable: 3 } },
      ],
    });
  } finally {
    rmSync(dir, { recursive: true });
  }
});

test("an unknown box artifact stops the build", () => {
  const dir = inputs({ "ci-run.json": RUN, "box/wasm-pct-2.4.20/pct.txt": "0.0 0 1\n" });
  try {
    assert.throws(() => collectMetrics(dir), /box\/wasm-pct-2.4.20: not a box report artifact/);
  } finally {
    rmSync(dir, { recursive: true });
  }
});

test("corpus projects carry their totals, cost and what was built", () => {
  const source = { url: "https://example.test/p", commit: "c".repeat(40), patch: "scripts/oss-corpus/patches/p.patch", kotlin_pin: "gradle.properties:kotlin" };
  const dir = inputs({
    "corpus/results.json": { projects: [project("p", { source }), project("q", { reference_exit: 1, cost: {}, rows: undefined })] },
  });
  try {
    const { corpus } = collectMetrics(dir);
    assert.deepEqual(corpus.total, { compilations: 8, built: 4, classes: 20, identical: 6 });
    assert.deepEqual(corpus.history, []);
    const [p, q] = corpus.projects;
    assert.deepEqual(p, {
      name: "p",
      stars: null,
      kotlinLoc: 100,
      referenceBuild: "ok",
      compilations: 4,
      built: 2,
      classes: 10,
      identical: 3,
      measured: project("p").measured,
      source,
      notBuilt: { leftToKotlinc: 1, failed: 1, notRun: 1 },
      cost: {
        kotlinc: { exit: null, builtCompileSeconds: 4, compileSeconds: 8, wallSeconds: 10, cpuSeconds: 20, compilerPeakRssMb: 500, buildPeakRssMb: 900 },
        krusty: { exit: null, builtCompileSeconds: 1, compileSeconds: 3, wallSeconds: 7, cpuSeconds: 9, compilerPeakRssMb: 80, buildPeakRssMb: 600 },
      },
      // One compilation failed and one was not run: the whole builds did different work.
      wholeBuildsComparable: false,
    });
    assert.equal(q.referenceBuild, "failed (exit 1)");
    assert.equal(q.source, null);
    assert.equal(q.notBuilt, null);
    assert.deepEqual(q.cost, { kotlinc: null, krusty: null });
  } finally {
    rmSync(dir, { recursive: true });
  }
});

test("corpus history sums each run's projects, across header versions", () => {
  const header = "date\tkrusty commit\tproject\treference exit\tbuilt\tcompilations\tidentical\tclasses\trun\tkotlinc wall s\n";
  const tsv =
    header +
    "2026-10-30\tc1\tp\t0\t1\t4\t2\t10\thttps://r/1\n" +
    "2026-10-30\tc1\tq\t0\t0\t3\t0\t5\thttps://r/1\n" +
    "2026-10-31\tc2\tp\t0\t2\t4\t3\t10\thttps://r/2\t12.5\n";
  assert.deepEqual(corpusHistory(tsv), [
    { date: "2026-10-30", commit: "c1", run: "https://r/1", projects: 2, built: 1, compilations: 7, identical: 2, classes: 15 },
    { date: "2026-10-31", commit: "c2", run: "https://r/2", projects: 1, built: 2, compilations: 4, identical: 3, classes: 10 },
  ]);
  assert.throws(() => corpusHistory("date\tproject\n"), /corpus\/history.tsv: no "krusty commit" column/);
});

test("the coverage baseline must name every measure", () => {
  const dir = inputs({ "coverage-baseline.json": { lines: { covered: 1, count: 2 } } });
  try {
    assert.throws(() => collectMetrics(dir), /coverage-baseline.json: missing functions/);
  } finally {
    rmSync(dir, { recursive: true });
  }
});

const built = new URL("../dist/metrics/index.html", import.meta.url);

test("the built page shows every measured project, or says nothing was measured", { skip: !existsSync(built) }, () => {
  // Text only: scoped styles put attributes on every element.
  const html = readFileSync(built, "utf8").replace(/<[^>]+>/g, "|");
  const data = JSON.parse(readFileSync(new URL("../dist/metrics.json", import.meta.url), "utf8"));
  if (data.corpus) {
    for (const p of data.corpus.projects) assert.ok(html.includes(`|${p.name}|`), `page is missing project ${p.name}`);
  } else {
    assert.ok(html.includes("no nightly corpus result was available"));
  }
  if (data.box) {
    for (const v of data.box.versions) assert.ok(html.includes(`|${v.version}|`), `page is missing Kotlin ${v.version}`);
  } else {
    assert.ok(html.includes("no successful master CI run with box reports"));
  }
});

test("the landing page links the metrics page", { skip: !existsSync(new URL("../dist/index.html", import.meta.url)) }, () => {
  assert.ok(readFileSync(new URL("../dist/index.html", import.meta.url), "utf8").includes('href="/metrics/"'));
});

// From the corpus producer to the page's data: compare.py and summarize.py write results.json the
// way the nightly run does, and the page credits a whole-build change only where both Gradle builds
// succeeded and krusty did the same work.
test("whole builds are comparable only when both succeeded and did the same work", () => {
  const corpus = new URL("../../scripts/oss-corpus/", import.meta.url).pathname;
  const metricsJson = (exit, wall, rss) =>
    JSON.stringify({ exit, wall_seconds: wall, cpu_seconds: wall, peak_rss_mb: { total: rss, gradle: 500, kotlin_daemon: 0, krusty: 0, other: 0 } });
  const work = inputs({});
  const files = {};
  const projects = {
    // Every compilation built, but krusty's Gradle build failed after them.
    failed: { krustyExit: 1, krustyLog: "" },
    succeeded: { krustyExit: 0, krustyLog: "" },
    // :b is compiled by kotlinc in both builds: the same work.
    fallback: { krustyExit: 0, krustyLog: "krusty: task ':b:compileKotlin' is left to kotlinc: unsupported plugin\n" },
  };
  for (const [name, { krustyExit, krustyLog }] of Object.entries(projects)) {
    for (const mode of ["kotlinc", "krusty"]) {
      files[`out/${name}/${mode}/a/build/classes/kotlin/main/A.class`] = "A";
      files[`out/${name}/${mode}/b/build/classes/kotlin/main/B.class`] = "B";
    }
    files[`logs/${name}.kotlinc.metrics.json`] = metricsJson(0, 60, 900);
    files[`logs/${name}.krusty.metrics.json`] = metricsJson(krustyExit, 30, 600);
    files[`logs/${name}.kotlinc.compilations.tsv`] = ":a:compileKotlin\ta/build/classes/kotlin/main\n:b:compileKotlin\tb/build/classes/kotlin/main\n";
    files[`logs/${name}.kotlinc.tasks.tsv`] = ":a:compileKotlin\t1000\t4000\texecuted\n:b:compileKotlin\t4000\t6500\texecuted\n";
    files[`logs/${name}.krusty.tasks.tsv`] = ":a:compileKotlinWithKrusty\t1100\t1900\texecuted\n:b:compileKotlin\t2100\t4600\texecuted\n";
    files[`logs/${name}.krusty.log`] = krustyLog;
  }
  for (const [path, contents] of Object.entries(files)) {
    mkdirSync(dirname(join(work, path)), { recursive: true });
    writeFileSync(join(work, path), contents);
  }
  try {
    const compares = Object.keys(projects).map((name) => {
      execFileSync("python3", [join(corpus, "compare.py"), work, name, "0"], { stdio: "ignore" });
      return join(work, "out", name, "compare.json");
    });
    mkdirSync(join(work, "input", "corpus"), { recursive: true });
    execFileSync("python3", [join(corpus, "summarize.py"), "--json", join(work, "input", "corpus", "results.json"), ...compares], { stdio: "ignore" });
    const byName = Object.fromEntries(collectMetrics(join(work, "input")).corpus.projects.map((p) => [p.name, p]));
    assert.deepEqual(
      Object.fromEntries(Object.entries(byName).map(([name, p]) => [name, [p.built, p.notBuilt, p.cost.kotlinc.exit, p.cost.krusty.exit, p.wholeBuildsComparable]])),
      {
        failed: [2, { leftToKotlinc: 0, failed: 0, notRun: 0 }, 0, 1, false],
        succeeded: [2, { leftToKotlinc: 0, failed: 0, notRun: 0 }, 0, 0, true],
        fallback: [1, { leftToKotlinc: 1, failed: 0, notRun: 0 }, 0, 0, true],
      },
    );
    // The raw measurements stay whatever the comparability.
    assert.deepEqual(byName.failed.cost.krusty, { exit: 1, builtCompileSeconds: 3.3, compileSeconds: 3.3, wallSeconds: 30, cpuSeconds: 30, compilerPeakRssMb: 0, buildPeakRssMb: 600 });
  } finally {
    rmSync(work, { recursive: true });
  }
});

test("a result without build outcomes leaves whole-build comparability unknown", () => {
  const dir = inputs({ "corpus/results.json": { projects: [project("old", { rows: [{ built: true, fallback: null, error: null }] })] } });
  try {
    assert.equal(collectMetrics(dir).corpus.projects[0].wholeBuildsComparable, null);
  } finally {
    rmSync(dir, { recursive: true });
  }
});
