// Callable-reference overloads rank by their declarations as written, not by the parameter types
// one expected shape instantiates them to. `pick(x: T)` and `pick(x: Holder<T>)` both read
// `Holder<String>` here, yet `Holder<T>` is the more specific declaration, for top-level, bound
// member, and bound or unbound extension references alike. A vararg that takes a whole
// `Array<String>` is likewise more specific than a single `T` element.
class Holder<T>(val value: T)

fun <T> pick(x: T): Int = 1
fun <T> pick(x: Holder<T>): Int = 2

fun <T : Any> spread(element: T?): Int = 1
fun <T : Any> spread(vararg elements: T?): Int = 2

class Picker {
    fun <T> pick(x: T): Int = 1
    fun <T> pick(x: Holder<T>): Int = 2
}

fun <T> String.tagged(x: T): Int = 1
fun <T> String.tagged(x: Holder<T>): Int = 2

fun <T, R> apply1(t: T, f: (T) -> R): R = f(t)

fun box(): String {
    val holder = Holder("a")
    val arr = arrayOf("a", "b")
    val top: (Holder<String>) -> Int = ::pick
    if (top(holder) != 2) return "top-level"
    if (apply1(holder, ::pick) != 2) return "top-level in a generic call"
    val array: (Array<String>) -> Int = ::spread
    if (array(arr) != 2) return "whole vararg array"
    val member: (Holder<String>) -> Int = Picker()::pick
    if (member(holder) != 2) return "bound member"
    val extension: (Holder<String>) -> Int = "s"::tagged
    if (extension(holder) != 2) return "bound extension"
    val unbound: (String, Holder<String>) -> Int = String::tagged
    if (unbound("s", holder) != 2) return "unbound extension"
    return "OK"
}
