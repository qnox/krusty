// LANGUAGE: +ContextParameters +CallableReferencesToContextual
// KOTLINC_VERSION: [2.5.0,)
// WITH_STDLIB
import kotlin.jvm.JvmInline
// A function reference binds a value-class context value at its site. The value travels boxed with
// the reference and reaches the declaration as the value class it declares.

@JvmInline
value class Z(val value: String)

@JvmInline
value class N(val value: Int)

context(z: Z)
fun render(suffix: String): String = z.value + suffix

context(z: Z, n: N)
fun both(prefix: String): String = prefix + z.value + n.value

context(z: Z?)
fun nullable(): String = z?.value ?: "null"

fun box(): String {
    val f = context(Z("O")) { val f: (String) -> String = ::render; f }
    if (f("K") != "OK") return "FAIL 1: ${f("K")}"
    val g = context(Z("z"), N(7)) { val g: (String) -> String = ::both; g }
    if (g("-") != "-z7") return "FAIL 2: ${g("-")}"
    val h = context<Z?, () -> String>(null) { ::nullable }
    if (h() != "null") return "FAIL 3: ${h()}"
    val r1 = context(Z("A")) { val r: (String) -> String = ::render; r }
    val r2 = context(Z("B")) { val r: (String) -> String = ::render; r }
    val r3 = context(Z("A")) { val r: (String) -> String = ::render; r }
    if (r1 == r2) return "FAIL 4"
    if (r1 != r3 || r1.hashCode() != r3.hashCode()) return "FAIL 5"
    return "OK"
}
