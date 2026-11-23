// A class one file of the module declares implements `KCallable`; another file reads `name` on a
// `KCallable` that may be it or a compiler-made reference. The reading file answers each from its
// own getter: the class's override through the slot the module numbers, the reference from its
// own table.
// FILE: Implementation.kt
import kotlin.reflect.KCallable

private var sample: String = ""

class NamedCallable<T>(
    private val source: KCallable<T>,
    override val name: String,
) : KCallable<T> by source

fun namedCallable(): KCallable<*> = NamedCallable(::sample, "runtimeName")

fun plainCallable(): KCallable<*> = ::sample

// FILE: Read.kt
import kotlin.reflect.KCallable

fun readName(callable: KCallable<*>): String = callable.name

fun box(): String {
    if (readName(namedCallable()) != "runtimeName") return "named"
    if (readName(plainCallable()) != "sample") return "reference"
    return "OK"
}
