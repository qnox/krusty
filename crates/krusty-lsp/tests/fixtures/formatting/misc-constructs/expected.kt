@Deprecated("x")
class A {
    companion object {
        const val C = 1
    }

    val x = 1
        get() = field
}

fun main() {
    val r = 1..10
    val neg = -1
    val b = !true
    loop@ for (i in r) {
        if (i == 2) break@loop
    }
    val s =
        when {
            r.isEmpty() -> "empty"
            else -> "non-empty"
        }
}
typealias Handler = (Int) -> Unit
