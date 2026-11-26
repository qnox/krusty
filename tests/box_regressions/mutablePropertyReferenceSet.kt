// A mutable property reference is a `KMutableProperty` value: `set` writes the property through
// it, whatever type the program holds it as. Top-level, unbound member, bound member, generic
// member and extension references each write through their own receiver shape.
import kotlin.reflect.KMutableProperty0
import kotlin.reflect.KMutableProperty1

class Holder(var count: Int)

class Box<T>(var content: T)

var top = "a"

var suffix = ""

var String.tagged: String
    get() = this + suffix
    set(value) {
        suffix = value
    }

fun <V> write(property: KMutableProperty0<V>, value: V) = property.set(value)

fun <T, V> write(property: KMutableProperty1<T, V>, receiver: T, value: V) =
    property.set(receiver, value)

fun box(): String {
    (::top).set("b")
    if (top != "b") return "FAIL top-level set: $top"
    write(::top, "c")
    if (top != "c") return "FAIL top-level set through a parameter: $top"

    val holder = Holder(1)
    holder::count.set(2)
    if (holder.count != 2) return "FAIL bound set: ${holder.count}"
    Holder::count.set(holder, 3)
    if (holder.count != 3) return "FAIL unbound set: ${holder.count}"
    write(Holder::count, holder, 4)
    if (holder.count != 4) return "FAIL unbound set through a parameter: ${holder.count}"
    write(holder::count, 5)
    if (holder.count != 5) return "FAIL bound set through a parameter: ${holder.count}"

    val box = Box("a")
    Box<String>::content.set(box, "b")
    if (box.content != "b") return "FAIL generic unbound set: ${box.content}"
    box::content.set("c")
    if (box.content != "c") return "FAIL generic bound set: ${box.content}"
    write(Box<String>::content, box, "d")
    if (box.content != "d") return "FAIL generic set through a parameter: ${box.content}"

    String::tagged.set("x", "y")
    if (suffix != "y") return "FAIL extension set: $suffix"
    "x"::tagged.set("z")
    if (suffix != "z") return "FAIL bound extension set: $suffix"
    return "OK"
}
