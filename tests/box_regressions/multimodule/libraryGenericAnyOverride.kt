// MODULE: library
// FILE: library.kt
package genericany

interface View<T> {
    fun get(): T
}

private class Hidden<T>(private val value: T) : View<T> {
    override fun get(): T = value
    override fun toString(): String = "value=$value"
}

fun make(): View<String> = Hidden("OK")

// MODULE: main(library)
// FILE: main.kt
import genericany.make

fun box(): String {
    val view = make()
    if (view.get() != "OK") return "get"
    val any: Any = view
    if (any.toString() != "value=OK") return "toString"
    return "OK"
}
