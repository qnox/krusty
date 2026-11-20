// MODULE: common
// FILE: common.kt
package p

expect open class Lockable()

// MODULE: main()()(common)
// FILE: main.kt
package p

actual typealias Lockable = Any

class Scope : Lockable() {
    fun describe(other: Any): String = other.toString()

    override fun toString(): String = "Scope"
}

fun box(): String {
    val scope = Scope()
    if (scope.describe(1) != "1") return "describe ${scope.describe(1)}"
    return if (scope.toString() == "Scope") "OK" else "toString ${scope.toString()}"
}
