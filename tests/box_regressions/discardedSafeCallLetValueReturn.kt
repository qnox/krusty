// A discarded safe-call `let` returns a value through its label and assigns a captured `var`.
// The labeled return and the fall-through both have to reach the statement after the call.
fun letWithParameter(s: String?): String {
    var result = "none"
    s?.let { v ->
        if (v == "xa") return@let v
        result = v
        v
    }
    return result
}

fun box(): String {
    if (letWithParameter("ok") != "ok") return "ok"
    if (letWithParameter("xa") != "none") return "xa"
    if (letWithParameter(null) != "none") return "null"
    return "OK"
}
