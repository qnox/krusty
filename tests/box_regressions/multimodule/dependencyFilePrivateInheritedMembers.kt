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

interface Named {
    fun name(): String = "named"
}

// A class private to its file calls members it inherits without overriding them: a final member
// of its generic superclass's superclass, one of its superclass, and an interface default.
private class Blocking(private val text: String, ticks: Int) : Timed<Int>(text, ticks), Named {
    override fun run(): Int = text.length

    fun join(): String = start("go") + "/" + elapsed(3) + "/" + name() + "/" + nameOf(this)
}

private fun nameOf(named: Named): String = named.name()

fun launch(text: String): String = Blocking(text, 2).join()

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val result = launch("abc")
    return if (result == "go:abc:3/6/named/named") "OK" else result
}
