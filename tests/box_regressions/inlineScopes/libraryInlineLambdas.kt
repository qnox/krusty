// TARGET_BACKEND: JVM_IR
// USE_INLINE_SCOPES_NUMBERS
// WITH_STDLIB

fun box(): String {
    val words = listOf("a", "bb", "ccc")
    val lengths = words.map { it.length }.filter { it > 1 }
    val total = lengths.fold(0) { acc, n -> acc + n }
    val first = words.firstOrNull { w -> w.length == total - 3 }
    return if (first == "bb") "OK" else "fail $first"
}
