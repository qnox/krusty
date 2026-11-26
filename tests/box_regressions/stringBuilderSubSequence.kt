fun box(): String {
    val builder = StringBuilder("hello")
    val text: CharSequence = builder
    if (text.subSequence(1, 3).toString() != "el") return "slice through CharSequence"
    if (text.subSequence(0, 5).toString() != "hello") return "whole slice"
    if (text.subSequence(5, 5).length != 0) return "empty at the end"
    builder.append(" ")
    if (text.subSequence(4, 6).toString() != "o ") return "slice after an append"
    if ((text as CharSequence).trim().toString() != "hello") return "trim through the slice"
    try {
        text.subSequence(3, 9)
        return "no exception past the end"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        text.subSequence(4, 2)
        return "no exception for a reversed range"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        text.subSequence(-1, 2)
        return "no exception for a negative start"
    } catch (e: IndexOutOfBoundsException) {
    }
    return "OK"
}
