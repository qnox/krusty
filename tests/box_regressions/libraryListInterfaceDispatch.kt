// A list the stdlib materializes answers type tests and calls through each of its library
// interfaces: `List`, `Collection` and `Iterable`, with the members `AbstractCollection`
// implements for them and the empty list's own `isEmpty`.
fun box(): String {
    val letters: Any = listOf("O", "x", "K")
    val empty: Any = listOf<String>()
    val tests = "${letters is List<*>} ${letters is Collection<*>} ${letters is Iterable<*>} " +
        "${empty is List<*>} ${empty is Collection<*>} ${empty is Iterable<*>}"
    if (tests != "true true true true true true") return tests
    val list = listOf("O", "x", "K")
    val iterable: Iterable<String> = list
    var joined = ""
    for (letter in iterable) joined += letter
    if (joined != "OxK") return joined
    if (!list.any { it == "x" }) return "any"
    if (!list.all { it.length == 1 }) return "all"
    if (!list.any()) return "any()"
    if (listOf<String>() != listOf<String>()) return "empty equals"
    return "OK"
}
