# Test quality — perturbation (mutation) testing

Coverage says which lines *ran*; it cannot tell whether a test would have *noticed* a change. A test
that exercises code but asserts nothing still shows as covered. Perturbation testing closes that gap:
it mutates the source (flip a comparison, drop a `!`, replace a return value, delete a statement) and
re-runs the tests. If no test fails, that **mutant survived** — the code it changed is not pinned by
any assertion. A pile of surviving mutants in a region means the tests there do nothing useful.

This is run **occasionally**, not on every commit — each mutant rebuilds and re-runs the suite, so a
full sweep is hours. Scope it.

## Running

```sh
just mutants                     # mutate only the diff vs origin/master (recent work) — tractable
just mutants -f src/resolve.rs   # mutate one file
just mutants -f src/jvm/ir_emit.rs --line-col-in-diff  # etc. — any cargo-mutants flags pass through
```

Needs the tool: `cargo install cargo-mutants`. Config is in `.cargo/mutants.toml` (fast `gate`
profile, generous timeout, tooling/entrypoints excluded). The recipe provisions `KRUSTY_KOTLINC` +
the box corpus like the test harness, so mutants are judged by the real suite.

## Reading the result

cargo-mutants classifies each mutant:

- **caught** — a test failed. Good: the behaviour is pinned.
- **MISSED** — all tests passed with the mutant applied. The interesting output: either the code is
  untested, or a test touches it without asserting the mutated behaviour. Add/strengthen a test.
- **unviable** — the mutant didn't compile (ignored).
- **timeout** — the mutant caused a hang (usually treated as caught).

Investigate the MISSED list. A cluster of misses in one function is a test-quality hole even when the
coverage number for that function is high.

## Notes

- Mutants run the full `cargo test` suite (including the kotlin box conformance suite) per mutant —
  that is deliberate: any test may be the one that kills a compiler-behaviour mutant. It is why this
  is slow and occasional.
- Keep the scope small (`--in-diff`, `-f <file>`). A whole-crate sweep of a compiler is thousands of
  mutants; only do that as a deliberate, long-running audit.
