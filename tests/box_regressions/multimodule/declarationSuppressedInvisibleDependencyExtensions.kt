// MODULE: lib
// FILE: Shared.kt
// A declaration-level suppression of INVISIBLE_REFERENCE and INVISIBLE_MEMBER reaches a
// dependency's internal extension only inside that declaration. kotlinc still prefers a visible
// candidate, suppressed or not, so every call of `tag` selects the public one, in any order.
package shared

internal fun ByteArray.tag(): String = "internal"

internal fun ByteArray.label(): String = "label"

// FILE: q.kt
package q

fun Any.tag(): String = "public"

// MODULE: main(lib)
// FILE: Main.kt
package shared

import q.*

fun plainBefore(bytes: ByteArray): String = bytes.tag()

@Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")
fun suppressedTag(bytes: ByteArray): String = bytes.tag()

@Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")
fun suppressedLabel(bytes: ByteArray): String = bytes.label()

fun plainAfter(bytes: ByteArray): String = bytes.tag()

fun box(): String {
    val bytes = ByteArray(0)
    if (plainBefore(bytes) != "public") return "plain before: " + plainBefore(bytes)
    if (suppressedTag(bytes) != "public") return "suppressed tag: " + suppressedTag(bytes)
    if (suppressedLabel(bytes) != "label") return "suppressed label: " + suppressedLabel(bytes)
    if (plainAfter(bytes) != "public") return "plain after: " + plainAfter(bytes)
    return "OK"
}
