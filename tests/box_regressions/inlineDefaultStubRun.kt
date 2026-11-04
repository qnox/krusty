var seen = 0
inline fun f(x: Int = 1): Int = x + 1
private inline fun hidden(x: Int = 1): Int = x + 3
inline fun <reified T> isA(x: Any? = null): Boolean = x is T
suspend inline fun later(x: Int = 1): Int = x + 4
inline fun String.ext(x: Int = 1): Int = length + x
inline fun u(x: Int = 1) {
    seen += x
}
inline fun two(a: String = "a", b: String = a + "b"): String {
    val c = a + b
    return c
}
var log = ""
inline fun guarded(x: String = "a"): String {
    try {
        log += x
        return log
    } finally {
        log += "f"
    }
}
inline fun loop(n: Int = 3): Int {
    var total = 0
    for (i in 0 until n) {
        total += i
    }
    return total
}
enum class E { A, B }
class C {
    inline fun g(x: Int = 1): Int = x + 2
    private inline fun hidden(x: Int = 1): Int = x + 2
    inline fun String.mext(x: Int = 1): Int = length + x
}
class Picker {
    inline fun pick(e: E, s: String = when (e) { E.A -> "OK"; E.B -> "FAIL" }): String = s
}
object O {
    inline fun h(x: Int = 1): Int = x + 5
    @JvmStatic inline fun hs(x: Int = 1): Int = x + 6
}
class K {
    companion object {
        inline fun c(x: Int = 1): Int = x + 7
        @JvmStatic inline fun cs(x: Int = 1): Int = x + 8
    }
}
open class Op {
    inline fun og(x: Int = 1): Int = x + 9
}
@JvmInline value class V(val v: Int) {
    inline fun plus(x: Int = 1): Int = v + x
}
inline fun caught(result: String = try { error("x") } catch (e: IllegalStateException) { "Y" }): String =
    result
fun useHidden(): Int = hidden()
class D {
    private inline fun hidden(x: Int = 1): Int = x + 2
    fun useHidden(): Int = hidden()
}
fun box(): String {
    u()
    val sum = f() + useHidden() + "ab".ext() + C().g() + D().useHidden() + with(C()) { "a".mext() } +
        O.h() + O.hs() + K.c() + K.cs() + Op().og() + V(1).plus()
    if (seen != 1 || isA<String>() || two() != "aab") return "FAIL: shapes"
    if (guarded() != "a" || log != "af" || loop() != 3 || caught() != "Y") return "FAIL: statements"
    if (Picker().pick(E.A) != "OK") return "FAIL: pick"
    return if (sum == 2 + 4 + 3 + 3 + 3 + 2 + 6 + 7 + 8 + 9 + 10 + 2) "OK" else "FAIL: $sum"
}
