//! A value class read out of a generic slot and then discarded is popped as the slot holds it.
//!
//! kotlinc's codegen discards an expression statement's value without coercing it
//! (`PromisedValue.discard` pops the callee's own result type): `id(ic)` as a statement is
//! `invokestatic id; pop`, with neither the `checkcast` to the box nor `unbox-impl`. krusty unboxed
//! the discarded result first. A Long carrier is the same single pop of the generic slot, not
//! `pop2` of the unboxed long. A value the statement goes on to use is still unboxed.

use super::common;

const SOURCE: &str = include_str!("box_regressions/valueClassDiscardedGenericResult.kt");

/// Each discarded generic result is popped boxed, including a Long carrier; the kept one is unboxed.
#[test]
fn a_discarded_value_class_result_is_popped_as_the_slot_holds_it() {
    // The portable program spells `@JvmInline` as the corpus placeholder, which only the JVM
    // target expands to the annotation.
    let source =
        krusty::conformance::prepare_test_source(SOURCE, krusty::conformance::TestTarget::Jvm);
    common::assert_class_matches_kotlinc(
        "ValueClassDiscardedResult",
        &source,
        "ValueClassDiscardedResultKt",
    );
    common::expect_box_same_as_kotlinc(&source, "ValueClassDiscardedResultRun");
}

const SUSPEND_CARRIER_READ: &str =
    include_str!("box_regressions/valueClassDiscardedSuspendCarrierRead.kt");

/// A discarded carrier read of a suspend call keeps the cast to the carrier before the pop.
#[test]
fn a_discarded_carrier_read_of_a_suspend_call_keeps_its_cast() {
    let source = krusty::conformance::prepare_test_source(
        SUSPEND_CARRIER_READ,
        krusty::conformance::TestTarget::Jvm,
    );
    common::assert_class_matches_kotlinc(
        "ValueClassDiscardedSuspendCarrierRead",
        &source,
        "ValueClassDiscardedSuspendCarrierReadKt$box$1",
    );
    common::expect_box_same_as_kotlinc(&source, "ValueClassDiscardedSuspendCarrierReadRun");
}
