// WITH_STDLIB
// FILE: Declaration.kt
interface Step {
    fun step()
}

class Steps {
    inline fun first(crossinline f: (Int) -> Any) = object : Step {
        override fun step() {
            f(1)
        }
    }.step()

    inline fun second(crossinline f: (Int) -> Any) = first { f(1) }
}
// FILE: Caller.kt
fun box(): String {
    var seen = 0
    Steps().second { seen = it }
    return if (seen == 1) "OK" else "seen $seen"
}
