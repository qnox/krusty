// MODULE: lib
// FILE: Interfaces.kt
package interfacecasts

interface Root<T> { fun value(): T }
interface Left<T> : Root<T>
interface Right<T> : Root<T>
interface Leaf<T> : Left<T>, Right<T>
private class Hidden(val held: String) : Leaf<String> {
    override fun value(): String = held
}
fun makeHidden(): Any = Hidden("OK")

// MODULE: main(lib)
// FILE: Queries.kt
package interfacecasts

fun root(value: Any?): Boolean = value is Root<*>
fun leaf(value: Any?): Leaf<*>? = value as? Leaf<*>
fun cast(value: Any?): String = (value!! as Leaf<*>).value() as String
fun <T> erased(value: Any?): T = value as T
fun <T : Any> nonNullErased(value: Any?): T = value as T

// FILE: Main.kt
package interfacecasts

class Unrelated(val held: String)
fun interface Local : Leaf<String> { override fun value(): String }
var evaluations = 0
fun once(value: Any?): Any? { evaluations++; return value }
fun box(): String {
    val value = makeHidden()
    if (!root(value) || leaf(value) == null || cast(once(value)) != "OK") return "valid"
    if (evaluations != 1) return "evaluation count"
    if (value !is Left<*> || value !is Right<*>) return "transitive diamond"
    val local: Any = Local { "OK" }
    if (!root(local) || leaf(local) == null || cast(local) != "OK") return "SAM membership"
    if (root(null) || leaf(null) != null) return "nullable query"
    val unrelated = Unrelated("sentinel")
    if (root(unrelated) || leaf(unrelated) != null) return "unrelated query"
    try { cast(once(unrelated)); return "cast accepted unrelated" }
    catch (expected: ClassCastException) {}
    if (evaluations != 2 || unrelated.held != "sentinel") return "cast effects"
    try { cast(null); return "cast accepted null" }
    catch (expected: NullPointerException) {}
    if (erased<Any?>(null) != null || erased<Any>(value) !== value) return "erased generic"
    try { nonNullErased<Any>(null); return "erased non-null" }
    catch (expected: NullPointerException) {}
    return "OK"
}
