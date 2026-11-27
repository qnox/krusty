// A bounded type parameter's intersection solution publishes its bound, but once a guard narrows it
// to non-null the narrowed type is a new intersection and publishes `Any`. A `!!` does the same
// before kotlinc 2.5, which keeps the bound; `describe()` reports which release's answer applies.
interface Base {
    fun base() = "b"
}

interface Base2

class One : Base, Base2

class Two : Base, Base2

fun <S : Base> pick(vararg s: S): S? = s.firstOrNull()

fun <S : Base> pickNonNull(vararg s: S): S = s[0]

fun viaGuard(a: One, b: Two) = pick(a, b).let { if (it != null) it else throw IllegalStateException() }

fun viaBang(a: One, b: Two) = pick(a, b)!!

fun viaElvis(a: One, b: Two) = pick(a, b) ?: throw IllegalStateException()

fun plain(a: One, b: Two) = pickNonNull(a, b)

fun take(x: Any) = "any"

fun take(x: Base) = "base"

fun describe(): String = take(viaBang(One(), Two()))

fun box(): String {
    val a = One()
    val b = Two()
    val result = take(viaGuard(a, b)) + " " + take(plain(a, b))
    // Every not-null operator publishes the same type in a given release, whichever it is.
    val operators = take(viaBang(a, b)) + " " + take(viaElvis(a, b))
    if (operators != "any any" && operators != "base base") return "fail $operators"
    return if (result == "any base") "OK" else "fail $result"
}
