// A nested call's formal that only a postponed lambda reaches stays open for the enclosing
// expectation. Which parameter the lambda reaches is the selected call's argument mapping: a named
// argument out of declaration order and a trailing lambda after a vararg land on their own
// parameters, not on the parameter at the argument's source position.
class Sh

class Duo<out A, out B>(val first: A, val second: B)

fun <A, B> duo(a: A, b: B): Duo<A, B> = Duo(a, b)

fun <A, B> gather(vararg a: A, b: B): Duo<List<A>, B> = Duo(a.toList(), b)

fun Sh.single(d: Duo<String, Sh.() -> String>): String = d.first + d.second(this)

fun Sh.many(d: Duo<List<String>, Sh.() -> String>): String =
    d.first.joinToString("") + d.second(this)

fun box(): String {
    val named = Sh().single(duo(b = { "sh" }, a = "x"))
    val trailing = Sh().many(gather("x", "y") { "sh" })
    return if (named == "xsh" && trailing == "xysh") "OK" else "FAIL: $named $trailing"
}
