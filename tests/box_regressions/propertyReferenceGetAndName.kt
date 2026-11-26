// A property reference is a `KProperty` value: `get` reads the property through it, whatever type
// the program holds it as, and `name` answers the property's name. Unbound member, bound member,
// top-level and extension references each read through their own receiver shape.
import kotlin.reflect.KProperty0
import kotlin.reflect.KProperty1

class Holder(val content: String, var count: Int) {
    val doubled: String
        get() = content + content
}

val top = "top"

val String.shout: String
    get() = this + "!"

fun <T, V> read(property: KProperty1<T, V>, receiver: T): V = property.get(receiver)

fun <V> read(property: KProperty0<V>): V = property.get()

fun box(): String {
    val content = Holder::content
    if (content.get(Holder("a", 0)) != "a") return "FAIL unbound get"
    if (content.name != "content") return "FAIL unbound name: ${content.name}"
    if (read(Holder::doubled, Holder("b", 0)) != "bb") return "FAIL getter through a parameter"
    if (Holder::count.get(Holder("c", 3)) != 3) return "FAIL mutable get"

    val holder = Holder("d", 4)
    val bound = holder::content
    if (bound.get() != "d") return "FAIL bound get"
    if (read(bound) != "d") return "FAIL bound get through a parameter"
    if (bound.name != "content") return "FAIL bound name: ${bound.name}"
    if (holder::count.get() != 4) return "FAIL bound mutable get"

    if ((::top).get() != "top") return "FAIL top-level get"
    if ((::top).name != "top") return "FAIL top-level name"

    if (String::shout.get("hi") != "hi!") return "FAIL extension get"
    if (read(String::shout, "x") != "x!") return "FAIL extension get through a parameter"
    if (String::shout.name != "shout") return "FAIL extension name"

    val held: KProperty1<Holder, String> = Holder::content
    if (held != Holder::content) return "FAIL equality"
    if (held.hashCode() != (Holder::content).hashCode()) return "FAIL hash code"
    if (held == Holder::doubled) return "FAIL distinct properties"
    return "OK"
}
