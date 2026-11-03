// MODULE: lib
package lib

typealias Tagged<T> = List<String>

fun <T> tagged(value: T): Tagged<T> = listOf(value.toString())
fun <T> emptyTagged(): Tagged<T> = emptyList()

class Factory {
    fun <T> made(value: T): Tagged<T> = listOf(value.toString())
    fun <T> none(): Tagged<T> = emptyList()
}

// MODULE: main(lib)
import lib.*

typealias Labelled<T> = List<String>

fun <T> labelled(value: T): Labelled<T> = listOf(value.toString())
fun <T> emptyLabelled(): Labelled<T> = emptyList()

class Maker {
    fun <T> made(value: T): Labelled<T> = listOf(value.toString())
    fun <T> none(): Labelled<T> = emptyList()
}

// The written type arguments are the selection's substitution of an alias argument the expansion
// never uses: `tagged<Any>("O")` is `Tagged<Any>`, not the `Tagged<String>` its argument would
// infer, and `emptyTagged<Int>()` is `Tagged<Int>` with nothing else to infer it from.
class Explicit(factory: Factory, maker: Maker) {
    val libraryWritten = tagged<Any>("O")
    val libraryOnlyWritten = emptyTagged<Int>()
    val libraryMemberWritten = factory.made<CharSequence>("K")
    val libraryMemberOnlyWritten = factory.none<Long>()
    val libraryInferred = tagged('-')
    val ownWritten = labelled<Any>("O")
    val ownOnlyWritten = emptyLabelled<Int>()
    val ownMemberWritten = maker.made<CharSequence>("K")
    val ownMemberOnlyWritten = maker.none<Long>()
    val ownInferred = labelled('-')
}

fun box(): String {
    val explicit = Explicit(Factory(), Maker())
    val library = explicit.libraryWritten + explicit.libraryOnlyWritten +
        explicit.libraryMemberWritten + explicit.libraryMemberOnlyWritten + explicit.libraryInferred
    val own = explicit.ownWritten + explicit.ownOnlyWritten + explicit.ownMemberWritten +
        explicit.ownMemberOnlyWritten + explicit.ownInferred
    if (library.joinToString("") != "OK-") return "fail: $library"
    if (own.joinToString("") != "OK-") return "fail: $own"
    return "OK"
}
