//! External corpus and reference-toolchain conformance suites. Kept separate from product e2e tests
//! so fast/coverage runs can skip this binary and the full harness can schedule it deliberately.
//! This avoids result filtering after expensive tests have already run.

// Heap profiling (`--features dhat-heap`): swap in dhat's global allocator so the profiler in the box
// conformance harness records every allocation. Zero cost without the feature (dhat isn't compiled).
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static DHAT_ALLOC: krusty::dhat::Alloc = krusty::dhat::Alloc;

mod common;

#[path = "box_corpus_regression_e2e.rs"]
mod box_corpus_regression_e2e;
#[path = "box_vendored_e2e.rs"]
mod box_vendored_e2e;
#[path = "ir_blockers.rs"]
mod ir_blockers;
#[path = "kotlin_box_ir_jvm_conformance.rs"]
mod kotlin_box_ir_jvm_conformance;
#[path = "ksp_real_e2e.rs"]
mod ksp_real_e2e;
#[path = "serialization_conformance.rs"]
mod serialization_conformance;
