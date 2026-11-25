// Two files of one module each lower a library body that installs a different file-private
// interface of the library. The installed members are numbered once for the module, so the two
// interfaces' members do not share a number, and each file's call dispatches to its own member.

// MODULE: library
// FILE: library.kt
package installed

private interface Named {
    fun name(): String
}

private interface Counted {
    fun count(step: Int): Int
}

private class Labelled(private val label: String) : Named {
    override fun name(): String = label
}

private class Counter(private val start: Int) : Counted {
    override fun count(step: Int): Int = start + step
}

fun labelled(label: String): Any = Labelled(label)

fun counter(start: Int): Any = Counter(start)

fun describe(value: Any): String = if (value is Named) value.name() else "other"

fun advance(value: Any, step: Int): Int = if (value is Counted) value.count(step) else -1

// MODULE: main(library)
// FILE: counting.kt
import installed.advance
import installed.counter

fun counted(): Int = advance(counter(40), 2) + advance("no", 1)

// FILE: main.kt
import installed.describe
import installed.labelled

fun box(): String {
    if (describe(labelled("O")) != "O") return "named: ${describe(labelled("O"))}"
    if (describe(3) != "other") return "int"
    if (counted() != 41) return "counted: ${counted()}"
    return "OK"
}
