// The expected type of a call constrains a member extension's result formal, as it does for any
// other generic call: `R` is fixed from `Box<String>` (or the caller's `Box<R>`) on the implicit,
// explicit and safe-call receiver forms, not completed to its bound.

class Box<T>(val value: T)

class Registry(val stored: Any?) {
    @Suppress("UNCHECKED_CAST")
    fun <R : CharSequence?> Registry.read(): Box<R> = Box(stored as R)

    fun implicitConcrete(): Box<String> = read()
    fun explicitConcrete(): Box<String> = this.read()
    fun <R : CharSequence> implicitCallerFormal(): Box<R> {
        return read()
    }
    fun safeConcrete(other: Registry?): Box<String>? = other?.read()
}

fun box(): String {
    val registry = Registry("abc")
    val lengths = registry.implicitConcrete().value.length +
        registry.explicitConcrete().value.length +
        registry.implicitCallerFormal<String>().value.length +
        (registry.safeConcrete(registry)?.value?.length ?: 0)
    return if (lengths == 12 && registry.safeConcrete(null) == null) "OK" else "lengths=$lengths"
}
