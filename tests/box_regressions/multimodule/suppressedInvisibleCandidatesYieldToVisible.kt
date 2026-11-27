// MODULE: lib
// FILE: Explicit.kt
// kotlinc ranks a suppressed invisible candidate as a visibility error, below every visible one:
// an invisible member function or member property yields to a visible extension, and an invisible
// explicitly imported extension property to a visible star-imported one.
package explicit

internal val String.label get() = "explicit label"

// FILE: Star.kt
package star

val Any.label get() = "star label"

internal fun String.only() = "star only"

class Holder {
    internal fun tag() = "member"
    internal val label get() = "member label"
}

// MODULE: main(lib)
// FILE: Main.kt
@file:Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")

import explicit.label
import star.*

fun Holder.tag() = "extension"
val Holder.label get() = "extension label"

fun box(): String {
    val only = "x".only()
    if (only != "star only") return "only: $only"
    val member = Holder().tag()
    if (member != "extension") return "member: $member"
    val memberProperty = Holder().label
    if (memberProperty != "extension label") return "member property: $memberProperty"
    val property = "x".label
    if (property != "star label") return "property: $property"
    return "OK"
}
