// KOTLINC_VERSION: [2.5.0,)
// From Kotlin 2.5 (ExplicitInlineValueClassesSyntax) `inline value class` and `value inline class`
// declare an inline value class with no `@JvmInline`, the same declaration as a legacy
// `inline class`. Each is unboxed where its type is known and boxed where it is not: as `Any`, as
// a nullable value and as a class's type argument.
inline value class Meters(val value: Int)
value inline class Name(val text: String)
inline value class Wrapped(val meters: Meters)

fun twice(m: Meters): Meters = Meters(m.value * 2)
fun <T> same(value: T): T = value
class Holder<T>(val first: T, val second: T?)
fun orNull(name: Name?): String = name?.text ?: "none"

fun box(): String {
    val m = twice(Meters(21))
    if (m.value != 42) return "fail value ${m.value}"
    if ((m as Any) != Meters(42)) return "fail boxed equality"
    if (m.toString() != "Meters(value=42)") return "fail toString $m"
    if (m.hashCode() != Meters(42).hashCode()) return "fail hashCode"
    if (same(Name("x")).text != "x") return "fail generic"
    if (orNull(null) != "none" || orNull(Name("y")) != "y") return "fail nullable"
    if (Wrapped(Meters(1)).meters.value != 1) return "fail nested"
    val pair = Holder(Name("O"), Name("K"))
    return pair.first.text + orNull(pair.second)
}
