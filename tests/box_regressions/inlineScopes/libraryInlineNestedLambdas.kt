// TARGET_BACKEND: JVM_IR
// USE_INLINE_SCOPES_NUMBERS
// WITH_STDLIB

fun box(): String {
    val words = listOf("a", "bb", "ccc")
    val joined = buildString { words.forEach { w -> append(w.length) } }
    val pairs = words.associate { w -> w to w.length }
    return if (joined == "123" && pairs["bb"] == 2) "OK" else "fail"
}
