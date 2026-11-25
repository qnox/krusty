// WITH_STDLIB
// A library value class (`Result`) that one file's support unit already materializes is reused by
// a later file's support unit. A boxed instance there still dispatches to the class's own
// overrides: `toString`, `equals` and `hashCode` on a `Result` read as `Any` reach the library's
// implementations instead of declining.
// FILE: helper.kt
fun failureOf(result: Result<Int>): Throwable? = result.exceptionOrNull()

// FILE: main.kt
fun box(): String {
    if (failureOf(Result.success(1)) != null) return "success"
    val first: Any = Result.success(1)
    val second: Any = Result.success(1)
    val other: Any = Result.success(2)
    if (first.toString() != "Success(1)") return "toString " + first.toString()
    if (first != second) return "equals"
    if (first == other) return "not equals"
    if (first.hashCode() != 1.hashCode()) return "hashCode"
    return "OK"
}
