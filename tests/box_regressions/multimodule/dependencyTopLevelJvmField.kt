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

// The field of an `internal` property is public on the JVM; the property stays internal.
@JvmField
internal val shadow: String = "library"

// MODULE: main(library)
// FILE: other.kt
package other

val shadow: String = "other"

// FILE: main.kt
import fieldlib.*
import other.*

private val described = marker!! + "!"

fun length() = marker?.length

fun bump() = counter + 1

fun box(): String {
    if (described != "marker!") return "described"
    if (length() != 6) return "length"
    counter = 4
    if (bump() != 5 || fieldlib.counter != 4) return "counter"
    if (shadow != "other") return "shadow"
    return "OK"
}
