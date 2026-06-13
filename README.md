# krust

Memory-lean Kotlin→JVM compiler **PoC**: a linear, data-oriented, **per-file streaming** pipeline.
Follow-up to the `kotlin-memory-bench` finding that kotlinc's whole-module pipeline is what caps
memory optimization; krust is the per-file design built from scratch. **Not** a production compiler.

- Spec: `docs/SPEC.md` · Plan: `docs/IMPLEMENTATION_PLAN.md`
- Build/test: `cargo test` · Run: `cargo run -- file.kt`
- Status: Phase 0 (lexer + diagnostics) ✅
