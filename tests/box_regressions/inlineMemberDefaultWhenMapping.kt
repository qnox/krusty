// A `when` that only a public inline member's `$default` wrapper evaluates: the wrapper carries
// the copied body, so its `$WhenMappings` table is numbered in the member's class.
enum class E { A, B }

class Picker {
    inline fun pick(e: E, s: String = when (e) {
        E.A -> "OK"
        E.B -> "FAIL"
    }): String = s
}

fun box(): String = Picker().pick(E.A, "OK")
