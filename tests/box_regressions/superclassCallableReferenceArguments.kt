class B

fun B.g(i: Int): String = "g$i"
fun B.g(s: String, t: String): String = s + t
fun B.h(): Int = 7

open class P(val f: (B, Int) -> String)
class C : P(B::g)

open class Q(val a: Any)
class D : Q(B::h)

fun box(): String {
    if (C().f(B(), 3) != "g3") return "fail selected overload"
    val h = D().a as (B) -> Int
    if (h(B()) != 7) return "fail natural reference"
    return "OK"
}
