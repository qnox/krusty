// A generic call nested as another generic call's spread argument keeps its own inference
// variables until the outermost call is solved. In `f(*mk(*none()))` neither `none()` nor `mk(...)`
// has evidence of its own: both reified `T`s come from `f`'s `E` parameter. Completing `none()`
// against `mk`'s still-open `T` instead leaked that formal into a reified type argument.
enum class E { X }

fun f(vararg v: E) = v.size

inline fun <reified T> mk(vararg e: T): Array<T> = arrayOf(*e)

inline fun <reified T> none(): Array<T> = arrayOf()

fun chain() = f(*mk(*none()))

fun box(): String {
    if (chain() != 0) return "chain"
    val nested: Array<String> = arrayOf(*arrayOf(*emptyArray()))
    if (nested.size != 0) return "stdlib"
    return "OK"
}
