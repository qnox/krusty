// MODULE: lib
package lib

typealias Tagged<T> = List<String>

open class Base
class Child : Base()

class Scope(val tag: String)

context(scope: Scope)
fun <T> scoped(first: T, second: T): Tagged<T> = listOf(scope.tag)

class Factory {
    context(scope: Scope)
    fun <T> pair(first: T, second: T): Tagged<T> = listOf(scope.tag)
}

// MODULE: main(lib)
import lib.*

typealias Labelled<T> = List<String>

context(scope: Scope)
fun <T> labelled(first: T, second: T): Labelled<T> = listOf(scope.tag)

class Maker {
    context(scope: Scope)
    fun <T> pair(first: T, second: T): Labelled<T> = listOf(scope.tag)
}

// The callee's context parameter is supplied from the caller's own context, so selection matches
// the written operands against the value parameters alone. The recorded abbreviation carries the
// argument selection solved over those operands: `scoped(Base(), Child())` is `Tagged<Base>`.
class Contexts(private val factory: Factory, private val maker: Maker) {
    context(scope: Scope)
    fun libraryWidened() = scoped(Base(), Child())

    context(scope: Scope)
    fun libraryNarrowedFirst() = scoped(Child(), Base())

    context(scope: Scope)
    fun libraryMember() = factory.pair(Base(), Child())

    context(scope: Scope)
    fun ownWidened() = labelled(Base(), Child())

    context(scope: Scope)
    fun ownNarrowedFirst() = labelled(Child(), Base())

    context(scope: Scope)
    fun ownMember() = maker.pair(Child(), Base())
}

fun box(): String {
    val contexts = Contexts(Factory(), Maker())
    val library = with(Scope("O")) {
        contexts.libraryWidened() + contexts.libraryNarrowedFirst() + contexts.libraryMember()
    }
    val own = with(Scope("K")) {
        contexts.ownWidened() + contexts.ownNarrowedFirst() + contexts.ownMember()
    }
    if (library.joinToString("") != "OOO") return "fail: $library"
    if (own.joinToString("") != "KKK") return "fail: $own"
    return "OK"
}
