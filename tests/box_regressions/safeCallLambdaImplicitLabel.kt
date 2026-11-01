// A lambda passed through a safe call takes the callee's name as its implicit label, as it does
// through an ordinary call: `s?.let { return@let … }` leaves the lambda.
fun letWithParameter(s: String?): String =
    s?.let { value ->
        if (value == "xa") return@let "none"
        value
    } ?: "none"

fun letWithIt(s: String?): String =
    s?.let {
        if (it.length == 0) return@let "empty"
        it + "!"
    } ?: "null"

fun runLength(s: String?): Int =
    s?.run {
        if (length > 3) return@run 0
        length
    } ?: -1

fun box(): String {
    if (letWithParameter("ok") != "ok") return "letWithParameter ok"
    if (letWithParameter("xa") != "none") return "letWithParameter skipped"
    if (letWithParameter(null) != "none") return "letWithParameter null"
    if (letWithIt("ab") != "ab!" || letWithIt("") != "empty") return "letWithIt"
    if (letWithIt(null) != "null") return "letWithIt null"
    if (runLength("abc") != 3 || runLength("abcd") != 0 || runLength(null) != -1) return "runLength"
    return "OK"
}
