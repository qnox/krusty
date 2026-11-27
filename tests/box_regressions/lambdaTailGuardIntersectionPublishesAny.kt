// A jumping guard narrows the lambda tail's binding to non-null, and the narrowed bounded
// intersection publishes `Any`, not its bound, as kotlinc approximates it.
interface Base {
    fun base() = "b"
}

interface Base2

class One : Base, Base2

class Two : Base, Base2

fun <S : Base> pick(vararg s: S): S? = s.firstOrNull()

fun guarded(a: One, b: Two) = run {
    val v = pick(a, b)
    if (v == null) throw IllegalStateException()
    v
}

fun take(x: Any) = "any"

fun take(x: Base) = "base"

fun box(): String {
    val result = take(guarded(One(), Two()))
    return if (result == "any") "OK" else "fail $result"
}
