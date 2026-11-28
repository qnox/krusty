// Before its parameter completes it, a result-only producer argument (`emptyList()`) is typed by its
// classifier alone; it still fits a nullable parameter naming that classifier.
class Box<T>(val value: T?)

fun <T> boxOf(defaultValue: List<T>? = null): Box<T> = Box(defaultValue?.firstOrNull())

val fromEmptyList = boxOf<String>(emptyList())
val fromListOf = boxOf<String>(listOf())

fun inferred() = boxOf<String>(emptyList())

fun box(): String {
    if (fromEmptyList.value != null) return "FAIL emptyList"
    if (fromListOf.value != null) return "FAIL listOf"
    if (inferred().value != null) return "FAIL inferred"
    return "OK"
}
