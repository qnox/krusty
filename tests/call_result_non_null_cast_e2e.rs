//! PLATFORM: JVM cast guards and complete classfile output, not a new conformance inventory.

use super::common;

const NON_NULL: &str = "fun supplied(): Any = \"OK\"\n\
fun items(): Array<String> = arrayOf(\"OK\")\n\
fun cast(): String = supplied() as String\n\
fun castArray(): Array<String> = items() as Array<String>\n\
fun box(): String = if (cast() == \"OK\") castArray()[0] else \"FAIL\"\n";

const NULLABLE: &str = "var calls = 0\n\
fun supplied(value: Any?): Any? { calls++; return value }\n\
fun <T> identity(value: T): T = value\n\
fun castNullable(value: Any?): String = supplied(value) as String\n\
fun castGeneric(value: Any?): String = identity(value) as String\n\
fun box(): String {\n\
    try { castNullable(null); return \"nullable\" } catch (error: NullPointerException) {}\n\
    try { castGeneric(null); return \"generic\" } catch (error: NullPointerException) {}\n\
    return if (calls == 1) \"OK\" else \"calls\"\n\
}\n";

#[test]
fn call_result_casts_match_kotlinc_including_reference_narrowing() {
    let classes = common::classes_against_kotlinc_module(&[("NonNullCallResult.kt", NON_NULL)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn nullable_and_unbounded_generic_call_results_keep_cast_guards_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("NullableCallResult.kt", NULLABLE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn checked_call_result_casts_preserve_runtime_and_single_evaluation() {
    for (stem, source) in [
        ("NonNullCallResult", NON_NULL),
        ("NullableCallResult", NULLABLE),
    ] {
        assert_eq!(common::kotlinc_box_result(source), "OK", "{stem}");
        assert_jvm_box(source, stem);
    }
}

const ARRAY_SHAPES: &str = r#"
var arrayCalls = 0
fun produced(): Array<String> { arrayCalls++; return arrayOf("OK") }
fun evaluated(): Array<String> = produced() as Array<String>
fun nullable(values: Array<String>?): Array<String> = values as Array<String>
fun nested(values: Array<Array<String>>): Array<Array<CharSequence>> = values as Array<Array<CharSequence>>
fun projected(values: Array<out String>): Array<String> = values as Array<String>
fun consumed(values: Array<in String>): Array<Any?> = values as Array<Any?>
fun narrow(values: Array<Any?>): Array<String> = values as Array<String>
fun primitive(values: IntArray): Array<Int> = values as Array<Int>
fun <T> generic(values: Array<T>): Array<Any?> = values as Array<Any?>
fun box(): String {
    if (evaluated()[0] != "OK" || arrayCalls != 1) return "evaluation"
    val retained: Any? = nullable(null)
    if (retained != null) return "null array"
    if (nested(arrayOf(arrayOf("OK")))[0][0] != "OK") return "nested"
    if (projected(arrayOf("OK"))[0] != "OK") return "out"
    if (consumed(arrayOf<Any?>("OK"))[0] != "OK") return "in"
    if (generic(arrayOf("OK"))[0] != "OK") return "generic"
    try { narrow(arrayOf<Any?>("OK")); return "narrow" } catch (error: ClassCastException) {}
    try { primitive(intArrayOf(42)); return "primitive" } catch (error: ClassCastException) {}
    return "OK"
}
"#;

const DECLARATION_SCOPES: &str = r#"
interface Marker
open class Base
class Both : Base(), Marker
fun <T> expected(values: Array<Base>): Array<T> where T : Base, T : Marker = values as Array<T>
fun <T> actual(values: Array<T>): Array<Marker> where T : Base, T : Marker = values as Array<Marker>
class Holder<T>(input: Array<T>) {
    val values: Array<Any?> = input as Array<Any?>
    fun cast(input: Array<T>): Array<Any?> = input as Array<Any?>
}
inline fun <T> scoped(input: Array<T>, use: (Array<Any?>) -> String): String =
    use(input as Array<Any?>)
fun <T> supplied(input: Array<T>): Array<T> = input
fun <T> lambda(input: Array<T>): String = scoped(input) { values ->
    val cast = supplied(input) as Array<Any?>
    if (values[0] == cast[0]) "OK" else "lambda"
}
fun box(): String {
    val both = arrayOf(Both())
    if (actual(both)[0] !is Marker) return "actual"
    if (expected<Both>(both as Array<Base>)[0] !is Base) return "expected"
    val holder = Holder(arrayOf("OK"))
    if (holder.values[0] != "OK" || holder.cast(arrayOf("OK"))[0] != "OK") return "holder"
    return lambda(arrayOf("OK"))
}
"#;

#[test]
fn compatible_array_shapes_match_complete_kotlinc_classfiles() {
    let classes = common::classes_against_kotlinc_module(&[("CompatibleArrays.kt", ARRAY_SHAPES)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn compatible_array_declaration_scopes_match_complete_kotlinc_classfiles() {
    let classes = common::classes_against_kotlinc_module(&[("ArrayScopes.kt", DECLARATION_SCOPES)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn compatible_arrays_keep_runtime_nulls_narrowing_and_single_evaluation() {
    for (stem, source) in [
        ("CompatibleArrays", ARRAY_SHAPES),
        ("ArrayScopes", DECLARATION_SCOPES),
    ] {
        assert_eq!(common::kotlinc_box_result(source), "OK", "{stem}");
        assert_jvm_box(source, stem);
    }
}

fn assert_jvm_box(source: &str, stem: &str) {
    // PLATFORM: JVM's compatible-array lowering deliberately preserves nullable array values.
    // Other backends' written-cast null checks are not this classfile realization's contract.
    let result = common::compile_and_run_box(
        source,
        stem,
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("JVM compiles and runs the array-cast fixture");
    assert_eq!(result.trim(), "OK", "{stem}");
}
