// `String.CASE_INSENSITIVE_ORDER` is a library property whose initializer is a SAM-converted
// lambda, `Comparator { a, b -> a.compareTo(b, ignoreCase = true) }`.
fun box(): String {
    val order = String.CASE_INSENSITIVE_ORDER
    if (order.compare("a", "B") >= 0) return "a before B"
    if (order.compare("b", "A") <= 0) return "b after A"
    if (order.compare("Ab", "aB") != 0) return "equal ignoring case"
    return "OK"
}
