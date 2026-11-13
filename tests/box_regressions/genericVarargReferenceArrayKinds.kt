// A reference to a generic `vararg xs: E` adapted to a one-input function type whose input is a
// fixed `Array<…>` takes the input as the whole vararg array. A primitive array is not an `Array<E>`,
// so it becomes the single collected element `E`. Defaulted parameters after the vararg do not stop
// the whole-array reading.
fun <T> applyIt(t: T, f: (T) -> Int): Int = f(t)

fun <E> countG(vararg xs: E): Int = xs.size

fun <E> countD(vararg xs: E, k: Int = 10): Int = xs.size + k

fun countI(vararg xs: Int): Int = xs.size

fun box(): String {
    val whole: (Array<String>) -> Int = ::countG
    if (whole(arrayOf("a", "b")) != 2) return "reference array"
    if (applyIt(intArrayOf(1, 2), ::countG) != 1) return "primitive array"
    if (applyIt("x", ::countG) != 1) return "single element"
    if (applyIt(intArrayOf(1, 2), ::countI) != 2) return "primitive vararg"
    val wholeD: (Array<String>) -> Int = ::countD
    if (wholeD(arrayOf("a", "b")) != 12) return "array before a default"
    if (applyIt(intArrayOf(1, 2), ::countD) != 11) return "primitive array before a default"
    return "OK"
}
