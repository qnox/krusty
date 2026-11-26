// MODULE: library
// FILE: Library.kt
package fixture

abstract class Task<T>(private val label: String) {
    fun start(prefix: String): String = "$prefix:$label:${run()}"

    abstract fun run(): T
}

open class Timed<T>(label: String, private val ticks: Int) : Task<T>(label) {
    override fun run(): T = throw IllegalStateException("not run")

    fun elapsed(scale: Int): Int = ticks * scale
}

// A class private to its file calls members it inherits without overriding them: a final member
// of its generic superclass's superclass, and one of its superclass.
private class Blocking(private val text: String, ticks: Int) : Timed<Int>(text, ticks) {
    override fun run(): Int = text.length

    fun join(): String = start("go") + "/" + elapsed(3)
}

fun launch(text: String): String = Blocking(text, 2).join()

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val result = launch("abc")
    return if (result == "go:abc:3/6") "OK" else result
}
