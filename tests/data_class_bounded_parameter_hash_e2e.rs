//! A data class property typed by a type parameter with a non-null bound hashes without a null
//! branch, as in kotlinc; an unbounded or `Any?`-bounded one keeps it.

use super::common;

const SOURCE: &str = "interface Marker<J>\n\
                      data class Unbounded<J>(val a: J)\n\
                      data class Nullable<J : Any?>(val a: J)\n\
                      data class AnyBound<J : Any>(val a: J)\n\
                      data class InterfaceBound<J, T : Marker<J>>(val a: J, val b: T)\n\
                      data class SecondBound<T>(val a: T) where T : Any?, T : Marker<*>\n";

#[test]
fn a_data_class_hashes_a_bounded_parameter_property_without_a_null_branch() {
    for class in [
        "Unbounded",
        "Nullable",
        "AnyBound",
        "InterfaceBound",
        "SecondBound",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "DataClassBoundedParameterHash",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc:\n{diff}"));
    }
}
