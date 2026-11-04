// WITH_STDLIB

open class Leaf(val value: Int)
open class Container(val child: Leaf)

class Owner {
    fun make(): Container = object : Container(object : Leaf(7) {}) {}
}

fun box(): String = if (Owner().make().child.value == 7) "OK" else "wrong value"
