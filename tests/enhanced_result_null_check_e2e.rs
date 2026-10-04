//! A Java result whose nullability an overridden Kotlin declaration fixes is enhanced to not-null,
//! and kotlinc checks it where a declared type receives it, exactly as it checks a flexible value.
//! `StringBuilder.toString()` overrides `Any.toString(): String`, and `ArrayList<E>.get` and
//! `iterator()` override `List<E>.get(): E` and `MutableIterable<E>.iterator()`. Unlike a flexible
//! value, an enhanced one is also checked where a declaration takes its type from it: an inferred
//! local, an inferred function result, and the iterator and element a `for` loop stores. A declared
//! result checks the branch of a conditional that produces the value.

use super::common;

const SOURCE: &str = r#"
fun build(): StringBuilder {
    val builder = StringBuilder()
    builder.append("O")
    builder.append("K")
    return builder
}

fun inferredLocal(): Int {
    val text = build().toString()
    return text.length
}

fun inferredResult() = build().toString()

fun declaredResult(): String = build().toString()

fun declaredConditional(flag: Boolean): String = if (flag) build().toString() else "fail"

fun primitiveElement(): Int {
    val list = ArrayList<Int>()
    list.add(2)
    val value: Int = list.get(0)
    return value
}

fun loopElements(): Int {
    val list = ArrayList<String>()
    list.add("ab")
    var length = 0
    for (item in list) {
        length += item.length
    }
    return length
}

fun <T> nullableElements(list: ArrayList<T>): Int {
    var present = 0
    for (item in list) {
        if (item != null) present++
    }
    return present
}

fun box(): String {
    if (inferredLocal() != 2) return "inferredLocal"
    if (inferredResult() != "OK") return "inferredResult"
    if (declaredResult() != "OK") return "declaredResult"
    if (declaredConditional(true) != "OK") return "declaredConditional"
    if (primitiveElement() != 2) return "primitiveElement"
    if (loopElements() != 2) return "loopElements"
    val list = ArrayList<String?>()
    list.add(null)
    list.add("x")
    if (nullableElements(list) != 1) return "nullableElements"
    return "OK"
}
"#;

#[test]
fn enhanced_java_results_are_checked_like_kotlinc() {
    let pair = common::ModuleClassPair::compile(
        &[("EnhancedResultNullCheck.kt", SOURCE)],
        "EnhancedResultNullCheckKt",
    );
    for method in [
        "inferredLocal",
        "inferredResult",
        "declaredResult",
        "declaredConditional",
        "primitiveElement",
        "loopElements",
        "nullableElements",
    ] {
        let (kotlinc, krusty) = pair.method_code("EnhancedResultNullCheckKt", method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}

#[test]
fn enhanced_java_results_run_like_kotlinc() {
    let jdk = common::jdk_modules();
    let output = common::compile_and_run_box(
        SOURCE,
        "EnhancedResultNullCheck",
        &[common::stdlib_jar()],
        Some(jdk.as_path()),
    )
    .expect("krusty compiles and runs the box");
    assert_eq!(output, "OK");
}
