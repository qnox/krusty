// A read of a generic extension property calls its getter, which yields the declared `T`; the read
// is specialized at its use.
class W<T>(val t: T)

val <T> W<T>.value: T get() = t
val <T> (() -> T).asFunction: () -> T get() = this
val String.twice: String get() = this + this

class Scope {
    val <T> W<T>.inScope: T get() = t
    fun read(w: W<String>) = w.inScope
}

fun box(): String {
    val fromClass: String = W("O").value
    val fromLambda = ({ "K" }).asFunction
    val fromFunction: () -> Int = { 2 }
    val count: Int = fromFunction.asFunction() + W(1).value
    val result = fromClass + fromLambda() + "O".twice + Scope().read(W("!")) + count
    return if (result == "OKOO!3") "OK" else "fail $result"
}
