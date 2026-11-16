// MODULE: lib
var seen = 0

inline fun f(x: Int = 1): Int = x + 1

inline fun String.ext(x: Int = 1): Int = length + x

inline fun record(x: Int = 1) {
    seen += x
}

inline fun two(a: String = "a", b: String = a + "b"): String {
    val c = a + b
    return c
}

class C {
    inline fun g(x: Int = 1): Int = x + 2
}

object O {
    inline fun h(x: Int = 1): Int = x + 5
}

// MODULE: main(lib)
fun box(): String {
    record()
    if (seen != 1) return "record"
    if (f() != 2) return "f"
    if ("ab".ext() != 3) return "ext"
    if (two() != "aab") return "two"
    if (two(b = "c") != "ac") return "two named"
    if (C().g() != 3) return "g"
    if (O.h() != 6) return "h"
    return "OK"
}
