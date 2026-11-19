fun box(): String {
    val s = "hello"
    val text: CharSequence = s
    if (s.subSequence(1, 3).toString() != "el") return "direct slice"
    if (text.subSequence(0, 5).toString() != "hello") return "slice through CharSequence"
    if (s.subSequence(2, 2).length != 0) return "empty slice"
    if (s.subSequence(0, 0).toString() != "") return "empty at the start"
    if (s.subSequence(5, 5).toString() != "") return "empty at the end"
    try {
        s.subSequence(3, 9)
        return "no exception past the end"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        s.subSequence(4, 2)
        return "no exception for a reversed range"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        s.subSequence(-1, 2)
        return "no exception for a negative start"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        text.subSequence(3, 9)
        return "no exception past the end through CharSequence"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        text.subSequence(4, 2)
        return "no exception for a reversed range through CharSequence"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        text.subSequence(-1, 2)
        return "no exception for a negative start through CharSequence"
    } catch (e: IndexOutOfBoundsException) {
    }
    return "OK"
}
