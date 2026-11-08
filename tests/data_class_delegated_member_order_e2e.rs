//! A data class's generated `equals`/`hashCode`/`toString` and its `interface by` forwarders.
//!
//! kotlinc emits the forwarders after the structural members and lists both in one run of the
//! class's metadata, sorted by its declaration comparator, after the declared functions and the
//! components and `copy`: `answer`, `equals`, `greet`, `hashCode`, `toString`. krusty listed the
//! structural members first and the forwarders after them.

use super::common;

const SOURCE: &str = include_str!("box_regressions/dataClassDelegatedMemberOrder.kt");

#[test]
fn data_class_lists_structural_and_delegated_members_as_kotlinc_does() {
    common::assert_class_matches_kotlinc("DataClassDelegatedMemberOrder", SOURCE, "Named");
    common::expect_box_same_as_kotlinc(SOURCE, "DataClassDelegatedMemberOrderRun");
}
