// MODULE: lib
// FILE: Library.kt

package fixture
class Generic<T>(seed: T, effect: (T) -> T) {
    private val stored = effect(seed)
    init { effect(seed) }
}
fun <T> construct(seed: T, effect: (T) -> T): Generic<T> = Generic(seed, effect)
// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    var intCalls = 0
    construct(Int.MIN_VALUE) {
        if (it != Int.MIN_VALUE) throw IllegalStateException("Int carrier")
        intCalls++
        it
    }
    var longCalls = 0
    construct(Long.MIN_VALUE) {
        if (it != Long.MIN_VALUE) throw IllegalStateException("Long carrier")
        longCalls++
        it
    }
    var text = ""
    construct("OK") { text += it; it }
    var nullCalls = 0
    construct<String?>(null) {
        if (it != null) throw IllegalStateException("nullable carrier")
        nullCalls++
        it
    }
    return if (intCalls == 2 && longCalls == 2 && text == "OKOK" && nullCalls == 2)
        "OK" else "constructor effects"
}
