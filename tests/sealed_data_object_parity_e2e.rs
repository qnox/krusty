//! A sealed hierarchy's metadata order, and what a `data object` synthesizes.
//!
//! Three facts, all measured against kotlinc 2.4.10 on the same fixture:
//!
//!   * `Class.nestedClassName` is in DECLARATION order. The IR arena is ordered by stable
//!     declaration id, which is not source order for every shape — a `data object` declared above a
//!     `data class` lands after it — so the metadata list has to be ordered explicitly.
//!   * a `data object`'s `hashCode()` is the hash of its SOURCE QUALIFIED NAME as a constant, not
//!     `0`. Returning `0` made every data object in a program hash alike.
//!   * its `equals` DISCARDS the narrowed value: there is no property to compare, so kotlinc emits
//!     the `checkcast` (the type test's own result) followed by `pop`, not a store into a local
//!     nothing loads.

use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

fn fixture() -> &'static str {
    "sealed interface Origin { data object Owned : Origin; data class ImportedRef(val id: String) : Origin }\n"
}

/// `Owned` is declared FIRST but sorts second, and both declarations start on the same line. The
/// exact result therefore distinguishes stable declaration order from name, arena, and line order.
#[test]
fn a_sealed_classifiers_nested_names_are_in_declaration_order() {
    let Some(result) = common::byte_diff_against_kotlinc_cp_target(
        "SealedNestedOrder",
        fixture(),
        "Origin",
        &[common::stdlib_jar()],
        Some("25"),
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("Origin byte-identical to kotlinc");
}

/// The constant is `"Origin.Owned".hashCode()` — the source qualified name, dots for nesting —
/// and the narrowed value in `equals` is discarded rather than stored.
#[test]
fn a_data_object_hashes_its_qualified_name_and_discards_the_narrowed_value() {
    let Some(built) = compare_with_kotlinc_plugin(
        "DataObjectShape",
        fixture(),
        "Origin$Owned",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };

    let reference_hash = method_instructions(&built.reference, "hashCode()");
    assert_ne!(
        reference_hash,
        Vec::<String>::new(),
        "kotlinc emits data-object hashCode instructions"
    );
    assert_eq!(
        method_instructions(&built.krusty, "hashCode()"),
        reference_hash,
        "data-object hashCode instruction sequence"
    );

    let reference_equals = method_instructions(&built.reference, "equals(java.lang.Object)");
    assert_ne!(
        reference_equals,
        Vec::<String>::new(),
        "kotlinc emits data-object equals instructions"
    );
    assert_eq!(
        method_instructions(&built.krusty, "equals(java.lang.Object)"),
        reference_equals,
        "data-object equals instruction sequence"
    );
}
