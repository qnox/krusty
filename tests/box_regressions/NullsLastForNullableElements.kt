// A library's return-only `T : Comparable<T>` whose result `Order<T?>` is passed where
// `Order<in String?>` is expected is fixed to `String`: the expectation's `String?` reaches `T?`,
// and the self-referential bound is checked against the narrowed `String`, as
// `Comparable<String>`. The stdlib's `nullsLast(): Comparator<T?>` has this shape.
// MODULE: lib
interface Order<in T> {
    fun compare(left: T, right: T): Int
}

class NullableLast<T : Comparable<T>> : Order<T?> {
    override fun compare(left: T?, right: T?): Int = when {
        left === right -> 0
        left == null -> 1
        right == null -> -1
        else -> left.compareTo(right)
    }
}

fun <T : Comparable<T>> nullableLast(): Order<T?> = NullableLast()

// MODULE: main(lib)
fun order(order: Order<in String?>, left: String?, right: String?): String {
    val sign = order.compare(left, right)
    return if (sign < 0) "<" else if (sign > 0) ">" else "="
}

fun box(): String {
    val read = order(nullableLast(), "a", null) + order(nullableLast(), null, "b") +
        order(nullableLast(), "a", "b") + order(nullableLast(), null, null)
    return if (read == "<><=") "OK" else read
}
