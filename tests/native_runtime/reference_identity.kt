// Kotlin's answers for `reference_identity.c`: when two callable references are equal, and which
// calls into the receiver the comparison or the hash makes.
val log = StringBuilder()

class R(val n: Int) {
    fun f() = n
    fun g() = n
    override fun equals(other: Any?): Boolean {
        log.append("eq($n) ")
        return other is R && other.n == n
    }
    override fun hashCode(): Int {
        log.append("hash($n) ")
        return n
    }
}

fun top() = 1
fun other() = 2

fun box(): String {
    val out = StringBuilder()
    fun show(label: String, value: Any?) {
        out.append("$label = $value [${log.toString().trim()}]\n")
        log.setLength(0)
    }
    val a = R(1)
    val a2 = R(1)
    val b = R(2)
    val u1: Any = R::f
    val u2: Any = R::f
    show("unbound, two sites", u1 == u2)
    show("unbound, two sites, identical", u1 === u2)
    show("unbound, two declarations", (R::f as Any) == (R::g as Any))
    show("top-level, two sites", (::top as Any) == (::top as Any))
    show("top-level, two declarations", (::top as Any) == (::other as Any))
    show("bound, one receiver", (a::f as Any) == (a::f as Any))
    show("bound, equal receivers", (a::f as Any) == (a2::f as Any))
    show("bound, receivers that differ", (a::f as Any) == (b::f as Any))
    show("bound vs unbound", (a::f as Any) == (R::f as Any))
    show("unbound vs bound", (R::f as Any) == (a::f as Any))
    show("bound, two declarations", (a::f as Any) == (a::g as Any))
    show("reference vs lambda", (::top as Any) == ({ top() } as Any))
    show("equal unbound references hash alike", u1.hashCode() == u2.hashCode())
    show("equal bound references hash alike", (a::f as Any).hashCode() == (a2::f as Any).hashCode())
    return out.toString()
}
