//! Fields kotlinc leaves out of `@Metadata`.
//!
//! Measured against kotlinc 2.4.0, 2.4.10 and 2.4.20, whose `ProtoBuf` classes agree on both points:
//!
//! * `ValueParameter` has no field 9 in any of them, so nothing records a strict-equality bound on
//!   `equals`' parameter, neither for a declared `equals` nor for the ones a data class or a value
//!   class synthesizes. (A declared `equals` also inherits `Any.equals`' return-value status, which
//!   the return-value-status tests cover.)
//! * `Class.inlineClassUnderlyingType` (f18) is written only when the underlying property is not
//!   public API (`private` or `internal`). A `public` or `protected` property carries its own
//!   return type, so the class records just the property name (f17).

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    let Some(result) =
        common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
    else {
        eprintln!("skip ({stem}: provisioned kotlinc unavailable)");
        return;
    };
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn a_data_class_equals_records_no_equality_bound() {
    const SRC: &str = "package app\n\
        \n\
        data class D(val x: Int, val s: String)\n";
    assert_identical("data_equals", SRC, "app/D");
}

#[test]
fn a_public_value_class_property_records_only_its_name() {
    const SRC: &str = "package app\n\
        \n\
        @JvmInline value class V(val x: Int)\n";
    assert_identical("value_public", SRC, "app/V");
}

#[test]
fn a_protected_value_class_property_records_only_its_name() {
    const SRC: &str = "package app\n\
        \n\
        @JvmInline value class V(protected val x: Long)\n";
    assert_identical("value_protected", SRC, "app/V");
}

#[test]
fn a_private_value_class_property_records_its_type() {
    const SRC: &str = "package app\n\
        \n\
        @JvmInline value class V(private val x: Int)\n";
    assert_identical("value_private", SRC, "app/V");
}
