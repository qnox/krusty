//! A generic call asserted into a number unboxes the erased reference through `Number`.
//!
//! `fun <T> f() = 1L as T` returns `Object`. `val x: Int = f()!!` null-checks that object and
//! unboxes with `Number.intValue`: the `Long` is a `Number`, so the read yields `1`. Checking the
//! object as `Integer` first throws `ClassCastException`. A value stored as `Int?`, an explicit
//! `as Int?`, and a `Boolean` or `Char` assertion keep the wrapper `checkcast`.

use super::common;

#[test]
fn a_generic_long_asserted_as_int_is_one() {
    let source = r#"
        @Suppress("UNCHECKED_CAST")
        fun <T> f() = 1L as T
        fun box(): String {
            val x: Int = f()!!
            return if (x == 1) "OK" else "fail: $x"
        }
    "#;
    common::expect_box_same_as_kotlinc(source, "GenericCastNumber");
}

#[test]
fn asserted_generic_numbers_match_kotlinc_and_stored_wrappers_stay() {
    let source = r#"
        @Suppress("UNCHECKED_CAST")
        fun <T> f() = 1L as T
        @Suppress("UNCHECKED_CAST")
        fun <T> flag() = true as T
        @Suppress("UNCHECKED_CAST")
        fun <T> letter() = 'a' as T

        fun bare(): Int {
            val x: Int = f()!!
            return x
        }
        fun wide(): Long {
            val x: Long = f()!!
            return x
        }
        fun narrow(): Byte {
            val x: Byte = f()!!
            return x
        }
        fun boxed(): Int? = f()
        fun plain(): Int {
            val x: Int? = f()
            return x!!
        }
        fun explicit(): Int {
            val x: Int = (f() as Int?)!!
            return x
        }
        fun asBool(): Boolean {
            val x: Boolean = flag()!!
            return x
        }
        fun asChar(): Char {
            val x: Char = letter()!!
            return x
        }
    "#;
    let built = common::compare_with_kotlinc_plugin(
        "GenericCastShapes",
        source,
        "GenericCastShapesKt",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    for member in [
        "int bare();",
        "long wide();",
        "byte narrow();",
        "java.lang.Integer boxed();",
        "int plain();",
        "int explicit();",
        "boolean asBool();",
        "char asChar();",
    ] {
        let reference = common::method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "kotlinc emits {member}");
        assert_eq!(
            common::method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}
