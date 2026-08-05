//! A generic classpath function must widen its type parameter to the common
//! nullable supertype when one argument is non-null and another is nullable
//! (`kotlin.test.assertEquals("literal", nullableActual)` shape).

use super::common;

const LIB: &str = "package lib\n\
     class Tag(val name: String)\n\
     fun <T> eq(expected: T, actual: T): String = if (expected == actual) \"eq\" else \"ne\"\n\
     fun <T> eqd(expected: T, actual: T, message: String? = null): String =\n\
     \x20 (message ?: \"\") + (if (expected == actual) \"eq\" else \"ne\")\n\
     fun <T : Any> eqnn(expected: T, actual: T): String = if (expected == actual) \"eq\" else \"ne\"\n\
     fun <T> firstOf(head: T, vararg rest: T): String = (head ?: rest.firstOrNull()).toString()\n\
     fun <T> tld(expected: T, actual: T, message: String? = null, render: () -> String): String =\n\
     \x20 render() + (if (expected == actual) \"eq\" else \"ne\")\n\
     fun maybe(flag: Boolean): String? = if (flag) \"x\" else null\n\
     fun maybeInt(flag: Boolean): Int? = if (flag) 7 else null\n\
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
fn nullable_join_still_rejected_by_a_non_null_upper_bound() {
    // `fun <T : Any> eqnn` — kotlinc refuses `T := String?` (bound violation), so the nullable
    // widening must not leak past a non-null upper bound.
    let main = "import lib.eqnn\n\
        import lib.maybe\n\
        fun box(): String {\n\
        \x20 return eqnn(\"x\", maybe(true))\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpgenericnullbound", LIB, main) {
        assert!(
            !diags.is_empty(),
            "a nullable join must still violate a non-null upper bound"
        );
    }
}

#[test]
fn explicit_type_argument_overrides_the_nullable_join() {
    // `eq<String>("x", nullable)` — the explicit argument fixes `T := String`, so the nullable
    // actual is a mismatch (kotlinc rejects it too).
    let main = "import lib.eq\n\
        import lib.maybe\n\
        fun box(): String {\n\
        \x20 return eq<String>(\"x\", maybe(true))\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpgenericexplicittarg", LIB, main) {
        assert!(
            !diags.is_empty(),
            "an explicit type argument must pin T and reject the nullable actual"
        );
    }
}

#[test]
fn null_literal_actual_widens_generic_param() {
    let main = "import lib.eq\n\
        fun box(): String {\n\
        \x20 if (eq(\"x\", null) != \"ne\") return \"fail\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpgenericnullliteral", LIB, main);
}

#[test]
fn nullable_primitive_actual_widens_generic_param() {
    let main = "import lib.eq\n\
        import lib.maybeInt\n\
        fun box(): String {\n\
        \x20 if (eq(7, maybeInt(true)) != \"eq\") return \"fail same\"\n\
        \x20 if (eq(7, maybeInt(false)) != \"ne\") return \"fail null\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpgenericnullprimitive", LIB, main);
}

#[test]
fn named_parameter_omitting_call_widens_generic_param() {
    // The labelled form that omits `message` routes through the named-slot `$default` branch.
    let main = "import lib.eqd\n\
        import lib.maybe\n\
        fun box(): String {\n\
        \x20 if (eqd(expected = \"x\", actual = maybe(false)) != \"ne\") return \"fail\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpgenericnullnamed", LIB, main);
}

#[test]
fn nullable_fixed_prefix_before_a_vararg_widens_generic_param() {
    let main = "import lib.firstOf\n\
        import lib.maybe\n\
        fun box(): String {\n\
        \x20 if (firstOf(maybe(false), \"x\") != \"x\") return \"fail\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpgenericnullvarargprefix", LIB, main);
}

#[test]
fn nullable_prefix_of_a_defaulted_trailing_lambda_call_widens_generic_param() {
    // Omits `message` but passes the trailing lambda — the default-trailing-lambda branch.
    let main = "import lib.tld\n\
        import lib.maybe\n\
        fun box(): String {\n\
        \x20 if (tld(\"x\", maybe(false)) { \"r\" } != \"rne\") return \"fail\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_against("cpgenericnulltrailing", LIB, main);
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
