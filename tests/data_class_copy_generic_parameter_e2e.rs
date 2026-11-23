//! A data class's generated `copy` guards a parameter only when its type excludes `null`, as
//! kotlinc's does. A type parameter whose bound admits `null` (`T` with the implicit `Any?` bound)
//! is not guarded, in a top-level, nested or `out`-variant class alike.

use super::common;

const SOURCE: &str = "data class Plain<T>(val value: T)\n\
                      data class Covariant<out T>(val value: T)\n\
                      data class Mixed<T>(val first: T, val second: String, val third: String?)\n\
                      class Outer {\n\
                      \x20   data class Nested<out T>(val value: T)\n\
                      }\n";

#[test]
fn a_data_class_copy_guards_only_a_parameter_that_excludes_null() {
    for class in ["Plain", "Covariant", "Mixed", "Outer$Nested"] {
        common::byte_diff_against_kotlinc_cp(
            "DataClassCopyGenericParameter",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc:\n{diff}"));
    }
}
