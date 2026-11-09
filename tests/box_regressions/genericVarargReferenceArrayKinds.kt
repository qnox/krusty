// A reference to a generic `vararg xs: E` adapted to a one-input function type takes the input as
// the whole vararg array only when the input is the vararg's own array classifier (`Array<…>`).
// A primitive array is not an `Array<E>`, so it becomes the single collected element `E`.
fun <T> applyIt(t: T, f: (T) -> Int): Int = f(t)

fun <E> countG(vararg xs: E): Int = xs.size

fun countI(vararg xs: Int): Int = xs.size

fun box(): String {
    if (applyIt(arrayOf("a", "b"), ::countG) != 2) return "reference array"
    if (applyIt(intArrayOf(1, 2), ::countG) != 1) return "primitive array"
    if (applyIt(longArrayOf(1L, 2L, 3L), ::countG) != 1) return "long array"
    if (applyIt("x", ::countG) != 1) return "single element"
    if (applyIt(intArrayOf(1, 2), ::countI) != 2) return "primitive vararg"
    return "OK"
}
