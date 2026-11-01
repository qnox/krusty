// MODULE: lib
package lib

typealias Done = Unit
typealias Names = List<String>
typealias Boxed<T> = List<T>

fun finish(block: () -> Unit): Done = block()
fun names(): Names = listOf("O")
fun <T> boxed(value: T): Boxed<T> = listOf(value)
fun maybeNames(): Names? = null

class Holder {
    fun names(): Names = listOf("K")
}

// MODULE: main(lib)
import lib.*

// Each inferred result is reached through a call whose declaration spelled its result through a
// typealias: kotlinc records that alias as the result's abbreviation in `@Metadata`.
class Results(private val holder: Holder) {
    fun done() = finish { }
    fun first() = names()
    fun boxedCount() = boxed(1)
    fun missing() = maybeNames()
    fun present() = names()!!
    fun member() = holder.names()
    fun either(flag: Boolean) = if (flag) names() else names()
}

fun box(): String {
    val results = Results(Holder())
    results.done()
    if (results.missing() != null || results.boxedCount().single() != 1) return "fail"
    if (results.present() != results.either(true)) return "fail either"
    return results.first().single() + results.member().single()
}
