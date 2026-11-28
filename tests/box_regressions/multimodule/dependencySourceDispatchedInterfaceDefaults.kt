// MODULE: library
// FILE: Library.kt
package fixture

interface Source {
    fun item(): String

    fun label(): String = "source"
}

// A library-private object and class implement an interface without overriding its default, and
// the library hands them out only as the interface. No library body calls the default: only the
// consumer's call dispatched through the interface reaches it.
private object Empty : Source {
    override fun item(): String = "none"
}

private class Single(private val value: String) : Source {
    override fun item(): String = value
}

fun emptySource(): Source = Empty

fun singleSource(value: String): Source = Single(value)

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val empty = emptySource()
    val single = singleSource("one")
    val result = empty.label() + "/" + empty.item() + "/" + single.label() + "/" + single.item()
    return if (result == "source/none/source/one") "OK" else result
}
