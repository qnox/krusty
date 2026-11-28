// A dependency function's receiver lambda keeps its receiver when the call's name is also a local
// value that cannot be invoked (`flow { emit(...) }` in a function with a `flow` parameter). The
// local does not claim the call, so it must not hide the function's lambda shape either.
// MODULE: lib
// FILE: Lib.kt
package lib

class Collector<T> {
    val items = mutableListOf<T>()

    fun emit(value: T) {
        items += value
    }
}

fun <T> build(block: Collector<T>.() -> Unit): Collector<T> = Collector<T>().apply(block)

fun plain(block: StringBuilder.() -> Unit): String = StringBuilder().apply(block).toString()

// MODULE: main(lib)
// FILE: main.kt
import lib.Collector
import lib.build
import lib.plain

fun collect(build: Collector<String>): Collector<String> = build { emit("O") }

fun text(plain: Int): String = plain { for (i in 0 until plain) append("K") }

fun box(): String {
    val collected = collect(Collector())
    if (collected.items != listOf("O")) return "fail ${collected.items}"
    return collected.items.single() + text(1)
}
