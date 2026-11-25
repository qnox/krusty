// A property reference prints as `property NAME (Kotlin reflection is not available)` through an
// erased `Any`, a string template and a type parameter, bound or not, read-only or mutable.

var top = "t"

class A(var p: String) {
    val q: String get() = p
}

fun <T> render(value: T): String = "$value"

fun box(): String {
    val a = A("a")
    val references: List<Any> = listOf(::top, A::p, a::p, A::q, a::q)
    val printed = references.map { it.toString() }
    val expected = listOf("top", "p", "p", "q", "q").map { "property $it (Kotlin reflection is not available)" }
    if (printed != expected) return "toString: $printed"
    val template = "${references[0]}"
    if (template != expected[0]) return "template: $template"
    val generic = render(A::p)
    if (generic != expected[1]) return "generic: $generic"
    return "OK"
}
