fun <A, B> valuesOf(items: List<A>, transform: (A) -> B) =
    items.map { mapOf(it to transform(it)) }.flatMap { it.values }

fun box(): String {
    val result: List<String> = valuesOf(listOf(1, 2)) { "v$it" }
    return if (result == listOf("v1", "v2")) "OK" else "FAIL: $result"
}
