fun box(): String {
    val text: CharSequence = "hello"
    if (text.subSequence(1, 3).toString() != "el") return "fail string range"
    if (text.subSequence(0, 5).toString() != "hello") return "fail whole string"
    if (text.subSequence(2, 2).toString() != "") return "fail empty range"
    val builder: CharSequence = StringBuilder("hello")
    if (builder.subSequence(1, 4).toString() != "ell") return "fail builder range"
    try {
        text.subSequence(3, 9)
        return "fail string end past length"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        text.subSequence(4, 2)
        return "fail string start past end"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        builder.subSequence(-1, 2)
        return "fail builder negative start"
    } catch (e: IndexOutOfBoundsException) {
    }
    return "OK"
}
