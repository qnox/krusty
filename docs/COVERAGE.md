# Test coverage

krusty tracks **regions, functions, lines, and branches** with LLVM source-based coverage
(`-C instrument-coverage`). Branch coverage is the headline metric the project cares about most —
it is what distinguishes "the line ran" from "both sides of the `if` ran".

## Running it

```sh
just coverage        # measure: instrumented build + own suite, prints the four totals
just coverage-gate   # measure + fail if any metric regressed below coverage-baseline.json
just coverage-bless  # re-measure and overwrite coverage-baseline.json (intentional changes only)
```

Branch coverage requires the nightly `-Zcoverage-options=branch` path, so the run goes through the
nightly toolchain. `llvm-tools-preview` must be installed on nightly
(`rustup component add llvm-tools-preview --toolchain nightly`) and `cargo-llvm-cov` on `PATH`
(`cargo install cargo-llvm-cov`).

Artifacts land in `target/coverage/`: `full.json` (per-file `llvm-cov export`) and `summary.json`
(the four totals — the shape the gate compares).

## Runner — why not `cargo llvm-cov test` / nextest

The normal gate is fast because `run-tests.sh` runs the test *binaries* in parallel (threads within
each binary, many binaries at once, one shared JVM daemon). The two obvious coverage runners both
throw that away:

- `cargo llvm-cov test` wraps plain `cargo test`, which runs the binaries **serially** — the
  dominant cost.
- `cargo llvm-cov nextest` runs each test case in its **own process**. Every test then contends on
  the shared JVM daemon, and tests that rely on per-binary shared state fail outright. It is
  net-negative here.

So `scripts/coverage.sh` uses the `llvm-cov show-env` workflow instead: it sets the instrumentation
environment, builds once, runs the (non-excluded) binaries in parallel itself — the same model as
`run-tests.sh` — and aggregates the profraw counters with `llvm-cov report`.

## What counts — scope of the metric

The metric measures **krusty's own test suite**. Suites whose *input* is an external corpus or the
reference compiler are **excluded**, because counting them measures kotlinc's coverage of its own
testdata rather than the quality of krusty-authored tests. The excluded set lives in
`scripts/coverage.sh` (`EXCLUDE=(...)`):

- `conformance` — external corpus/reference-toolchain suites: Kotlin box/codegen, pinned box regressions, IR blockers, vendored box cases, serialization conformance, and KSP real-toolchain checks

Everything else counts, including project-authored e2e suites (`feature_box_e2e`, the `metadata_*`
e2e, …) whose *inputs live in-repo* even though they exercise the JVM runtime. The product `e2e` binary is kept by default, so new product tests count automatically. Adding a new external suite should go under the `conformance` binary or be paired with an explicit `EXCLUDE` entry, visible in review. Coverage is reported for the product library
(`src/`); the test harness (`tests/`) and CLI/survey tooling (`src/main.rs`, `src/bin/`) are ignored.

## The regression gate

`coverage-baseline.json` is the committed master coverage. `just coverage-gate` re-measures and
fails if any metric drops more than `KRUSTY_COVERAGE_EPS` (default `0.10`) percentage points below
baseline. `EPS` absorbs LLVM's small run-to-run branch jitter; it is not licence to lose coverage.

Enforced in two places:

- **pre-push git hook** (`lefthook.yml`) — three steps: `lint`, then `coverage-gate` (the own suite
  WITH coverage — one instrumented run that both checks correctness and enforces the baseline, so
  there is no separate plain own-suite run), then `conformance-plain` (the kotlin box corpus WITHOUT
  coverage). Blocks a push that lowers coverage or breaks a test.
- **CI** (`just ci` → `… coverage-gate`) — the unbypassable backstop. A local `git push --no-verify`
  skips the hook, but CI re-measures and fails the PR, so coverage cannot be lowered by bypassing
  the client-side hook.

To land an intentional coverage change, run `just coverage-bless` and commit the refreshed
`coverage-baseline.json` with the change.

## Performance

The pre-push gate was folded from two full suite runs (plain `just test` + `coverage-gate`) into one
instrumented own-suite run plus the plain conformance suite, and the runners were tuned:

- **No double-run.** `coverage-gate`'s instrumented run is also the correctness check for the own
  suite, so the old separate `just test` is gone.
- **`--test-threads=1` per binary** in both `run-tests.sh` and `scripts/coverage.sh`. With ~190
  binaries the `-P jobs` across-binary parallelism already fills the cores; letting each binary
  default to `nproc` threads made it `jobs×nproc`-wide and thrashed (measured: 4×1 beats 4×2).
- **Concurrent JVM box-runner** (`tests/common`): requests carry an id and a background thread demuxes
  tagged responses, so a multi-threaded binary overlaps its JVM round-trips instead of serialising on
  one lock. (Neutral at the 4×1 default; it unblocks threading a small tail of binaries.)

On a 4-core box the gate is ~3 minutes for this scope (own+coverage + conformance). A flamegraph of
the compile pipeline shows the floor: `check` (call resolution) ≈ 60% and `emit` ≈ 35% of compile
time, spread thin with no single hotspot — it is unoptimised-compiler CPU, and raising `opt-level`
is rejected because the rebuild cost after each change outweighs the faster run. So the gate is
CPU-bound on 4 cores; the full suite across all Kotlin versions still runs in CI.
