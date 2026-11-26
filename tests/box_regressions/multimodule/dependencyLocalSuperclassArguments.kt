// MODULE: library
// FILE: Library.kt
package fixture

abstract class Base(val name: String, val active: Boolean, val weight: Int) {
    fun describe(): String = "$name:$active:$weight"
}

private class Fixed(name: String) : Base(name, true, name.length * 2)

class Outer(val prefix: String) {
    private inner class Inside(weight: Int) : Base("$prefix-in", false, weight + 1)

    fun inside(weight: Int): String = Inside(weight).describe()
}

fun fixed(name: String): String = describe(Fixed(name))

fun describe(base: Base): String = base.describe()

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val fixed = fixed("abc")
    if (fixed != "abc:true:6") return "fail: fixed $fixed"
    val inside = Outer("out").inside(4)
    return if (inside == "out-in:false:5") "OK" else "fail: inside $inside"
}
