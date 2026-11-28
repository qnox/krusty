// `size` reads an array at its own dispatch slot (`Array<T>` or the primitive array), so a generic
// result reaches it in its declaration's erased slot: an `Array<out E>` property is read as it is,
// and a `T` result is cast only to the array kind, never to the substituted element type. An
// array of arrays held at its own type is still cast to `Array<T>`'s slot.
fun <T> id(t: T): T = t

class Wrap<E>(val items: Array<out E>)

class Holder<E>(val e: E)

fun <T> nullableId(t: T): T? = t

fun box(): String {
    val strings = arrayOf("a", "b")
    val nested = arrayOf(arrayOf("a"), arrayOf("b"), arrayOf("c"))
    val ints = intArrayOf(1, 2, 3, 4)
    val sizes = listOf(
        id(strings).size,
        id(ints).size,
        id(nested).size,
        nested.size,
        Wrap(nested).items.size,
        Wrap(strings).items.size,
        Holder(ints).e.size,
        Holder(strings).e.size,
        nullableId(strings)?.size ?: -1,
    )
    return if (sizes == listOf(2, 4, 3, 3, 3, 2, 4, 2, 2)) "OK" else "FAIL: $sizes"
}
