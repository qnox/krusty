// MODULE: lib
// FILE: Scope.kt
package fixture

// A library lambda calls an abstract member of the lambda's receiver class before any body the
// consumer reaches constructs that class: the call dispatches to the private subclass's override.
abstract class Scope {
    abstract fun put(value: Int)
}

private class Summing : Scope() {
    var sum = 0

    override fun put(value: Int) {
        sum += value
    }
}

fun total(block: Scope.() -> Unit): Int {
    val scope = Summing()
    scope.block()
    return scope.sum
}

fun usesScope(): Int = total {
    put(1)
    put(2)
}

// MODULE: main(lib)
// FILE: main.kt
import fixture.usesScope

fun box(): String {
    val total = usesScope()
    return if (total == 3) "OK" else "FAIL $total"
}
