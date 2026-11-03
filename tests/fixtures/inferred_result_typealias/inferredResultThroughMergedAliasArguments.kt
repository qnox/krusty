// MODULE: lib
package lib

typealias Tagged<T> = List<String>

open class Base
class Child : Base()
class Other : Base()

fun <T> tagged(first: T, second: T): Tagged<T> = listOf("O")

class Factory {
    fun <T> pair(first: T, second: T): Tagged<T> = listOf("K")
}

// MODULE: main(lib)
import lib.*

typealias Labelled<T> = List<String>

fun <T> labelled(first: T, second: T): Labelled<T> = listOf("O")

class Maker {
    fun <T> pair(first: T, second: T): Labelled<T> = listOf("K")
}

// Several operands constrain a type parameter that the alias expansion never uses. The recorded
// abbreviation carries the selection's solved argument, their common supertype, not either
// operand's own type: `tagged(Child(), Base())` is `Tagged<Base>`.
class Merged(factory: Factory, maker: Maker) {
    val libraryWidened = tagged(Child(), Base())
    val librarySiblings = tagged(Child(), Other())
    val libraryMember = factory.pair(Base(), Child())
    val ownWidened = labelled(Child(), Base())
    val ownSiblings = labelled(Other(), Child())
    val ownMember = maker.pair(Child(), Base())
}

fun box(): String {
    val merged = Merged(Factory(), Maker())
    val library = merged.libraryWidened + merged.librarySiblings + merged.libraryMember
    val own = merged.ownWidened + merged.ownSiblings + merged.ownMember
    if (library.joinToString("") != "OOK") return "fail: $library"
    if (own.joinToString("") != "OOK") return "fail: $own"
    return "OK"
}
