// Kotlin ranks a member the call site cannot see below every resolved candidate, on every
// operator form: a local extension takes the binary, indexed and unary operators from private
// members, and an accessible member overload takes the call from a more specific private one.
class B {
    private operator fun plus(x: Int): String = "fail: member plus"
    private operator fun get(index: Int): String = "fail: member get"
    private operator fun unaryMinus(): String = "fail: member unaryMinus"
}

class A {
    private operator fun plus(x: String): String = "fail: private plus"
    operator fun plus(x: Any): String = "O"
    private operator fun get(index: Int): String = "fail: private get"
    operator fun get(index: Any): String = "K"
}

fun local(): String {
    operator fun B.plus(x: Int): String = "O"
    operator fun B.get(index: Int): String = "K"
    operator fun B.unaryMinus(): String = ""
    return (B() + 1) + B()[0] + -B()
}

fun box(): String {
    val l = local()
    if (l != "OK") return l
    return (A() + "s") + A()[0]
}
