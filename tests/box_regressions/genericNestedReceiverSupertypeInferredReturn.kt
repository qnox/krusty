fun <A, B> flattened(items: List<A>, transform: (A) -> B) =
    items.map { listOf(transform(it)) }.flatten()

fun <A, B> nested(items: List<A>, transform: (A) -> B) =
    items.map { listOf(it).map { transform(it) } }

fun box(): String {
    val flat: List<String> = flattened(listOf(1, 2)) { "v$it" }
    val grouped: List<List<String>> = nested(listOf(1, 2)) { "w$it" }
    return if (flat == listOf("v1", "v2") && grouped == listOf(listOf("w1"), listOf("w2"))) "OK" else "FAIL: $flat $grouped"
}
