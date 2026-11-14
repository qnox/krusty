//! A value class over an array renders the array's contents, as kotlinc's generated `toString`
//! does: `Bits(raw=[1, 2])` through `Arrays.toString`, not the array's identity (`[I@1b6d3586`).
//!
//! The overload follows the property's declared type: a primitive array takes its own
//! (`([I)`, `([J)`), and every reference array, nullable or not and whatever its element, takes
//! `([Ljava/lang/Object;)`. A nullable array takes it too, and `Arrays.toString` renders `null`.
//! An unsigned array is itself a value class, so it renders through `UIntArray.toString-impl`
//! (`UBits(raw=UIntArray(storage=[1, -294967296]))`), not through `Arrays.toString` of its
//! carrier. `hashCode` already used `Arrays.hashCode` and `equals` compares the arrays by
//! reference, both as kotlinc does; the byte comparison of each whole class covers them too.
//! `Uns` (`Array<UInt>`) runs but is not byte-compared: its local-variable tables differ from
//! kotlinc's, which is unrelated to rendering.

use super::common;

const SRC: &str = include_str!("box_regressions/valueClassArrayToString.kt");

#[test]
fn a_value_class_over_an_array_renders_its_contents_like_kotlinc() {
    // PLATFORM-SPECIFIC: compare the entire JVM classfiles, including array descriptors and locals.
    let source =
        krusty::conformance::prepare_test_source(SRC, krusty::conformance::TestTarget::Jvm);
    let (declarations, _) = source
        .split_once("\nfun box")
        .expect("whole fixture has box");
    common::assert_classes_identical_to_kotlinc(
        "ArrayValueClasses",
        declarations,
        &["Bits", "Longs", "NBits", "Names", "NNames", "UBits"],
    );
}

#[test]
fn a_value_class_over_an_array_prints_like_kotlinc() {
    let source =
        krusty::conformance::prepare_test_source(SRC, krusty::conformance::TestTarget::Jvm);
    common::expect_box_same_as_kotlinc(&source, "value_class_array_to_string");
}
