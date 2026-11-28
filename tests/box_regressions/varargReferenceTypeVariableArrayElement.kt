// KOTLINC_VERSION: (,2.5.0)
// A callable reference to a generic `vararg` function whose element is the function's own type
// variable, passed where the enclosing call declares the reference's input as one of its type
// variables and supplies an array. Releases with KT-90169 collect that array as one element,
// although kotlinc first mapped it as the whole array, so the reference reports no vararg
// conversion. A primitive array is mapped the same way. An element that only mentions a type
// variable, a concrete vararg, and a non-array input are mapped element by element.
fun <T> applyIt(t: T, f: (T) -> Int): Int = f(t)
fun <T> applyNullable(t: T, f: (T?) -> Int): Int = f(t)
fun <T> applyAfter(t: T, f: (Int, T) -> Int): Int = f(10, t)
fun <E> countG(vararg xs: E): Int = xs.size
fun <E : Any> countB(vararg xs: E): Int = xs.size
fun <E> countAfter(k: Int, vararg xs: E): Int = k + xs.size
fun countA(vararg xs: Array<String>): Int = xs.size
fun <E> countAE(vararg xs: Array<E>): Int = xs.size

class Box {
    fun <E> count(vararg xs: E): Int = xs.size
}

fun <E> Box.countX(vararg xs: E): Int = xs.size

fun check(name: String, actual: Int, expected: Int): String? =
    if (actual == expected) null else "$name=$actual"

fun box(): String {
    val arr = arrayOf("a", "b")
    val ints = intArrayOf(1, 2, 3)
    fun <E> localCount(vararg xs: E): Int = xs.size
    fun <E : CharSequence> localBounded(vararg xs: E): Int = xs.size
    return check("top", applyIt(arr, ::countG), 1)
        ?: check("nullableInput", applyNullable(arr, ::countG), 1)
        ?: check("explicit", applyIt<Array<String>?>(arr, ::countG), 1)
        ?: check("primitive", applyIt(ints, ::countB), 1)
        ?: check("after", applyAfter(arr, ::countAfter), 11)
        ?: check("member", applyIt(arr, Box()::count), 1)
        ?: check("extension", applyIt(arr, Box()::countX), 1)
        ?: check("local", applyIt(arr, ::localCount), 1)
        ?: check("localBounded", applyIt("s", ::localBounded), 1)
        ?: check("concrete", applyIt(arr, ::countA), 1)
        ?: check("mentioned", applyIt(arrayOf(1), ::countAE), 1)
        ?: check("element", applyIt("s", ::countG), 1)
        ?: "OK"
}
