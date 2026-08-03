# Contributor & assistant guidelines for krusty

## Branding policy — NO AI/tool attribution (hard rule)

This project does **not** carry any AI, assistant, or tool branding. When working on this repo
(human or AI assistant), you MUST NOT add:

- `Co-Authored-By:` trailers naming an AI/assistant/tool (e.g. Claude, Copilot, GPT, etc.)
- "Generated with", "Created by", "🤖", or similar attribution in commit messages, PR bodies,
  code comments, or docs
- Tool/vendor names in author/committer fields

All commits are authored by the project maintainer. Commit messages describe the change only —
what and why — with no tooling provenance. Keep this rule when amending or rewriting history.

## Engineering conventions

- **TDD is required.** Every feature lands with a test; every phase ends on a green harness run.
- **Use the test harness, not plain `cargo test`, for full-suite validation.** Run `./run-tests.sh`
  with no parameters for the normal gate. It self-provisions the reference Kotlin compiler and box
  corpus when `just` is available, uses the fast `gate` Cargo profile, builds once, then runs test
  binaries in parallel while preserving each binary's shared JVM runner. `just test` is equivalent.
  See `docs/TEST_HARNESS.md` for the canonical harness commands and profiling knobs.
- Harness parameters are normally unnecessary. Pass arguments only for a focused Cargo test/filter
  (`./run-tests.sh --test metadata_return_types`); any argument deliberately falls back to Cargo's
  normal runner. Set `KRUSTY_TEST_JOBS=<n>` only when profiling the full-suite binary scheduler. Do
  not use `--release` for tests: the longer build cycle costs more than the faster run saves.
- The harness already has profiling hooks. For compiler-only conformance profiling, run
  `KRUSTY_NO_RUN=1 KRUSTY_FLAMEGRAPH=1 ./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture`;
  it writes `target/flamegraph.svg` and prints phase timing. For full-suite profiling, use the slowest
  test-binaries table printed by `./run-tests.sh`.
- Do not add ad hoc JVM launchers in tests. Use `tests/common::compile_and_run_box`,
  `tests/common::run_box`, or `tests/common::javac_run`; these keep persistent JVM runners/servers and
  avoid per-test `javac`/`java` startup.
- **Diagnostics: use the trace facility, never raw prints.** For any debug output in compiler code use
  `trace_compiler!("<category>", …)` (`src/trace.rs`; categories listed there — `resolve`, `suspend`,
  `value_classes`, `splice`). It is gated by the `trace` cargo feature, **off by default** so the gate
  and release pay zero cost (every site compiles out). To diagnose, build `--features trace` then set
  `KRUSTY_TRACE=all` or a category list (e.g. `KRUSTY_TRACE=resolve`). Do **not** use
  `eprintln!`/`println!`/`dbg!` in the compiler: the differential harness parses stdout/stderr, so stray
  prints can corrupt it, and they tend to get left behind. The custom facility is intentional — do
  **not** add a logging crate (`tracing`/`log`); the project is deliberately dependency-lean.
- The AST/IR stays **index-based** (`u32` ids into parallel `Vec`s — no `Box`/`Rc` graphs).
- Correctness is defined by the **differential harness** vs the real `kotlinc`: don't claim a
  feature works without an ABI-signature diff and/or a round-trip test.
- Record every Kotlin-semantics decision in `docs/SPEC.md` with a test.
- Keep `docs/SPEC.md`, `docs/IMPLEMENTATION_PLAN.md`, and `docs/METADATA_NOTES.md` current.
