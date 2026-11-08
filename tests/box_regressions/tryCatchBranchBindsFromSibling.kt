// A `try` with catches completes like an `if` or a `when`: a branch whose generic call leaves its
// result open takes its type arguments from the other branches. In statement position a branch
// written as a block does the same; its trailing call is not a statement of its own.
class Pack<T>(val value: T?)

fun <T> empty(): Pack<T> = Pack(null)

var calls = ""

fun parse(fail: Boolean): Pack<String> {
    calls += "p"
    if (fail) throw IllegalStateException()
    return Pack("a")
}

fun catchBindsFromBody(fail: Boolean): String? {
    val parsed = try {
        parse(fail)
    } catch (t: Throwable) {
        empty()
    }
    return parsed.value
}

fun bodyBindsFromCatch(fail: Boolean): String? {
    val parsed = try {
        if (fail) throw IllegalStateException()
        empty()
    } catch (t: Throwable) {
        parse(false)
    }
    return parsed.value
}

fun statements(fail: Boolean) {
    try { parse(fail) } catch (e: Throwable) { empty() }
    if (fail) { parse(false) } else { empty() }
    when { fail -> { empty() } else -> { parse(false) } }
}

fun box(): String {
    if (catchBindsFromBody(false) != "a") return "fail1"
    if (catchBindsFromBody(true) != null) return "fail2"
    if (bodyBindsFromCatch(false) != null) return "fail3"
    if (bodyBindsFromCatch(true) != "a") return "fail4"
    calls = ""
    statements(true)
    statements(false)
    if (calls != "pppp") return "fail5:$calls"
    return "OK"
}
