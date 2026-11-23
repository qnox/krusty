// A library function that tests a value against a file-private interface of its own library and
// calls that interface's member needs the interface installed in the unit that lowers its body.
// The library's own private implementor reaches the consumer only as `Any`, so the type test
// succeeds for it and the member call dispatches to its override.

// MODULE: library
// FILE: library.kt
package installed

private interface Named {
    fun name(): String
}

private class Labelled(private val label: String) : Named {
    override fun name(): String = label
}

fun labelled(label: String): Any = Labelled(label)

fun describe(value: Any): String = if (value is Named) value.name() else "other"

// MODULE: main(library)
// FILE: main.kt
import installed.describe
import installed.labelled

fun box(): String {
    if (describe(1) != "other") return "int"
    if (describe("K") != "other") return "string"
    if (describe(labelled("O")) != "O") return "named: ${describe(labelled("O"))}"
    return "OK"
}
