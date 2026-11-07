// MODULE: lib
package fixture

open class Base<T>(val value: T) {
    inline fun marker(): Int = 1
    inline fun read(): T = value
    inline fun <R> mapped(transform: (T) -> R): R = transform(value)
}
open class Middle<U>(value: U) : Base<U>(value)
class Derived : Middle<String>("O")

// MODULE: main(lib)
import fixture.*

fun box(): String {
    val derived = Derived()
    val middle: Middle<String> = derived
    if (derived.marker() + middle.marker() != 2) return "marker"
    val read: String = derived.read()
    if (read != "O") return "read: $read"
    val mapped = middle.mapped { it + "K" }
    if (mapped != "OK") return "mapped: $mapped"
    return if (mapped + read == "OKO") "OK" else "combined: ${mapped + read}"
}
