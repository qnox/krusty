// MODULE: lib
// A public inline member of a library class is inlined at its call site, as kotlinc does. A
// receiver lambda passed to it sees the member's declared receiver, and a member inherited by a
// subclass is inlined for a receiver of that subclass too.
class Reader(private val text: String) {
    fun readAll(): String = text
}

abstract class Files {
    abstract fun open(path: String): Reader

    inline fun <T> read(path: String, action: Reader.() -> T): T = open(path).action()
}

class MemoryFiles : Files() {
    override fun open(path: String): Reader = Reader(path)
}

open class E(val x: String) {
    open inner class Base {
        inline fun foo(y: String) = x + y
    }

    inner class Sub : Base()
}

// MODULE: main(lib)
fun box(): String {
    val value = MemoryFiles().read("O") { readAll() + "K" }
    if (value != "OK") return "receiver lambda: $value"
    val sub = E("O").Sub().foo("K")
    if (sub != "OK") return "subclass receiver: $sub"
    return "OK"
}
