// KOTLINC_VERSION: (,2.5.0)
// A callable reference to a generic `vararg` function, passed where the enclosing call declares the
// function's input as a nullable use of its own type variable (`f: (T?) -> Int`). Releases with
// KT-90169 bind that input as one collected element, which a nullable argument can supply. From
// 2.5.0 the reference's vararg takes the whole non-null array, so kotlinc rejects this program.
fun <T> applyNullable(t: T, f: (T?) -> Int): Int = f(t)
fun <E> countG(vararg xs: E): Int = xs.size

fun box(): String {
    val arr = arrayOf("a", "b")
    val count = applyNullable(arr, ::countG)
    return if (count == 1) "OK" else "FAIL: nullable=$count"
}
