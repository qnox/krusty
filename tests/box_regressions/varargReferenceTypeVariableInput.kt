// A callable reference to a generic `vararg` function, passed where the enclosing call declares the
// function's input as one of its own type variables (`f: (T) -> Int`). Releases with KT-90169 bind
// that input as one collected element and later ones bind the whole array, so `box()` asserts that
// every such case agrees on one binding; a concrete or array-shaped input takes the whole array on
// every release. The release-specific binding itself is pinned by
// `vararg_reference_type_variable_input_e2e.rs`, which reuses this file. A nullable input is
// admitted only by the element binding, so `varargReferenceNullableTypeVariableInput.kt` owns it.
fun <T> applyIt(t: T, f: (T) -> Int): Int = f(t)
fun <T, R> applyR(t: T, f: (T) -> R): R = f(t)
fun <T> explicit(f: (T) -> Int): (T) -> Int = f
fun <T> applyArray(t: Array<T>, f: (Array<T>) -> Int): Int = f(t)
fun <E> countG(vararg xs: E): Int = xs.size
fun countS(vararg xs: String): Int = xs.size
fun countI(vararg xs: Int): Int = xs.size
fun <E> firstG(vararg xs: E): E = xs[0]
class Wrap<E>(val items: Array<out E>)
fun <E> listG(vararg xs: E): Wrap<E> = Wrap(xs)
// The result's runtime class, read as `Any?`: its static type is the binding's own, so a direct
// `is Array<*>` would be a check that is always true on one binding and always false on the other.
fun isArray(value: Any?): Boolean = value is Array<*>

class Box {
    fun <E> count(vararg xs: E): Int = xs.size
}
fun <E> Box.countX(vararg xs: E): Int = xs.size

class Holder<T>(val t: T) {
    fun run(f: (T) -> Int): Int = f(t)
    fun <U> runOwn(u: U, f: (U) -> Int): Int = f(u)
}

fun describe(): String {
    val arr = arrayOf("a", "b")
    fun <E> localCount(vararg xs: E): Int = xs.size
    val typed: (Array<String>) -> Int = ::countG
    return "top=${applyIt(arr, ::countG)}" +
        ",explicit=${explicit<Array<String>>(::countG)(arr)}" +
        ",let=${arr.let(::countG)}" +
        ",result=${isArray(applyR(arr, ::firstG))}" +
        ",list=${applyR(arr, ::listG).items.size}" +
        ",member=${applyIt(arr, Box()::count)}" +
        ",extension=${applyIt(arr, Box()::countX)}" +
        ",local=${applyIt(arr, ::localCount)}" +
        ",ownFormal=${Holder(1).runOwn(arr, ::countG)}" +
        ",receiverFormal=${Holder(arr).run(::countG)}" +
        ",arrayInput=${applyArray(arr, ::countG)}" +
        ",concreteVararg=${applyIt(arr, ::countS)}" +
        ",primitive=${applyIt(intArrayOf(1, 2, 3), ::countI)}" +
        ",declared=${typed(arr)}"
}

fun bindings(count: Int): String =
    "top=$count,explicit=$count,let=$count,result=${count == 1},list=$count" +
        ",member=$count,extension=$count,local=$count,ownFormal=$count" +
        ",receiverFormal=2,arrayInput=2,concreteVararg=2,primitive=3,declared=2"

fun box(): String {
    val actual = describe()
    return if (actual == bindings(1) || actual == bindings(2)) "OK" else actual
}
