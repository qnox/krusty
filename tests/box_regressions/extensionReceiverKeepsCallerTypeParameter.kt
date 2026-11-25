// WITH_STDLIB

// An extension whose receiver is a supertype of the call's receiver (`Collection<T>` read from
// `List<Q>`) binds `T` to the caller's own type parameter `Q`, not to `Any`.
fun <Q> copyOf(list: List<Q>) = list.toMutableList()

fun box(): String {
    val copy: MutableList<Boolean> = copyOf(listOf(true))
    copy.add(false)
    return if (copy == listOf(true, false)) "OK" else "Fail: $copy"
}
