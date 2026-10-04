//! A Java result whose nullability an overridden Kotlin declaration fixes is enhanced to not-null,
//! and kotlinc checks it where a declared type receives it, exactly as it checks a flexible value.
//! `StringBuilder.toString()` overrides `Any.toString(): String`, and `ArrayList<E>.get` and
//! `iterator()` override `List<E>.get(): E` and `MutableIterable<E>.iterator()`. Unlike a flexible
//! value, an enhanced one is also checked where a declaration takes its type from it: an inferred
//! local, an inferred function result, and the iterator and element a `for` loop stores. A declared
//! result checks each branch of a conditional that produces such a value, and only those; a braced
//! branch is checked around its block, with no name in the failure.

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

fun kotlinText(): String = "OK"

fun bracedSingle(flag: Boolean): String = if (flag) { build().toString() } else "fail"

fun nestedWithoutStatements(flag: Boolean): String = if (flag) {
    if (flag) build().toString() else "inner"
} else "fail"

fun kotlinBranches(first: Boolean, second: Boolean): String = if (first) {
    build().append("")
    kotlinText()
} else if (second) kotlinText() else build().toString()

fun declaredBlock(flag: Boolean): String = if (flag) {
    build().append("")
    build().toString()
} else "fail"

fun declaredNestedBlock(flag: Boolean): String = if (flag) {
    build().append("")
    if (flag) {
        build().append("")
        build().toString()
    } else "inner"
} else "fail"

fun declaredWhenBlocks(choice: Int): String = when (choice) {
    0 -> {
        build().append("")
        build().toString()
    }
    1 -> {
        if (choice > 0) {
            build().append("")
            build().toString()
        } else "inner"
    }
    else -> "fail"
}

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
    if (bracedSingle(true) != "OK") return "bracedSingle"
    if (nestedWithoutStatements(true) != "OK") return "nestedWithoutStatements"
    if (kotlinBranches(true, false) != "OK") return "kotlinBranches first"
    if (kotlinBranches(false, true) != "OK") return "kotlinBranches second"
    if (kotlinBranches(false, false) != "OK") return "kotlinBranches last"
    if (declaredBlock(true) != "OK") return "declaredBlock"
    if (declaredNestedBlock(true) != "OK") return "declaredNestedBlock"
    if (declaredWhenBlocks(0) != "OK") return "declaredWhenBlocks 0"
    if (declaredWhenBlocks(1) != "OK") return "declaredWhenBlocks 1"
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
        "bracedSingle",
        "nestedWithoutStatements",
        "kotlinBranches",
        "declaredBlock",
        "declaredNestedBlock",
        "declaredWhenBlocks",
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
