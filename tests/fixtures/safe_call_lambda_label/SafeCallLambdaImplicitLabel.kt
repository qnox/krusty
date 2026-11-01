// A lambda passed through a safe call takes the callee's name as its implicit label, as it does
// through an ordinary call: `s?.let { return@let }` leaves the lambda.
fun letWithParameter(s: String?): String {
    var result = "none"
    s?.let { value ->
        if (value.startsWith("x")) return@let
        result = value
    }
    return result
}

fun letWithIt(s: String?): String =
    s?.let {
        if (it.isEmpty()) return@let "empty"
        it.uppercase()
    } ?: "null"

fun alsoChain(s: String?): Int {
    var seen = 0
    s?.also {
        if (it.length > 3) return@also
        seen = it.length
    }
    return seen
}

fun box(): String {
    if (letWithParameter("ok") != "ok") return "letWithParameter ok"
    if (letWithParameter("xa") != "none") return "letWithParameter skipped"
    if (letWithParameter(null) != "none") return "letWithParameter null"
    if (letWithIt("ab") != "AB" || letWithIt("") != "empty" || letWithIt(null) != "null") return "letWithIt"
    if (alsoChain("abc") != 3 || alsoChain("abcd") != 0 || alsoChain(null) != 0) return "alsoChain"
    return "OK"
}
