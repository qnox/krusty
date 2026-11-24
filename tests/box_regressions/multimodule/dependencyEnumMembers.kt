// MODULE: library
// FILE: Library.kt
package fixture

// An enum class that declares its own members: a property computed from the constant and a
// function dispatching on it, which a library body calls on a constant it receives.
enum class Start {
    DEFAULT,
    LAZY;

    val isLazy: Boolean
        get() = this === LAZY

    fun run(block: () -> String): String =
        when (this) {
            DEFAULT -> block()
            LAZY -> "lazy:" + name
        }
}

fun launch(start: Start, block: () -> String): String =
    if (start.isLazy) "deferred/" + start.run(block) else start.run(block)

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val eager = launch(Start.DEFAULT) { "ran" }
    if (eager != "ran") return "fail: eager $eager"
    val lazy = launch(Start.LAZY) { "ran" }
    return if (lazy == "deferred/lazy:LAZY") "OK" else "fail: lazy $lazy"
}
