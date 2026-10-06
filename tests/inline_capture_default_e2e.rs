//! A `var` an inlined lambda captures stays a plain local, and kotlinc stores the JVM default
//! before the real initializer so the local's range covers that initializer. A `val`, an
//! uncaptured `var`, and a `var` closed over by an ordinary lambda do not.

use super::common;

const SRC: &str = "\
fun capturedVar(p: Int): Int {\n\
    var x = p\n\
    var y = x + 1\n\
    repeat(1) {\n\
        x = y\n\
        y = x\n\
    }\n\
    return x\n\
}\n\
\n\
fun readOnlyVar(p: Int): Int {\n\
    var x = p\n\
    var y = x + 1\n\
    var acc = 0\n\
    repeat(1) { acc = x + y }\n\
    return x + acc\n\
}\n\
\n\
fun capturedVal(p: Int): Int {\n\
    val x = p\n\
    val y = x + 1\n\
    var acc = 0\n\
    repeat(1) { acc += x + y }\n\
    return acc\n\
}\n\
\n\
fun uncaptured(p: Int): Int {\n\
    var x = p\n\
    var y = x + 1\n\
    return x + y\n\
}\n\
\n\
fun capturedAfter(p: Int): Int {\n\
    var x = p\n\
    x = x + 1\n\
    repeat(1) { x = x + 1 }\n\
    return x\n\
}\n\
\n\
fun alreadyZero(p: Int): Int {\n\
    var acc = 0\n\
    repeat(1) { acc += p }\n\
    return acc\n\
}\n\
\n\
fun alreadyOne(p: Int): Int {\n\
    var acc = 1\n\
    repeat(1) { acc += p }\n\
    return acc\n\
}\n\
\n\
fun summed(p: Int): Int {\n\
    var x = p + 1\n\
    repeat(1) { x += 2 }\n\
    return x\n\
}\n\
\n\
fun capturedLong(p: Long): Long {\n\
    var x = p + 1L\n\
    repeat(1) { x += 1L }\n\
    return x\n\
}\n\
\n\
fun capturedString(p: String): String {\n\
    var x = p\n\
    repeat(1) { x = x + \"a\" }\n\
    return x\n\
}\n\
\n\
fun capturedBool(p: Boolean): Boolean {\n\
    var x = p\n\
    repeat(1) { x = !x }\n\
    return x\n\
}\n\
\n\
fun closedOver(p: Int): Int {\n\
    var x = p\n\
    val f = { x + 1 }\n\
    return f()\n\
}\n\
";

#[test]
fn inlined_capture_defaults_match_kotlinc() {
    let methods = [
        "public static final int capturedVar(",
        "public static final int readOnlyVar(",
        "public static final int capturedVal(",
        "public static final int uncaptured(",
        "public static final int capturedAfter(",
        "public static final int alreadyZero(",
        "public static final int alreadyOne(",
        "public static final int summed(",
        "public static final long capturedLong(",
        "public static final java.lang.String capturedString(",
        "public static final boolean capturedBool(",
        "public static final int closedOver(",
    ];
    let results = common::method_code_diffs_against_kotlinc(
        "InlineCaptureDefault",
        &[],
        SRC,
        "InlineCaptureDefaultKt",
        &methods,
    )
    .expect("reference kotlinc is provisioned");
    let failures: Vec<String> = methods
        .into_iter()
        .zip(results)
        .filter_map(|(method, result)| {
            result
                .err()
                .map(|difference| format!("{method}: {difference}"))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
