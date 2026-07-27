//! Smart-cast within an `&&` condition: after `x is T` (or `x != null`) on the left, `x` is `T` while
//! evaluating the right operand (`x is String && x.length == 1`). Round-tripped under `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn smartcast_in_and_condition() {
    const SRC: &str = "fun check(x: Any): Boolean = x is String && x.length == 2\n\
fun box(): String {\n\
    if (!check(\"ok\")) return \"fail string\"\n\
    if (check(\"too long\")) return \"fail len\"\n\
    if (check(42)) return \"fail nonstring\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("smart-cast in && should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn smartcast_in_or_negated_condition() {
    // `x !is String || x.length` — reaching the `||` RHS means `x` IS a `String` (the LHS was false).
    const SRC: &str = "fun lenOk(x: Any): Boolean {\n\
    if (x !is String || x.length != 2) return false\n\
    return true\n\
}\n\
fun box(): String {\n\
    if (!lenOk(\"ok\")) return \"fail string\"\n\
    if (lenOk(\"too long\")) return \"fail len\"\n\
    if (lenOk(42)) return \"fail nonstring\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("smart-cast in || (negated) should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn nested_or_rhs_sees_every_false_lhs_fact() {
    const SRC: &str = "fun length(x: String?, y: String?): Int {\n\
    if (x == null || y == null || x.length + y.length == 0) return 0\n\
    return x.length + y.length\n\
}\n\
fun box(): String = if (length(\"ab\", \"cde\") == 5 && length(null, \"x\") == 0) \"OK\" else \"FAIL\"\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(run(SRC), Some("OK".to_string()));
}

#[test]
fn or_guard_and_else_narrowings_survive_lambda_capture() {
    const SRC: &str = "class Adapter {\n\
    fun convert(value: Int): Int = value + 1\n\
}\n\
inline fun execute(block: () -> Int): Int = block()\n\
fun use(adapter: Adapter?): Int {\n\
    val marker: String? = \"ready\"\n\
    if (marker == null || adapter == null) return 0\n\
    return execute { marker.length + adapter.convert(36) }\n\
}\n\
fun guardedLength(value: Any?): Int {\n\
    if (value == null || value !is String?) return 0\n\
    return value.length\n\
}\n\
fun elseLength(value: Any?): Int =\n\
    if (value == null || value !is String?) 0 else value.length\n\
fun box(): String =\n\
    if (use(Adapter()) == 42 && use(null) == 0 &&\n\
        guardedLength(\"abc\") == 3 && guardedLength(null) == 0 &&\n\
        elseLength(\"abcd\") == 4 && elseLength(1) == 0) \"OK\" else \"FAIL\"\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        run(SRC).expect("guarded local val smartcast survives lambda capture"),
        "OK"
    );
}

#[test]
fn unrelated_false_facts_do_not_pick_a_type_by_declaration_order() {
    const SRC: &str = "interface Left { fun left(): Int }\n\
interface Right { fun right(): Int }\n\
fun use(value: Any): Int {\n\
    if (value !is Left || value !is Right) return 0\n\
    return value.left() + value.right()\n\
}\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("left"))
            && diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("right")),
        "{diagnostics:?}"
    );
}

#[test]
fn or_guard_does_not_narrow_to_an_unmodeled_value_class() {
    const SRC: &str = "@JvmInline value class Token(val value: Int)\n\
fun use(value: Any?): Int {\n\
    if (value !is Token || false) return 0\n\
    return value.value\n\
}\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        !diagnostics.is_empty(),
        "value-class narrowing must stay rejected"
    );
}
