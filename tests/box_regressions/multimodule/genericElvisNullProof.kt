// MODULE: lib
// FILE: Library.kt
package genericelvis

fun <T> choose(value: T, fallback: T & Any): T & Any = value ?: fallback

// MODULE: main(lib)
// FILE: Main.kt
import genericelvis.choose

private var evaluations = 0
private fun operand(value: String?): String? {
    evaluations++
    return value
}

fun box(): String {
    val primary: String? = "primary"
    val fallback = "fallback"
    if (choose<String?>(operand(primary), fallback) !== primary) return "present identity"
    if (evaluations != 1) return "present evaluated twice"
    if (choose<String?>(operand(null), fallback) !== fallback) return "fallback identity"
    if (evaluations != 2) return "absent evaluated twice"
    if (choose<Int?>(42, 7) != 42) return "present primitive"
    if (choose<Int?>(null, 7) != 7) return "fallback primitive"
    if (choose<Any?>(primary, fallback) !== primary) return "erased identity"
    if (choose<String>("bounded", fallback) != "bounded") return "non-null argument"
    return "OK"
}
