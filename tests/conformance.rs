//! External corpus and reference-toolchain conformance suites. Kept separate from product e2e tests
//! so fast/coverage runs can skip this binary and the full harness can schedule it deliberately.
//! This avoids result filtering after expensive tests have already run.

// Heap profiling (`--features dhat-heap`): swap in dhat's global allocator so the profiler in the box
// conformance harness records every allocation. Zero cost without the feature (dhat isn't compiled).
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static DHAT_ALLOC: krusty::dhat::Alloc = krusty::dhat::Alloc;

mod common;

#[path = "common/asm_text.rs"]
mod asm_text;

#[path = "box_byte_score.rs"]
mod box_byte_score;
mod box_inventory;
#[path = "box_lane.rs"]
mod box_lane;
#[path = "box_ratchet.rs"]
mod box_ratchet;
#[path = "box_reference_classes.rs"]
mod box_reference_classes;
#[path = "box_vendored_e2e.rs"]
mod box_vendored_e2e;
mod dependency_template_rejections;
#[path = "kotlin_box_ir_jvm_conformance.rs"]
mod kotlin_box_ir_jvm_conformance;
#[path = "kotlin_box_native_conformance.rs"]
mod kotlin_box_native_conformance;
#[path = "kotlin_box_wasm_conformance.rs"]
mod kotlin_box_wasm_conformance;
#[path = "kotlin_bytecode_text_conformance.rs"]
mod kotlin_bytecode_text_conformance;
#[path = "kotlin_cli_jvm_conformance.rs"]
mod kotlin_cli_jvm_conformance;
#[path = "common/kotlin_native_multimodule.rs"]
mod kotlin_native_multimodule_support;
#[path = "common/reference_recording.rs"]
mod reference_recording_support;
#[path = "serialization_conformance.rs"]
mod serialization_conformance;
