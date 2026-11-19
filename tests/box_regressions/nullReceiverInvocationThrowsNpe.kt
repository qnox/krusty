// Calling a member on a null value that a static type promised non-null throws the Kotlin
// NullPointerException the program can catch: a function value, a member of a library interface
// and a member of a source interface.
interface Named {
    fun name(): String
}

// A holder whose `get` hands back its null as a `T`: the unchecked cast is erased, so only the
// call on the result can fail.
class Holder<T>(private val value: Any?) {
    @Suppress("UNCHECKED_CAST")
    fun get(): T = value as T
}

fun threw(call: () -> Any?): Boolean =
    try {
        call()
        false
    } catch (e: NullPointerException) {
        true
    }

fun box(): String {
    val function = Holder<() -> String>(null)
    if (!threw { function.get()() }) return "function value"
    val sequence = Holder<CharSequence>(null)
    if (!threw { sequence.get().length }) return "library interface member"
    val named = Holder<Named>(null)
    if (!threw { named.get().name() }) return "source interface member"
    return "OK"
}
