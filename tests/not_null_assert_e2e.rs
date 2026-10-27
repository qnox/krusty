//! The not-null assertion `x!!` — `kotlin/jvm/internal/Intrinsics.checkNotNull` on a duplicate of the
//! value (yields the value, throwing on null). Round-tripped against the JVM under `-Xverify:all`.

use super::common;

#[test]
fn not_null_assert_runs() {
    let src = "fun pick(b: Boolean): String? = if (b) \"hi\" else null\n\
fun len(s: String): Int = s.length\n\
fun box(): String {\n\
val x: String? = pick(true)\n\
if (x!! != \"hi\") return \"f1\"\n\
if (len(pick(true)!!) != 2) return \"f2\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "N");
}

#[test]
fn inferred_signature_flow_facts_round_trip_through_a_dependency() {
    let lib = r#"package flow
class Runtime(val status: String, val mock: Boolean)
class Backend { fun status(): Runtime? = Runtime("ready", true) }
fun bang(backend: Backend) = run {
    val result = backend.status()
    val length = result!!.status.length
    result.status + length
}
fun asserted(backend: Backend) = run {
    val result = backend.status()
    check(result != null)
    result.status
}
fun safeEquality(result: Runtime?) = run {
    if (result?.mock == true) result.status else "missing"
}
fun safeInequality(result: Runtime?) = run {
    if (result?.status != null) result.mock else false
}
"#;
    let main = r#"import flow.*
fun box(): String {
    val backend = Backend()
    val runtime = Runtime("ready", true)
    return if (
        bang(backend) == "ready5" &&
        asserted(backend) == "ready" &&
        safeEquality(runtime) == "ready" &&
        safeInequality(runtime)
    ) "OK" else "FAIL"
}
"#;
    assert_eq!(
        common::expect_box_run_against("inferred-signature-flow-facts", lib, main),
        Some("OK".to_string())
    );
}

/// kotlinc states a `!!`'s own line before its operand and keeps it when the operand's first
/// instruction marks another line at the same offset: `foo(\n a)!!` writes `line 7: 0, line 8: 0`.
#[test]
fn a_not_null_assertion_keeps_its_own_line_beside_its_operands_line() {
    let source = "class Box(val v: Int?) { fun get(a: Int): Int? = v }\n\
         fun foo(a: Int): Int? = a\n\
         fun bar(a: Int): Int = a\n\
         fun any(a: Int): Any? = a\n\
         \n\
         fun declared(a: Int): Int {\n\
         \x20   val y = foo(\n\
         \x20       a)!!\n\
         \x20   return y\n\
         }\n\
         fun argument(a: Int): Int {\n\
         \x20   return bar(foo(\n\
         \x20       a)!!)\n\
         }\n\
         fun body(a: Int): Int = foo(\n\
         \x20   a)!!\n\
         fun member(b: Box, a: Int): Int = b.get(\n\
         \x20   a)!!\n\
         fun later(a: Int): Int {\n\
         \x20   val y =\n\
         \x20       foo(\n\
         \x20           a)!!\n\
         \x20   return y\n\
         }\n\
         fun coerced(a: Int): Any {\n\
         \x20   val y: Any = any(\n\
         \x20       a)!!\n\
         \x20   return y\n\
         }\n\
         fun oneLine(a: Int): Int = foo(a)!! + 1\n\
         fun property(b: Box): Int = b\n\
         \x20   .v!!\n\
         \n\
         fun box(): String {\n\
         \x20   val total = declared(1) + argument(2) + body(3) + member(Box(4), 0) + later(5) +\n\
         \x20       (coerced(6) as Int) + oneLine(6) + property(Box(8))\n\
         \x20   return if (total == 36) \"OK\" else \"fail $total\"\n\
         }\n\
         ";
    common::expect_box_ok_with_stdlib(source, "NotNullOwnLine");
    let classes = common::classes_against_kotlinc_module(&[("NotNullOwnLine.kt", source)]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
