// MODULE: lib
// A public inline member of a library class is inlined at its call site, as kotlinc does, so a
// lambda passed to it may return non-locally from the caller, with a value or with Unit.
class Reader(private val text: String) {
    fun readAll(): String = text
}

abstract class Files {
    abstract fun open(path: String): Reader

    inline fun <T> read(path: String, action: Reader.() -> T): T = open(path).action()

    inline fun <T> run(action: () -> T): T = action()
}

class MemoryFiles : Files() {
    override fun open(path: String): Reader = Reader(path)
}

// MODULE: main(lib)
fun firstRead(files: Files): String {
    files.read("O") {
        return readAll() + "K"
    }
}

fun stopEarly(files: Files, log: StringBuilder) {
    files.run {
        log.append("before")
        return
    }
}

fun box(): String {
    val files = MemoryFiles()
    val value = firstRead(files)
    if (value != "OK") return "value: $value"
    val log = StringBuilder()
    stopEarly(files, log)
    if (log.toString() != "before") return "unit: $log"
    return "OK"
}
