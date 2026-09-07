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
