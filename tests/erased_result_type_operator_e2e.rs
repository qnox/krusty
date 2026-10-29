//! A type operator over a generic call that leaves an erased reference slot (`m["k"] as? String`,
//! `m["k"] is String`, `m["k"] as String`) tests or casts that slot directly, as kotlinc does: the
//! substituted result type's own `checkcast` is not written first. A named binding of the same
//! call still converts to its declared type.

use super::common;

const SRC: &str = "class Ops {\n\
    \x20   fun safeCast(m: Map<String, CharSequence>): String? = m[\"x\"] as? String\n\
    \x20   fun test(m: Map<String, CharSequence>): Boolean = m[\"x\"] is String\n\
    \x20   fun notTest(m: Map<String, CharSequence>): Boolean = m[\"x\"] !is String\n\
    \x20   fun cast(m: Map<String, CharSequence>): String = m[\"x\"] as String\n\
    \x20   fun element(m: List<CharSequence>): String? = m[0] as? String\n\
    \x20   fun subject(m: Map<String, CharSequence>): Int = when (val v = m[\"x\"]) { is String -> v.length; else -> 2 }\n\
    }\n\
    fun box(): String {\n\
    \x20   val ops = Ops()\n\
    \x20   val text = mapOf<String, CharSequence>(\"x\" to \"abc\")\n\
    \x20   val builder = mapOf<String, CharSequence>(\"x\" to StringBuilder(\"b\"))\n\
    \x20   if (ops.safeCast(text) != \"abc\" || ops.safeCast(builder) != null) return \"fail as?\"\n\
    \x20   if (!ops.test(text) || ops.test(builder) || !ops.notTest(builder)) return \"fail is\"\n\
    \x20   if (ops.cast(text) != \"abc\") return \"fail as\"\n\
    \x20   if (ops.element(listOf(StringBuilder())) != null) return \"fail element\"\n\
    \x20   return if (ops.subject(text) == 3 && ops.subject(builder) == 2) \"OK\" else \"fail subject\"\n\
    }\n";

#[test]
fn a_type_operator_tests_an_erased_call_result_directly_like_kotlinc() {
    let built = common::compare_with_kotlinc_plugin(
        "ErasedResultTypeOperator",
        SRC,
        "Ops",
        &[common::stdlib_jar(), common::jdk_modules()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    for method in [
        " safeCast(",
        " test(",
        " notTest(",
        " cast(",
        " element(",
        " subject(",
    ] {
        let reference = common::method_instructions(&built.reference, method);
        assert!(!reference.is_empty(), "kotlinc writes Ops.{method}");
        assert_eq!(
            common::method_instructions(&built.krusty, method),
            reference,
            "Ops.{method}"
        );
    }
}

#[test]
fn a_type_operator_over_an_erased_call_result_keeps_its_semantics() {
    common::expect_box_same_as_kotlinc(SRC, "ErasedResultTypeOperator");
}
