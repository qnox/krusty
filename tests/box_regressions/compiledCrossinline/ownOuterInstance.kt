// WITH_STDLIB
// FILE: OwnOuterInstance.kt
class A(val s: String) {
    inline fun wrap(crossinline f: () -> String): () -> String = object : () -> String {
        override fun invoke() = s + f()
    }

    fun go(): String = wrap { s + "K" }()
}

class B(val t: String) {
    fun go(a: A): String = a.wrap { t }()
}

fun box(): String {
    val same = A("O").go()
    if (same != "OOK") return "same type $same"
    val other = B("K").go(A("O"))
    if (other != "OK") return "other type $other"
    return "OK"
}
