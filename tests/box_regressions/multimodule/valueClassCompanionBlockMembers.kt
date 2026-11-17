// LANGUAGE: +CompanionBlocks
// KOTLINC_VERSION: [2.5.0,)
// WITH_STDLIB
// A `companion { … }` member is a member of its declaring class for value-class mangling: a value
// class names it like a member's static implementation (`getSeed-impl`, `make-<hash>`), so its
// `value` property does not clash with the instance property it shares a name with, and any class
// mangles a value-class result (`make-<hash>`, `getWrapped-<hash>`). Callers in the declaring
// file, a sibling file and a dependent module reach the same names.

// MODULE: lib
// FILE: Wrapper.kt
package fixture

@JvmInline
value class Wrapper(val value: Int) {
    fun twice(): Int = value * 2

    companion {
        val value get() = 1
        val seed: Int get() = 5
        fun make(n: Int): Wrapper = Wrapper(n + seed)
        fun describe(w: Wrapper): String = "w" + w.value
    }
}

class Holder {
    companion {
        fun make(n: Int): Wrapper = Wrapper(n * 3)
        val wrapped: Wrapper get() = Wrapper(4)
    }
}

fun sameFile(): Int = Wrapper.make(1).value + Wrapper.value + Holder.make(1).value + Holder.wrapped.value

// FILE: Sibling.kt
package fixture

fun sibling(): String =
    Wrapper.describe(Wrapper.make(Wrapper.seed)) + Wrapper.describe(Holder.make(2)) + Holder.wrapped.twice()

// MODULE: main(lib)
// FILE: main.kt
import fixture.*

fun box(): String {
    if (sameFile() != 14) return "Fail same file: ${sameFile()}"
    if (sibling() != "w10w68") return "Fail sibling: ${sibling()}"
    if (Holder.make(5).value != 15) return "Fail holder make"
    if (Holder.wrapped.twice() != 8) return "Fail holder wrapped"
    if (Wrapper.value != 1) return "Fail companion value"
    if (Wrapper(42).value != 42) return "Fail instance value"
    if (Wrapper.make(2).twice() != 14) return "Fail make"
    if (Wrapper.describe(Wrapper(3)) != "w3") return "Fail describe"
    return "OK"
}
