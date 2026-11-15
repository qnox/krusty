// `is` and `as?` against a library interface answer for the program's classes below it and for
// the builtin values whose classes are below it, from source and from a library body
// (`Iterable.firstOrNull` tests its receiver `is List`).
class Counter(private val last: Int) : Iterator<String> {
    private var next = 1
    override fun hasNext(): Boolean = next <= last
    override fun next(): String = "#" + next++
}

class Numbers(private val last: Int) : Iterable<String> {
    override fun iterator(): Iterator<String> = Counter(last)
}

class Letters : CharSequence {
    override val length: Int get() = 2
    override fun get(index: Int): Char = if (index == 0) 'a' else 'b'
    override fun subSequence(startIndex: Int, endIndex: Int): CharSequence = this
}

fun isIterable(value: Any?): Boolean = value is Iterable<*>
fun isList(value: Any?): Boolean = value is List<*>
fun isCharSequence(value: Any?): Boolean = value is CharSequence
fun isComparable(value: Any?): Boolean = value is Comparable<*>
fun isNotIterator(value: Any): Boolean = value !is Iterator<*>
fun nullableIterable(value: Any?): Boolean = value is Iterable<*>?

fun box(): String {
    if (!isIterable(Numbers(1))) return "fail: Numbers is Iterable"
    if (isList(Numbers(1))) return "fail: Numbers is List"
    if (isIterable("text")) return "fail: String is Iterable"
    if (!isCharSequence("text")) return "fail: String is CharSequence"
    if (!isCharSequence(Letters())) return "fail: Letters is CharSequence"
    if (isCharSequence(Numbers(1))) return "fail: Numbers is CharSequence"
    if (!isComparable("text")) return "fail: String is Comparable"
    if (!isComparable(42)) return "fail: Int is Comparable"
    if (!isComparable('c')) return "fail: Char is Comparable"
    if (!isComparable(7u)) return "fail: UInt is Comparable"
    if (!isComparable(7uL)) return "fail: ULong is Comparable"
    if (isComparable(Letters())) return "fail: Letters is Comparable"
    if (isCharSequence(42)) return "fail: Int is CharSequence"
    if (isNotIterator(Counter(1))) return "fail: Counter !is Iterator"
    if (!nullableIterable(null)) return "fail: null is Iterable?"
    if (isIterable(null)) return "fail: null is Iterable"
    val sequence: Any = Letters()
    if ((sequence as? CharSequence)?.length != 2) return "fail: as? CharSequence"
    if (("text" as Any as? Iterable<*>) != null) return "fail: String as? Iterable"
    if (Numbers(3).firstOrNull() != "#1") return "fail: firstOrNull"
    if (Numbers(0).firstOrNull() != null) return "fail: empty firstOrNull"
    return "OK"
}
