// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A value-class field is as stable as the value class's underlying type, wherever the value class is
// declared, unless the value class applies a stable marker itself. An underlying class of another
// file still reads its `$stable` at run time.
// FILE: other.kt
package model

@JvmInline value class OtherId(val raw: Int)
@JvmInline value class OtherItems(val items: MutableList<Int>)
class Plain(val n: Int)
@JvmInline value class WrapsPlain(val plain: Plain)

// FILE: holders.kt
package model

import androidx.compose.runtime.Stable

@JvmInline value class Id(val raw: Int)
@JvmInline value class Name(val raw: String)
@JvmInline value class Items(val items: MutableList<Int>)
@Stable @JvmInline value class MarkedItems(val items: MutableList<Int>)
@JvmInline value class Nested(val id: Id)

class User(val id: Id)
class Named(val name: Name?)
class HoldsItems(val items: Items)
class HoldsMarked(val items: MarkedItems)
class HoldsNested(val nested: Nested)
class HoldsOther(val id: OtherId)
class HoldsOtherItems(val items: OtherItems)
class HoldsWrapsPlain(val wraps: WrapsPlain)
class HoldsUInt(val value: UInt)
class Mixed(val id: Id, val items: Items)

// FILE: box.kt
package model

private fun stable(name: String): Int = Class.forName(name).getField("\$stable").getInt(null)

private val expected = listOf(
    "model.User" to 0,
    "model.Named" to 0,
    "model.HoldsItems" to 8,
    "model.HoldsMarked" to 0,
    "model.HoldsNested" to 0,
    "model.HoldsOther" to 0,
    "model.HoldsOtherItems" to 8,
    "model.HoldsUInt" to 0,
    "model.Mixed" to 8,
)

fun box(): String {
    for ((name, bits) in expected) {
        if (stable(name) != bits) return "$name: not $bits"
    }
    if (stable("model.HoldsWrapsPlain") != stable("model.Plain")) {
        return "model.HoldsWrapsPlain does not read Plain"
    }
    return "OK"
}
