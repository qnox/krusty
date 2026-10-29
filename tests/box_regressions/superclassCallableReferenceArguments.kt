class B

fun B.g(i: Int): String = "g$i"
fun B.g(s: String, t: String): String = s + t
fun B.h(): Int = 7

open class P(f: (B, Int) -> String) {
    val result = f(B(), 3)
}
class C : P(B::g)

open class Q(a: Any) {
    val named = a is Function1<*, *>
}
class D : Q(B::h)

fun box(): String {
    if (C().result != "g3") return "fail selected overload"
    if (!D().named) return "fail natural reference"
    return "OK"
}
