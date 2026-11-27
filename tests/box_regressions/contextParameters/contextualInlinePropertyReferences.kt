// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +ContextParameters +CallableReferencesToContextual
context(c: String)
inline val topInl: String get() = c + "!"

class A(val t: String) {
    context(c: String)
    inline val m: String get() = t + c
}
context(c: String)
inline val String.ext: String get() = this + c

context(c: String)
fun go(): String {
    val f: () -> String = ::topInl
    if (f() != "X!") return "FAIL1 ${f()}"
    val g: () -> String = A("t")::m
    if (g() != "tX") return "FAIL2 ${g()}"
    val h: () -> String = "R"::ext
    if (h() != "RX") return "FAIL3 ${h()}"
    val k: (A) -> String = A::m
    if (k(A("u")) != "uX") return "FAIL4 ${k(A("u"))}"
    val e: (String) -> String = String::ext
    if (e("Q") != "QX") return "FAIL5 ${e("Q")}"
    return "OK"
}

fun box(): String = with("X") { go() }
