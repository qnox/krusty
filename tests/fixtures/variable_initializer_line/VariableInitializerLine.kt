// kotlinc's `visitVariable` marks a local's initializer line once the value is on the stack, then
// the declaration's line. When the initializer starts on a later line than `val`, both entries
// stay at the store.

fun chosen(count: Int?, name: String): String {
    val merged =
        if (count != null && name.isNotEmpty()) {
            name
        } else {
            "none"
        }
    return merged
}

fun matched(count: Int, name: String): String {
    val merged =
        when (count) {
            1 -> name
            else -> "none"
        }
    return merged
}

fun platformResult(count: Int, name: String): String {
    val tail =
        name.substring(
            count,
        )
    return tail
}

fun scoped(count: Int, name: String): String {
    val joined =
        name.let {
            it + count
        }
    return joined
}

fun trimmed(name: String): String {
    val start = name
        .trimStart()
    return start
}

fun box(): String {
    if (chosen(1, "a") != "a" || chosen(null, "a") != "none") return "fail chosen"
    if (matched(1, "a") != "a" || matched(2, "a") != "none") return "fail matched"
    if (platformResult(1, "abc") != "bc") return "fail platformResult"
    if (scoped(1, "a") != "a1") return "fail scoped"
    if (trimmed("  a") != "a") return "fail trimmed"
    return "OK"
}
