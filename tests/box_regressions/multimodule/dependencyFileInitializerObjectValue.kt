// MODULE: lib
// FILE: token.kt
object InitializerToken

// FILE: stored.kt
private val stored: Any? = InitializerToken

fun readInitializedObject(): Any? = stored

// MODULE: main(lib)
// FILE: main.kt
fun box(): String {
    val first = readInitializedObject()
    if (first == null) return "null initialized object"
    val second = readInitializedObject()
    if (second == null) return "null repeated object"
    if (first !== second) return "changed initialized object"
    return "OK"
}
