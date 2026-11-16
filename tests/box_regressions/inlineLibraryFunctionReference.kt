// A reference to an inline library function is that function's call, so the adapter behind
// the reference runs the function's own body.
fun <T, R> apply(value: T, f: (T) -> R): R = f(value)

fun box(): String {
    val notEmpty: (CharSequence) -> Boolean = CharSequence::isNotEmpty
    if (!notEmpty("a") || notEmpty("")) return "fail unbound"

    val bound: () -> Boolean = "abc"::isNotEmpty
    if (!bound()) return "fail bound"

    val orEmpty: (String?) -> String = String?::orEmpty
    if (orEmpty(null) != "" || orEmpty("x") != "x") return "fail nullable receiver"

    if (!apply("b", CharSequence::isNotEmpty)) return "fail argument"
    if (apply(null, String?::orEmpty) != "") return "fail generic argument"
    return "OK"
}
