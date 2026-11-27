// A delegated property whose getValue/setValue are inline extensions declared in a dependency.
// Each accessor expands the dependency's inline body; nothing calls the convention itself.
// MODULE: lib
package lib

import kotlin.reflect.KProperty

class Cell(var content: String)

inline operator fun Cell.getValue(thisRef: Any?, property: KProperty<*>): String = content

inline operator fun Cell.setValue(thisRef: Any?, property: KProperty<*>, value: String) {
    content = value
}

class Box<T>(var item: T)

inline operator fun <T> Box<T>.getValue(thisRef: Any?, property: KProperty<*>): T = item

inline operator fun <T> Box<T>.setValue(thisRef: Any?, property: KProperty<*>, value: T) {
    item = value
}

// MODULE: main(lib)
import lib.Box
import lib.Cell
import lib.getValue
import lib.setValue

val shared = Cell("top")
val anyCell: Any = Cell("cast")

var top by shared

class Holder {
    var member by Cell("member")
    val cast by (anyCell as Cell)
    var count by Box(1)
}

fun local(): String {
    var value by Cell("local")
    value += "!"
    return value
}

fun box(): String {
    if (top != "top") return "fail: top $top"
    top = "top2"
    if (shared.content != "top2") return "fail: top write ${shared.content}"
    val holder = Holder()
    if (holder.member != "member") return "fail: member ${holder.member}"
    holder.member = "member2"
    if (holder.member != "member2") return "fail: member write ${holder.member}"
    if (holder.cast != "cast") return "fail: cast ${holder.cast}"
    holder.count += 41
    if (holder.count != 42) return "fail: count ${holder.count}"
    if (local() != "local!") return "fail: local ${local()}"
    return "OK"
}
