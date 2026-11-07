// WITH_STDLIB
// FILE: Declaration.kt
interface Consumer {
    fun consume(s: String)
}

inline fun cross(crossinline block: (String) -> Unit): Consumer = object : Consumer {
    override fun consume(s: String) {
        block(s)
    }
}

inline fun <T> readAction(crossinline action: () -> T): T = action()

inline fun produce(crossinline make: () -> String): String = object {
    fun get(): String = make()
}.get()
// FILE: Caller.kt
class Container {
    var shown = ""
    private var hidden = ""

    fun viaSetter(): Consumer = cross { shown = it }

    fun viaAccessor(): Consumer = cross { hidden = it }

    fun read() = shown + hidden
}

class Named {
    val value = produce(::name)

    fun name() = "named"
}

fun bar(): Int? = 42

class Foo {
    fun <K> infer(): K = "OK" as K
}

fun early(): String = readAction {
    bar() ?: return@readAction "Failed"
    val foo = Foo()
    foo.infer()
}

fun box(): String {
    val container = Container()
    container.viaSetter().consume("O")
    container.viaAccessor().consume("K")
    if (container.read() != "OK") return "container ${container.read()}"
    if (Named().value != "named") return "named ${Named().value}"
    return early()
}
