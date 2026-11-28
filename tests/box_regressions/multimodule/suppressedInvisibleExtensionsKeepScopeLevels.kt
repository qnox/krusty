// KOTLINC_VERSION: (,2.4.20)
// MODULE: lib
// FILE: Explicit.kt
// Before 2.4.20, kotlinc's suppressed invisible candidates still compete scope level by scope
// level: the explicitly imported `Any` extensions take the calls from the more specific
// star-imported `String` ones.
package explicit

internal fun Any.tag() = "explicit"

internal val Any.mark get() = "explicit"

// FILE: Star.kt
package star

internal fun String.tag() = "star"

internal val String.mark get() = "star"

// MODULE: main(lib)
// FILE: Main.kt
@file:Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")

import explicit.mark
import explicit.tag
import star.*

fun box(): String {
    val tag = "x".tag()
    if (tag != "explicit") return "tag: $tag"
    val mark = "x".mark
    if (mark != "explicit") return "mark: $mark"
    return "OK"
}
