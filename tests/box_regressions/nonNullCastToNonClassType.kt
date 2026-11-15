// `as` to a non-null type that is not a class of the file: the cast checks the value and its
// result is the non-null type, through a concrete target, `Any`, and a type parameter bounded by
// `Any`.

fun narrow(value: Any): String = value as String

fun present(value: Any?): Any = value as Any

fun <T : Any> exact(value: Any?): T = value as T

fun box(): String {
    val text = narrow("O")
    if (text.length != 1) return "narrow: $text"
    if (present(42) != 42) return "present"
    val generic: String = exact("K")
    return text + generic
}
