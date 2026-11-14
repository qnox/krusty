// MODULE: lib
// FILE: Library.kt
package genericdispatch

abstract class Base<T> {
    abstract fun value(input: T): T
    open fun <R> roundTrip(input: R): R = input
}
class LibraryDerived : Base<String>() {
    override fun value(input: String): String = input + "K"
}
fun produce(): Base<String> = LibraryDerived()

// MODULE: main(lib)
// FILE: Main.kt
import genericdispatch.Base
import genericdispatch.produce

class ConsumerDerived : Base<Int>() {
    override fun value(input: Int): Int = input + 1
}
fun box(): String {
    val strings: Base<String> = produce()
    if (strings.value("O") != "OK") return "producer class application"
    if (strings.roundTrip(7) != 7) return "producer callable substitution"
    if (strings.roundTrip<String?>(null) != null) return "nullable callable substitution"
    val integers: Base<Int> = ConsumerDerived()
    if (integers.value(6) != 7) return "consumer class application"
    if (integers.roundTrip("OK") != "OK") return "inherited callable substitution"
    return "OK"
}
