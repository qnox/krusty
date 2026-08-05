//! A generic classpath function must widen its type parameter to the common
//! nullable supertype when one argument is non-null and another is nullable
//! (`kotlin.test.assertEquals("literal", nullableActual)` shape).

use super::common;

const LIB: &str = "package lib\n\
     class Tag(val name: String)\n\
     fun <T> eq(expected: T, actual: T): String = if (expected == actual) \"eq\" else \"ne\"\n\
     fun <T> eqd(expected: T, actual: T, message: String? = null): String =\n\
     \x20 (message ?: \"\") + (if (expected == actual) \"eq\" else \"ne\")\n\
     fun maybe(flag: Boolean): String? = if (flag) \"x\" else null\n\
     fun maybeTag(flag: Boolean): Tag? = if (flag) Tag(\"t\") else null\n\
";

#[test]
fn nullable_actual_widens_generic_param_from_string_literal() {
    let main = "import lib.eq\n\
        import lib.maybe\n\
        fun box(): String {\n\
        \x20 if (eq(\"x\", maybe(true)) != \"eq\") return \"fail same\"\n\
        \x20 if (eq(\"x\", maybe(false)) != \"ne\") return \"fail null\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpgenericnullwiden", LIB, main);
}

#[test]
fn nullable_actual_widens_generic_param_for_class_type() {
    let main = "import lib.Tag\n\
        import lib.eq\n\
        import lib.maybeTag\n\
        fun box(): String {\n\
        \x20 if (eq(Tag(\"t\"), maybeTag(false)) != \"ne\") return \"fail null\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpgenericnullwidenclass", LIB, main);
}

#[test]
fn nullable_expected_with_non_null_actual_is_accepted() {
    let main = "import lib.eq\n\
        import lib.maybe\n\
        fun box(): String {\n\
        \x20 if (eq(maybe(false), \"x\") != \"ne\") return \"fail\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpgenericnullwidenswap", LIB, main);
}

#[test]
fn nullable_actual_widens_generic_param_on_a_defaulted_call() {
    // `kotlin.test.assertEquals(expected, actual, message = null)` — the 2-arg call resolves
    // through the `$default` synthetic, which must inherit the base's generic signature.
    let main = "import lib.eqd\n\
        import lib.maybe\n\
        fun box(): String {\n\
        \x20 if (eqd(\"x\", maybe(true)) != \"eq\") return \"fail same\"\n\
        \x20 if (eqd(\"x\", maybe(false)) != \"ne\") return \"fail null\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpgenericnullwidendefault", LIB, main);
}

#[test]
fn source_level_generic_also_widens_to_nullable() {
    let main = "fun <T> localEq(a: T, b: T): Boolean = a == b\n\
        fun maybe(flag: Boolean): String? = if (flag) \"x\" else null\n\
        fun box(): String {\n\
        \x20 if (localEq(\"x\", maybe(true)) != true) return \"fail same\"\n\
        \x20 if (localEq(\"x\", maybe(false)) != false) return \"fail null\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("srcgenericnullwiden", LIB, main);
}
