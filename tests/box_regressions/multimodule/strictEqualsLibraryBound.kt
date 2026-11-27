// KOTLINC_VERSION: [2.5.0,)
// kotlinc 2.4 rejects @EqualityBound.
// LANGUAGE: +StrictEquals
// MODULE: lib
// FILE: Library.kt
package fixture

open class Named(val key: String) {
    override fun equals(@EqualityBound(Named::class) other: Any?): Boolean = key == other.key
    override fun hashCode(): Int = key.hashCode()
}

open class Keyed(val id: Int) {
    override fun equals(
        @EqualityBound(Keyed::class) other: Any?
    ): Boolean {
        return id == other.id
    }

    override fun hashCode(): Int = id
}

// MODULE: main(lib)
// FILE: Main.kt
import fixture.*

class Tagged(key: String, val tag: Int) : Named(key) {
    override fun equals(other: Any?): Boolean = key == other.key && (other !is Tagged || tag == other.tag)
}

fun sameKey(keyed: Named, other: Any?): Boolean = keyed == other

fun box(): String {
    if (Named("a") != Named("a")) return "Named"
    if (Tagged("a", 1) != Named("a")) return "Tagged against Named"
    if (Tagged("a", 1) == Tagged("a", 2)) return "different tags"
    if (!sameKey(Tagged("b", 1), Named("b"))) return "sameKey"
    if (sameKey(Named("b"), "b")) return "a String reached Named.equals"
    if (Keyed(1) != Keyed(1)) return "Keyed"
    if (Keyed(1) == Keyed(2)) return "different ids"
    return "OK"
}
