// A library function that tests a value against a file-private interface of its own library and
// calls that interface's member needs the interface installed in the unit that lowers its body.

// MODULE: library
// FILE: library.kt
package installed

private interface Named {
    fun name(): String
}

fun describe(value: Any): String = if (value is Named) value.name() else "other"

// MODULE: main(library)
// FILE: main.kt
import installed.describe

fun box(): String {
    if (describe(1) != "other") return "int"
    if (describe("K") != "other") return "string"
    return "OK"
}
