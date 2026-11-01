// TARGET_BACKEND: JVM_IR
// MODULE: library
// FILE: library.kt
// A dependency's top-level `@JvmField` property has no getter: its facade field is its only
// accessor. A declaration whose type is inferred from a read of it still resolves the property.
package fieldlib

@JvmField
val marker: String? = "marker"

@JvmField
var counter: Int = 1

// MODULE: main(library)
// FILE: main.kt
import fieldlib.counter
import fieldlib.marker

private val described = marker!! + "!"

fun length() = marker?.length

fun bump() = counter + 1

fun box(): String {
    if (described != "marker!") return "described"
    if (length() != 6) return "length"
    counter = 4
    if (bump() != 5 || fieldlib.counter != 4) return "counter"
    return "OK"
}
