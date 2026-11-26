// MODULE: lib
package lib

typealias Tagged<T> = List<String>
typealias Paired<T> = Map<T, String>

class Store<T>(private val key: T) {
    val tagged: Tagged<T>? = listOf("a")
    val paired: Paired<T> = mapOf(key to "b")

    fun taggedOf(): Tagged<T> = listOf("c")
    fun <R> pairedWith(value: R): Paired<R> = mapOf(value to "d")
}

fun <T> taggedFor(value: T): Tagged<T> = listOf("e")

// MODULE: main(lib)
import lib.*

typealias Marked<T> = List<String>

class Own<T> {
    val marked: Marked<T>? = listOf("f")

    fun markedOf(): Marked<T> = listOf("g")
}

// Each selected declaration spells its result through an alias application whose arguments the
// expansion does not use, or uses only partly: the inferred declaration records the application
// with the selection's substituted arguments, as kotlinc writes it.
class Arguments(store: Store<Int>, own: Own<Long>) {
    val libraryRead = store.tagged
    val libraryCall = store.taggedOf()
    val libraryPaired = store.paired
    val libraryGenericCall = store.pairedWith('x')
    val libraryTopLevel = taggedFor(true)
    val libraryChainedRead = Store("k").tagged
    val libraryChainedCall = Store(1.0).taggedOf()
    val ownRead = own.marked
    val ownCall = own.markedOf()
    val ownChainedRead = Own<Short>().marked
    val ownChainedCall = Own<Byte>().markedOf()
}

fun box(): String {
    val arguments = Arguments(Store(1), Own())
    val lists = listOfNotNull(
        arguments.libraryRead,
        arguments.libraryCall,
        arguments.libraryTopLevel,
        arguments.libraryChainedRead,
        arguments.libraryChainedCall,
        arguments.ownRead,
        arguments.ownCall,
        arguments.ownChainedRead,
        arguments.ownChainedCall,
    ).flatten()
    if (lists.joinToString("") != "aceacfgfg") return "fail: $lists"
    if (arguments.libraryPaired[1] != "b") return "fail: paired"
    if (arguments.libraryGenericCall['x'] != "d") return "fail: generic"
    return "OK"
}
