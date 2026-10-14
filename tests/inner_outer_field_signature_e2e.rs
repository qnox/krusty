//! An inner class's `this$0` field is typed as its outer class's default type, the outer type
//! parameters applied, so a generic outer gives the field a `Signature` as kotlinc's does; the
//! constructor's outer-instance parameter keeps the plain class.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
class Outer<T, R : CharSequence>(val seed: T) {\n\
    inner class Inner(val extra: Int) {\n\
        fun get(): T = seed\n\
    }\n\
\n\
    inner class Generic<S>(val s: S) {\n\
        fun first(): T = seed\n\
    }\n\
}\n\
\n\
class Plain {\n\
    inner class Nested {\n\
        fun one() = 1\n\
    }\n\
}\n\
";

#[test]
fn an_inner_class_outer_field_is_signed_with_the_outer_type_parameters() {
    for class in [
        "store/Outer$Inner",
        "store/Outer$Generic",
        "store/Plain$Nested",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "InnerOuterFieldSignature",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
