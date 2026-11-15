// A library body calling abstract interface members (`Sequence.iterator`, `Iterator.hasNext`,
// `Iterator.next`) on values of classes it never sees dispatches to the program's
// implementations, as a source call of the member does.
class Words(private vararg val words: String) : Sequence<String> {
    override fun iterator(): Iterator<String> = object : Iterator<String> {
        private var index = 0
        override fun hasNext(): Boolean = index < words.size
        override fun next(): String = words[index++]
    }
}

fun box(): String {
    if (Words("a", "b").firstOrNull() != "a") return "fail: firstOrNull"
    if (Words().firstOrNull() != null) return "fail: empty firstOrNull"
    return "OK"
}
