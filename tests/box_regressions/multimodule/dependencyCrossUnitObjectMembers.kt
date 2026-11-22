// MODULE: lib
// FILE: Checked.kt
package fixture

abstract class Checked {
    fun at(index: Int): Int = check(index)

    companion object {
        fun check(index: Int): Int = index + 1
    }
}

class Item : Checked()

fun laterUse(index: Int): Int = Checked.check(index) + 1

// MODULE: main(lib)
// FILE: First.kt
package fixture

// The first checked source lowers `Checked`, whose body reads its companion.
fun first(): Int = Item().at(1)

// FILE: Consumer.kt
package fixture

// The later one lowers `laterUse`, whose body calls a member of the companion the first realized:
// the call runs through that unit's export, not a second copy of the object.
fun box(): String {
    if (first() != 2) return "first:${first()}"
    val later = laterUse(1)
    return if (later == 3) "OK" else "second:$later"
}
