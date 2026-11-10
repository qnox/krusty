// A reference to a generic `vararg xs: E` adapted to a one-input function type takes the input as
// the whole vararg array only when the input is the vararg's own array classifier (`Array<…>`).
// A primitive array is not an `Array<E>`, so it becomes the single collected element `E`.
// Defaulted parameters after the vararg do not stop the whole-array reading.
fun <T> applyIt(t: T, f: (T) -> Int): Int = f(t)

fun <E> countG(vararg xs: E): Int = xs.size

fun <E> countD(vararg xs: E, k: Int = 10): Int = xs.size + k

fun countI(vararg xs: Int): Int = xs.size

fun box(): String {
    if (applyIt(arrayOf("a", "b"), ::countG) != 2) return "reference array"
    if (applyIt(intArrayOf(1, 2), ::countG) != 1) return "primitive array"
    if (applyIt("x", ::countG) != 1) return "single element"
    if (applyIt(intArrayOf(1, 2), ::countI) != 2) return "primitive vararg"
    if (applyIt(arrayOf("a", "b"), ::countD) != 12) return "array before a default"
    if (applyIt(intArrayOf(1, 2), ::countD) != 11) return "primitive array before a default"
    return "OK"
}
