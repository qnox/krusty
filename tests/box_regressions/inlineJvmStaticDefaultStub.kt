// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: `@JvmStatic` and `@JvmInline` exist only on the JVM, where kotlinc emits their
// `$default` stubs as static facades and value-class statics.
// A `@JvmStatic` inline member's `$default` stub has no receiver parameter. Its copied body still
// reads each parameter of mixed scalar and reference types from its own slot.
object S {
    @JvmStatic inline fun f(n: Int = 1, s: String = "OK"): String = s + n
    @JvmStatic inline fun g(s: String = "O", n: Long = 2L, t: String = s + "K"): String = t + n
}

class H {
    companion object {
        @JvmStatic inline fun c(n: Int = 3, s: String = "K"): String = s + n
    }
}

object O {
    @JvmStatic inline fun hs(x: Int = 1): Int = x + 6
}

class K {
    companion object {
        @JvmStatic inline fun cs(x: Int = 1): Int = x + 8
    }
}

@JvmInline value class V(val v: Int) {
    inline fun plus(x: Int = 1): Int = v + x
}

fun box(): String {
    if (S.f() != "OK1") return "FAIL f: " + S.f()
    if (S.f(s = "A") != "A1") return "FAIL f named"
    if (S.g() != "OK2") return "FAIL g: " + S.g()
    if (S.g(n = 5L) != "OK5") return "FAIL g named"
    if (H.c() != "K3") return "FAIL c: " + H.c()
    if (O.hs() != 7 || K.cs() != 9 || V(1).plus() != 2) return "FAIL members: ${O.hs()} ${K.cs()} ${V(1).plus()}"
    return "OK"
}
