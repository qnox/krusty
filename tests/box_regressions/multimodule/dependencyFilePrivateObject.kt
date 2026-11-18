// MODULE: library
// FILE: library.kt
package privateobject

interface Source {
    fun read(): String
}

private object Hidden : Source {
    private var reads = 0
    override fun read(): String {
        reads++
        return if (same() === this) "O" else "same"
    }
    private fun same(): Hidden = Hidden
    fun count(): Int = reads
    override fun toString(): String = "Hidden"
}

fun source(): Source = Hidden
fun reads(): Int = Hidden.count()
fun identical(): Boolean = source() === source()

// MODULE: main(library)
// FILE: main.kt
import privateobject.identical
import privateobject.reads
import privateobject.source

fun box(): String {
    val first = source().read()
    if (first != "O") return first
    if (source().read() != "O") return "second"
    if (reads() != 2) return "reads"
    if (!identical()) return "identity"
    if (source().toString() != "Hidden") return "toString"
    return "OK"
}
