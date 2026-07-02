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

- `kotlin_box_ir_jvm_conformance` — Kotlin box/codegen corpus, differential vs kotlinc
- `box_corpus_regression_e2e` — pinned subset of the same corpus
- `ir_blockers` — survey over the external box corpus
- `box_vendored_e2e` — external corpus box files run on the JVM
- `serialization_conformance` — kotlinx.serialization corpus + real kotlinc driver
- `ksp_real_e2e` — external KSP processor corpus + real toolchain

Everything else counts, including project-authored e2e suites (`feature_box_e2e`, the `metadata_*`
e2e, …) whose *inputs live in-repo* even though they exercise the JVM runtime. Binaries are kept by
default, so a **new** test file counts automatically — adding a new external suite must be paired
with an explicit entry in `EXCLUDE`, visible in review. Coverage is reported for the product library
(`src/`); the test harness (`tests/`) and CLI/survey tooling (`src/main.rs`, `src/bin/`) are ignored.

## The regression gate

`coverage-baseline.json` is the committed master coverage. `just coverage-gate` re-measures and
fails if any metric drops more than `KRUSTY_COVERAGE_EPS` (default `0.10`) percentage points below
baseline. `EPS` absorbs LLVM's small run-to-run branch jitter; it is not licence to lose coverage.

Enforced in two places:

- **pre-push git hook** (`lefthook.yml`) — blocks a push that lowers coverage, alongside `lint` and
  `test`. Runs last because it is the heaviest step.
- **CI** (`just ci` → `… coverage-gate`) — the unbypassable backstop. A local `git push --no-verify`
  skips the hook, but CI re-measures and fails the PR, so coverage cannot be lowered by bypassing
  the client-side hook.

To land an intentional coverage change, run `just coverage-bless` and commit the refreshed
`coverage-baseline.json` with the change.

## Performance impact

The coverage run is instrumented (larger, slower codegen; counters add runtime overhead), but it is
NOT JVM startup — the shared JVM daemon is amortized exactly as in `run-tests.sh` — and the parallel
`show-env` runner (above) avoids adding serialization on top. Measured cold, on a full build:

| run                         | wall  | CPU   |
|-----------------------------|-------|-------|
| `just test` (normal gate)   | ~5:06 | ~684s |
| `just coverage` (own suite) | ~5:26 | ~676s |

The coverage run lands close to the normal gate: the instrumentation overhead is offset by excluding
the six heavy external-corpus suites (`kotlin_box_ir_jvm_conformance` alone is a large fraction of
the gate). Warm/incremental re-runs are dominated by the test run, not the rebuild.

Consequence for the gate: pre-push runs `just test` AND `coverage-gate`, so it is roughly **2×** the
old pre-push (about +5 min cold, +2–3 min warm). If that is too heavy locally, run `coverage-gate`
in CI only — CI is the unbypassable enforcement point regardless — and treat the pre-push copy as an
optional early-warning (skip a single push with `git push --no-verify`; CI still blocks a real drop).
