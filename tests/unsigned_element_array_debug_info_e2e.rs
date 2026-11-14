//! An `Array<UInt>` stores `kotlin.UInt` boxes, so its descriptor is `[Lkotlin/UInt;` everywhere,
//! debug information included, as kotlinc writes it.
//!
//! The box class `kotlin/UInt` shares its name with the semantic scalar, so describing the boxed
//! element by that name described the unboxed `int` instead: `[I`. Method descriptors already took
//! the reference-slot path; the local-variable rows did not. A constructor typed its `raw` local
//! `[I`, and every generated value-class member over such an array lost its `LocalVariableTable`,
//! whose method descriptor then named no method of the class.

use super::common;

const SRC: &str = include_str!("box_regressions/unsignedElementArrayDebugInfo.kt");

#[test]
fn an_array_of_unsigned_values_is_described_by_its_boxes_like_kotlinc() {
    // PLATFORM-SPECIFIC: compare the entire JVM classfiles, including local-variable tables.
    let source =
        krusty::conformance::prepare_test_source(SRC, krusty::conformance::TestTarget::Jvm);
    let (declarations, _) = source
        .split_once("\nfun box")
        .expect("whole fixture has box");
    common::assert_classes_identical_to_kotlinc(
        "UnsignedElementArrays",
        declarations,
        &[
            "Uns",
            "Longs",
            "NUns",
            "Shorts",
            "Holder",
            "UnsignedElementArraysKt",
        ],
    );
}

#[test]
fn an_array_of_unsigned_values_runs_like_kotlinc() {
    let source =
        krusty::conformance::prepare_test_source(SRC, krusty::conformance::TestTarget::Jvm);
    common::expect_box_same_as_kotlinc(&source, "unsigned_element_array_debug_info");
}
