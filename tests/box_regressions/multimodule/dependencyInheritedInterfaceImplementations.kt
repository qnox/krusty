// MODULE: lib
// FILE: Sized.kt
package fixture

interface Sized {
    fun empty(): Boolean
}

// `Listy` redeclares `empty`, so a call through it names `Listy.empty`, which no class below
// overrides by that name.
interface Listy : Sized {
    override fun empty(): Boolean
}

abstract class BaseSized : Sized {
    abstract fun count(): Int

    override fun empty(): Boolean = count() == 0
}

// `BaseListy` reaches `Listy` directly, and every subclass reaches it only through `BaseListy`:
// `BaseSized.empty` implements `Listy.empty` for each of them.
abstract class BaseListy : BaseSized(), Listy

class Items(private val n: Int) : BaseListy() {
    override fun count(): Int = n
}

fun isEmpty(listy: Listy): Boolean = listy.empty()

fun itemsEmpty(n: Int): Boolean = isEmpty(Items(n))

// The same inheritance through classes private to their library.
class Outer {
    private open class Walker : Sized {
        override fun empty(): Boolean = false
    }

    private class ListWalker : Walker(), Listy

    fun walkerEmpty(): Boolean = isEmpty(ListWalker())
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    if (!itemsEmpty(0)) return "itemsEmpty(0)"
    if (itemsEmpty(2)) return "itemsEmpty(2)"
    if (Outer().walkerEmpty()) return "walkerEmpty"
    return "OK"
}
