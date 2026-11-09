// A suspend lambda is a function value on every target: with a parameter, with none, with an
// extension receiver, with a capture, and with a `Unit` result. A suspend function whose body
// never suspends is declared beside them.
suspend fun completesInPlace(x: Int): Int = x + 1

fun box(): String {
    val captured = "K"
    val n: suspend (Int) -> Int = { it }
    val s: suspend () -> String = { "O" + captured }
    val e: suspend Int.() -> Int = { this + 1 }
    val u: suspend () -> Unit = {}
    val first: Any = n
    val second: Any = s
    val third: Any = e
    val fourth: Any = u
    return if (first !== second && third !== fourth) "OK" else "fail"
}
