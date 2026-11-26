// A bounded type parameter's intersection solution publishes its bound, but once it is narrowed to
// non-null (by a guard or `!!`) the narrowed type is a new intersection and publishes `Any`.
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

fun plain(a: One, b: Two) = pickNonNull(a, b)

fun take(x: Any) = "any"

fun take(x: Base) = "base"

fun box(): String {
    val a = One()
    val b = Two()
    val result = take(viaGuard(a, b)) + " " + take(viaBang(a, b)) + " " + take(plain(a, b))
    return if (result == "any any base") "OK" else "fail $result"
}
