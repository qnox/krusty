// MODULE: lib
// FILE: Boxes.kt
package fixture

// A library body reads a property of a generic class of its own library at a concrete type
// argument. The getter returns its declared type parameter; the read's value is that result at
// the receiver's type argument, as a source read of the property is.
class Box<T>(var item: T)

fun boxed(box: Box<String>): String = box.item

class Holder(private val box: Box<String>) {
    fun held(): String = box.item
}

fun <T> generic(box: Box<T>): T = box.item

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    if (boxed(Box("a")) != "a") return "boxed ${boxed(Box("a"))}"
    if (Holder(Box("b")).held() != "b") return "held ${Holder(Box("b")).held()}"
    if (generic(Box("c")) != "c") return "generic ${generic(Box("c"))}"
    return "OK"
}
