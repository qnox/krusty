// WITH_STDLIB
// Two applicable overloads whose instantiated parameters do not order (`Collection<Int>?` against
// the vararg element `Any`) are ordered by their declarations: `T` of the vararg one can take
// `Collection<T>?`, so the collection overload is the more specific.
fun <T, C : Collection<T>> C.pick(expected: Collection<T>?): String = "collection"
fun <T, C : Collection<T>> C.pick(vararg expected: T): String = "vararg"

fun inferred(values: List<Int>) = values.pick(values)

fun box(): String {
    val inferred = inferred(listOf(1))
    val declared: String = listOf(2).pick(listOf(2))
    val elements = listOf(3).pick(3, 4)
    return if (inferred == "collection" && declared == "collection" && elements == "vararg") "OK"
    else "fail: $inferred $declared $elements"
}
