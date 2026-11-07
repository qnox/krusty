// A dependency's member functions with default arguments: a call that omits them selects the
// member like any other, and the callee evaluates each omitted default after the supplied ones.
// MODULE: lib
var evaluated = ""
val trace: String get() = evaluated
fun mark(step: String): String {
    evaluated += step
    return step
}
class C(val base: Int) {
    fun g(x: Int = 1): Int = base + x
    inline fun ig(x: Int = 1): Int = base + x
    fun two(a: String = mark("a"), b: String = a + mark("b")): String = a + b
    fun String.ext(times: Int = length): Int = times + base
}
object O {
    fun h(x: Int = 5): Int = x
}
open class Base {
    open fun f(x: Int = 9): Int = x
}
class Derived : Base() {
    override fun f(x: Int): Int = x + 1
}
// MODULE: main(lib)
fun box(): String {
    if (C(2).g() != 3) return "fail: g()"
    if (C(2).g(5) != 7) return "fail: g(5)"
    if (C(2).ig() != 3) return "fail: ig()"
    if (C(0).two() != "aab") return "fail: two()"
    if (C(0).two(b = "c") != "ac") return "fail: two(b)"
    if (trace != "aba") return "fail: evaluated $trace"
    if (with(C(1)) { "abc".ext() } != 4) return "fail: ext()"
    if (O.h() != 5) return "fail: h()"
    if (Derived().f() != 10) return "fail: f()"
    val base: Base = Derived()
    if (base.f() != 10) return "fail: virtual f()"
    return "OK"
}
